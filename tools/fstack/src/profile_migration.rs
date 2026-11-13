use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const STATE: &str = "fstack/profile.json";
const JOURNAL: &str = "fstack/profile.lock";
const CLAIMED: &str = "moved";
const AGENTS: &str = "AGENTS.md";
const START: &str = "<!-- fstack:profile:start -->";
const END: &str = "<!-- fstack:profile:end -->";
const LEGACY_ROLES: [&str; 6] = [
    "agents/allure_analyst.toml",
    "agents/auditor.toml",
    "agents/coder.toml",
    "agents/python_coder.toml",
    "agents/reviewer.toml",
    "agents/verifier.toml",
];

type Result<T> = std::result::Result<T, String>;

#[derive(PartialEq, Eq)]
enum FileState {
    Missing,
    Unsafe,
    Content(Vec<u8>),
}

struct Change {
    before: Vec<u8>,
    after: Option<Vec<u8>>,
}

#[derive(Default)]
struct Plan {
    changes: BTreeMap<&'static str, Change>,
    preserved: Vec<&'static str>,
}

fn digest(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn valid_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn snapshot(home: &Path, name: &str) -> Result<FileState> {
    let mut path = home.to_path_buf();
    let mut components = Path::new(name).components().peekable();
    while let Some(component) = components.next() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if !is_link(&metadata)
                    && if components.peek().is_some() {
                        metadata.is_dir()
                    } else {
                        metadata.is_file()
                    } => {}
            Ok(_) => return Ok(FileState::Unsafe),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(FileState::Missing),
            Err(error) => return Err(format!("{name}: {error}")),
        }
    }
    let mut file = match open_options().read(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(FileState::Missing),
        Err(error) => return Err(format!("{name}: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("{name}: {error}"))?;
    if is_link(&metadata) || !metadata.is_file() {
        return Ok(FileState::Unsafe);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("{name}: {error}"))?;
    Ok(FileState::Content(bytes))
}

fn remove_owned_block(bytes: &[u8], hash: &str) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut starts = text.match_indices(START);
    let mut ends = text.match_indices(END);
    let (start, _) = starts.next()?;
    let (end, _) = ends.next()?;
    if start >= end || starts.next().is_some() || ends.next().is_some() {
        return None;
    }
    let end = end + END.len();
    if digest(&bytes[start..end]) != hash {
        return None;
    }
    Some([&bytes[..start], &bytes[end..]].concat())
}

fn plan(home: &Path) -> Result<Plan> {
    let mut plan = Plan::default();
    let state = match snapshot(home, STATE)? {
        FileState::Missing => return Ok(plan),
        FileState::Unsafe => return Err(format!("Ожидался обычный файл {STATE}")),
        FileState::Content(state) => state,
    };
    let data: Value =
        serde_json::from_slice(&state).map_err(|error| format!("{STATE}: {error}"))?;
    let schema = data["schema"].as_u64();
    let files = data["files"]
        .as_object()
        .filter(|files| {
            !files.is_empty()
                && files
                    .iter()
                    .all(|(name, hash)| LEGACY_ROLES.contains(&name.as_str()) && valid_hash(hash))
        })
        .ok_or_else(|| format!("Некорректный реестр ролей {STATE}"))?;
    let block_hash = match schema {
        Some(1) if valid_hash(&data["links_sha256"]) => data["links_sha256"].as_str(),
        Some(2) if data.get("links_sha256").is_none() => None,
        _ => return Err(format!("Неподдерживаемая схема {STATE}")),
    };

    for name in LEGACY_ROLES {
        let Some(hash) = files.get(name).and_then(Value::as_str) else {
            continue;
        };
        match snapshot(home, name)? {
            FileState::Content(before) if digest(&before) == hash => {
                plan.changes.insert(
                    name,
                    Change {
                        before,
                        after: None,
                    },
                );
            }
            FileState::Missing => {}
            _ => plan.preserved.push(name),
        }
    }
    if let Some(hash) = block_hash {
        match snapshot(home, AGENTS)? {
            FileState::Content(before) => {
                if let Some(after) = remove_owned_block(&before, hash) {
                    plan.changes.insert(
                        AGENTS,
                        Change {
                            before,
                            after: Some(after),
                        },
                    );
                } else {
                    plan.preserved.push(AGENTS);
                }
            }
            FileState::Unsafe => plan.preserved.push(AGENTS),
            FileState::Missing => {}
        }
    }
    plan.changes.insert(
        STATE,
        Change {
            before: state,
            after: None,
        },
    );
    Ok(plan)
}

fn atomic_write(
    path: &Path,
    bytes: &[u8],
    replace: bool,
    permissions: Option<fs::Permissions>,
) -> Result<()> {
    let parent = path.parent().ok_or("Нет родительского каталога")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    if let Some(permissions) = permissions {
        file.as_file()
            .set_permissions(permissions)
            .map_err(|e| e.to_string())?;
    }
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    if replace {
        file.persist(path)
    } else {
        file.persist_noclobber(path)
    }
    .map_err(|e| e.to_string())?;
    Ok(())
}
#[cfg(windows)]
fn system_powershell() -> Result<PathBuf> {
    let root = PathBuf::from(std::env::var_os("SystemRoot").ok_or("Не задан SystemRoot")?);
    if !root.is_absolute() {
        return Err("SystemRoot должен быть абсолютным путём".into());
    }
    let path = root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !path.is_file() {
        return Err("Не найден системный Windows PowerShell".into());
    }
    Ok(path)
}
fn private_backup(path: &Path) -> Result<()> {
    fs::create_dir_all(path.parent().ok_or("Нет каталога резервных копий")?)
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let script = "$ErrorActionPreference = 'Stop'; $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $acl = [System.Security.AccessControl.DirectorySecurity]::new(); $acl.SetSecurityDescriptorSddlForm('D:P(A;OICI;FA;;;' + $sid + ')(A;OICI;FA;;;SY)', [System.Security.AccessControl.AccessControlSections]::Access); [System.IO.Directory]::SetAccessControl($env:FSTACK_BACKUP_PATH, $acl)";
        let result = std::process::Command::new(system_powershell()?)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("FSTACK_BACKUP_PATH", path)
            .output()
            .map_err(|e| e.to_string())?;
        if !result.status.success() {
            return Err("Не удалось защитить резервную копию".into());
        }
    }
    Ok(())
}

fn lock(home: &Path) -> Result<File> {
    let relative = "fstack/profile-migration.lock";
    if snapshot(home, relative)? == FileState::Unsafe {
        return Err(format!("Ожидался обычный файл {relative}"));
    }
    let file = open_options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.join(relative))
        .map_err(|error| format!("{relative}: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("{relative}: {error}"))?;
    if is_link(&metadata) || !metadata.is_file() {
        return Err(format!("Ожидался обычный файл {relative}"));
    }
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|error| format!("Не удалось заблокировать миграцию: {error}"))?;
    Ok(file)
}

fn check_snapshot(home: &Path, name: &str, expected: &[u8]) -> Result<()> {
    if matches!(snapshot(home, name)?, FileState::Content(bytes) if bytes == expected) {
        Ok(())
    } else {
        Err(format!("Файл изменился во время миграции: {name}"))
    }
}

fn restore_moved(source: &Path, target: &Path) -> Result<()> {
    let mut file = tempfile::TempPath::try_from_path(source).map_err(|error| error.to_string())?;
    file.disable_cleanup(true);
    file.persist_noclobber(target).map_err(|error| {
        format!(
            "Не удалось восстановить {} из {}: {error}",
            target.display(),
            source.display()
        )
    })
}

fn claim_location(backup: &Path, name: &str) -> PathBuf {
    let file = Path::new(name);
    let transaction = backup.file_name().unwrap().to_string_lossy();
    file.parent()
        .unwrap()
        .join(format!(".{transaction}"))
        .join(file.file_name().unwrap())
}

fn claimed_file(home: &Path, backup: &Path, name: &str) -> Result<(PathBuf, FileState)> {
    let old = format!("{CLAIMED}/{name}");
    let previous = snapshot(backup, &old)?;
    if previous != FileState::Missing {
        return Ok((backup.join(old), previous));
    }
    let relative = claim_location(backup, name);
    let state = snapshot(home, relative.to_str().unwrap())?;
    Ok((home.join(relative), state))
}

fn move_checked(source: &Path, target: &Path, expected: &[u8]) -> Result<()> {
    fs::rename(source, target).map_err(|error| format!("{}: {error}", source.display()))?;
    if let Err(error) = check_snapshot(
        target.parent().unwrap(),
        target.file_name().unwrap().to_str().unwrap(),
        expected,
    ) {
        restore_moved(target, source).map_err(|restore| format!("{error}\n{restore}"))?;
        return Err(format!("{}: {error}", source.display()));
    }
    Ok(())
}

fn report(plan: &Plan, status: &str) -> Value {
    migration_report(plan, status, &[])
}

fn migration_report(plan: &Plan, status: &str, keep: &[&str]) -> Value {
    json!({
        "status": status,
        "changes": plan.changes.keys().filter(|name| !keep.contains(name)).collect::<Vec<_>>(),
        "removed": plan.changes.iter().filter(|(name, change)| change.after.is_none() && !keep.contains(name)).map(|(name, _)| name).collect::<Vec<_>>(),
        "preserved": plan.preserved.iter().chain(keep).collect::<Vec<_>>(),
    })
}

fn create_backup(home: &Path, plan: &Plan) -> Result<PathBuf> {
    let backup_dir = home.join("fstack").join("backups");
    match fs::symlink_metadata(&backup_dir) {
        Ok(metadata) if !is_link(&metadata) && metadata.is_dir() => {}
        Ok(_) => return Err("Ожидался обычный каталог fstack/backups".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let name = format!("profile-migration-{stamp}-{}", std::process::id());
    let backup = backup_dir.join(&name);
    if backup.to_str().is_none() {
        return Err("Путь резервной копии должен быть в UTF-8".into());
    }
    private_backup(&backup)?;
    for (name, change) in &plan.changes {
        atomic_write(&backup.join(name), &change.before, false, None)?;
    }
    let mut receipt = report(plan, "backup");
    receipt["files"] = json!(plan
        .changes
        .iter()
        .map(|(name, change)| (*name, digest(&change.before)))
        .collect::<BTreeMap<_, _>>());
    let manifest = serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?;
    atomic_write(&backup.join("manifest.json"), &manifest, false, None)?;

    atomic_write(&home.join(JOURNAL), name.as_bytes(), false, None)?;
    Ok(backup)
}

fn legacy_owner_running(pid: u32) -> Result<bool> {
    #[cfg(unix)]
    {
        let pid = i32::try_from(pid).map_err(|_| "Некорректный PID прежнего помощника")?;
        if unsafe { libc::kill(pid, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            Some(libc::EPERM) => Ok(true),
            _ => Err(error.to_string()),
        }
    }
    #[cfg(windows)]
    {
        let script = "$ErrorActionPreference='Stop'; try { $p=[Diagnostics.Process]::GetProcessById([int]$env:FSTACK_LEGACY_PID); $p.Dispose() } catch { if ($_.Exception.GetBaseException() -is [ArgumentException]) { exit 3 }; throw }";
        let status = std::process::Command::new(system_powershell()?)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("FSTACK_LEGACY_PID", pid.to_string())
            .status()
            .map_err(|error| error.to_string())?;
        match status.code() {
            Some(0) => Ok(true),
            Some(3) => Ok(false),
            _ => Err("Не удалось проверить PID прежнего помощника".into()),
        }
    }
}

fn pending_backup(home: &Path, recover_stale: bool) -> Result<Option<PathBuf>> {
    let bytes = match snapshot(home, JOURNAL)? {
        FileState::Missing => return Ok(None),
        FileState::Content(bytes) => bytes,
        FileState::Unsafe => return Err(format!("Ожидался обычный файл {JOURNAL}")),
    };
    if bytes.iter().all(u8::is_ascii_digit) {
        let pid = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&id| id > 0);
        let Some(pid) = pid else {
            return Err(format!("У {JOURNAL} нет действительного PID. Остановите прежний помощник и переместите файл блокировки для ручного восстановления."));
        };
        if legacy_owner_running(pid)? {
            return Err(format!("Прежний помощник удерживает {JOURNAL} (PID {pid}). Если этот PID уже принадлежит другому процессу, остановите прежний помощник и переместите оставшийся файл блокировки."));
        }
        if recover_stale {
            check_snapshot(home, JOURNAL, &bytes)?;
            fs::remove_file(home.join(JOURNAL)).map_err(|error| error.to_string())?;
        }
        return Ok(None);
    }
    let name = std::str::from_utf8(&bytes)
        .ok()
        .filter(|name| {
            name.strip_prefix("profile-migration-")
                .and_then(|suffix| suffix.split_once('-'))
                .is_some_and(|(stamp, pid)| {
                    stamp.parse::<u128>().is_ok() && pid.parse::<u32>().is_ok()
                })
        })
        .ok_or_else(|| format!("Некорректный журнал {JOURNAL}"))?;
    let directory = format!("fstack/backups/{name}");
    if !matches!(
        snapshot(home, &format!("{directory}/manifest.json"))?,
        FileState::Content(_)
    ) {
        return Err(format!("Недоступна резервная копия {directory}"));
    }
    Ok(Some(home.join("fstack").join("backups").join(name)))
}

fn backed_up_plan(backup: &Path) -> Result<Plan> {
    let mut original = plan(backup)?;
    let receipt: Value = serde_json::from_slice(
        &fs::read(backup.join("manifest.json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let files = receipt["files"]
        .as_object()
        .ok_or("Нет списка файлов резервной копии")?;
    if !original.changes.contains_key(STATE)
        || files.len() != original.changes.len()
        || original
            .changes
            .iter()
            .any(|(name, change)| files.get(*name) != Some(&json!(digest(&change.before))))
    {
        return Err(format!("Резервная копия повреждена: {}", backup.display()));
    }
    original.preserved = LEGACY_ROLES
        .into_iter()
        .chain([AGENTS])
        .filter(|name| {
            receipt["preserved"]
                .as_array()
                .is_some_and(|list| list.contains(&json!(name)))
        })
        .collect();
    Ok(original)
}

enum NextChange {
    Capture,
    Install,
    Complete,
    Preserve,
    RestoreEdit,
}

fn next_change(
    name: &str,
    change: &Change,
    saved: &FileState,
    live: &FileState,
) -> Result<NextChange> {
    let action = match (saved, live) {
        (FileState::Missing, FileState::Content(bytes)) if *bytes == change.before => {
            NextChange::Capture
        }
        (FileState::Missing, FileState::Missing) if name != STATE => NextChange::Complete,
        (FileState::Content(bytes), live) if *bytes == change.before => match live {
            FileState::Missing if change.after.is_some() => NextChange::Install,
            FileState::Missing => NextChange::Complete,
            FileState::Content(bytes) if Some(bytes) == change.after.as_ref() => {
                NextChange::Complete
            }
            _ => NextChange::Preserve,
        },
        (FileState::Content(_), FileState::Missing) => NextChange::RestoreEdit,
        _ => NextChange::Preserve,
    };
    if name == STATE && matches!(action, NextChange::Preserve | NextChange::RestoreEdit) {
        return Err(format!("Файл изменился во время миграции: {name}"));
    }
    Ok(action)
}

fn apply_change(home: &Path, name: &str, backup: &Path, change: &Change) -> Result<bool> {
    let (claimed, mut saved) = claimed_file(home, backup, name)?;
    let source = home.join(name);
    let mut action = next_change(name, change, &saved, &snapshot(home, name)?)?;
    if matches!(action, NextChange::Capture) {
        private_claim_directory(&source, backup)?;
        move_checked(&source, &claimed, &change.before)?;
        saved = snapshot(
            claimed.parent().unwrap(),
            claimed.file_name().unwrap().to_str().unwrap(),
        )?;
        action = next_change(name, change, &saved, &snapshot(home, name)?)?;
    }
    match action {
        NextChange::Capture => Err(format!("Исходный файл исчез при переносе: {name}")),
        NextChange::RestoreEdit => {
            restore_moved(&claimed, &source)?;
            Ok(false)
        }
        NextChange::Preserve => Ok(false),
        NextChange::Complete => Ok(true),
        NextChange::Install => {
            let permissions = fs::metadata(&claimed)
                .map_err(|e| e.to_string())?
                .permissions();
            atomic_write(
                &source,
                change.after.as_ref().unwrap(),
                false,
                Some(permissions),
            )?;
            Ok(true)
        }
    }
}

fn rollback(home: &Path, name: &str, backup: &Path, change: &Change) -> Result<()> {
    let (claimed, saved) = claimed_file(home, backup, name)?;
    if saved == FileState::Missing {
        return Ok(());
    }
    let source = home.join(name);
    if let Some(after) = &change.after {
        if snapshot(home, name)? != FileState::Missing {
            check_snapshot(home, name, after)?;
            let directory = tempfile::Builder::new()
                .prefix("rollback-")
                .tempdir_in(private_claim_directory(&source, backup)?)
                .map_err(|error| error.to_string())?
                .keep();
            let replacement = directory.join("replacement");
            move_checked(&source, &replacement, after)?;
            restore_moved(&claimed, &source)?;
            fs::remove_file(&replacement)
                .and_then(|_| fs::remove_dir(&directory))
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
    }
    restore_moved(&claimed, &source)
}

fn private_claim_directory(source: &Path, backup: &Path) -> Result<PathBuf> {
    let directory = source.parent().unwrap().join(format!(
        ".{}",
        backup.file_name().unwrap().to_string_lossy()
    ));
    if !directory.exists() {
        private_backup(&directory)?;
    }
    Ok(directory)
}

fn preview(home: &Path) -> Result<Value> {
    let Some(backup) = pending_backup(home, false)? else {
        let current = plan(home)?;
        return Ok(report(
            &current,
            if current.changes.is_empty() {
                "unchanged"
            } else {
                "planned"
            },
        ));
    };
    if let Some(mut result) = committed_report(&backup)? {
        result["completed"] = result["changes"].take();
        result["changes"] = json!([]);
        result["removed"] = json!([]);
        result["status"] = json!("planned");
        return Ok(result);
    }
    let mut original = backed_up_plan(&backup)?;
    let mut completed = Vec::new();
    for name in original.changes.keys().copied().collect::<Vec<_>>() {
        let change = &original.changes[name];
        let (_, saved) = claimed_file(home, &backup, name)?;
        let live = snapshot(home, name)?;
        match next_change(name, change, &saved, &live)? {
            NextChange::Complete => completed.push(name),
            NextChange::RestoreEdit | NextChange::Preserve => original.preserved.push(name),
            NextChange::Capture | NextChange::Install => continue,
        }
        original.changes.remove(name);
    }
    let mut result = report(&original, "planned");
    result["completed"] = json!(completed);
    result["backup"] = json!(backup);
    Ok(result)
}

fn finish_claims(home: &Path, backup: &Path, plan: &Plan) -> Result<Vec<PathBuf>> {
    let mut retained = Vec::new();
    for (name, change) in &plan.changes {
        let relative = claim_location(backup, name);
        let file = home.join(&relative);
        match snapshot(home, relative.to_str().unwrap())? {
            FileState::Missing => {}
            FileState::Content(bytes) if bytes == change.before => {
                fs::remove_file(&file).map_err(|error| error.to_string())?;
            }
            _ => {
                retained.push(file);
                continue;
            }
        }
        match fs::remove_dir(file.parent().unwrap()) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(retained)
}

fn migrate_with(
    home: &Path,
    apply: &impl Fn(&Path, &str, &Path, &Change) -> Result<bool>,
) -> Result<Value> {
    if snapshot(home, JOURNAL)? == FileState::Missing
        && snapshot(home, STATE)? == FileState::Missing
    {
        return Ok(report(&Plan::default(), "unchanged"));
    }
    let _lock = lock(home)?;
    let (backup, plan) = match pending_backup(home, true)? {
        Some(backup) => {
            let original = backed_up_plan(&backup)?;
            if let Some(result) = committed_report(&backup)? {
                return finish_migration(home, &backup, &original, result);
            }
            (backup, original)
        }
        None => {
            let plan = plan(home)?;
            if plan.changes.is_empty() {
                return Ok(report(&plan, "unchanged"));
            }
            (create_backup(home, &plan)?, plan)
        }
    };
    let mut completed = Vec::new();
    let mut preserved = Vec::new();
    let ordered: Vec<_> = plan
        .changes
        .keys()
        .copied()
        .filter(|&name| name != STATE)
        .chain([STATE])
        .collect();
    for name in ordered {
        let change = &plan.changes[name];
        match apply(home, name, &backup, change) {
            Ok(true) => completed.push(name),
            Ok(false) => preserved.push(name),
            Err(error) => {
                let mut errors = vec![error];
                for previous in std::iter::once(name).chain(completed.into_iter().rev()) {
                    if let Err(error) = rollback(home, previous, &backup, &plan.changes[previous]) {
                        errors.push(error);
                    }
                }
                return Err(format!(
                    "{}\nРезервная копия: {}",
                    errors.join("\n"),
                    backup.display()
                ));
            }
        }
    }
    let mut result = migration_report(&plan, "applied", &preserved);
    result["backup"] = json!(backup);
    let bytes = serde_json::to_vec(&result).map_err(|error| error.to_string())?;
    atomic_write(&backup.join("applied.json"), &bytes, false, None)?;
    finish_migration(home, &backup, &plan, result)
}

fn committed_report(backup: &Path) -> Result<Option<Value>> {
    match snapshot(backup, "applied.json")? {
        FileState::Missing => Ok(None),
        FileState::Content(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if value["status"] != "applied" || value["backup"] != json!(backup) {
                return Err("Некорректная квитанция миграции".into());
            }
            Ok(Some(value))
        }
        FileState::Unsafe => Err("Некорректная квитанция миграции".into()),
    }
}

fn finish_migration(home: &Path, backup: &Path, plan: &Plan, mut result: Value) -> Result<Value> {
    let retained = finish_claims(home, backup, plan)?;
    if !retained.is_empty() {
        result["retained_originals"] = json!(retained);
    }
    fs::remove_file(home.join(JOURNAL)).map_err(|error| error.to_string())?;
    Ok(result)
}

fn codex_home(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let user_home = || {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .ok_or_else(|| "Не найден домашний каталог".to_owned())
    };
    let home = match explicit.or_else(|| {
        std::env::var_os("CODEX_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    }) {
        Some(home) => home,
        None => user_home()?.join(".codex"),
    };
    let home = if let Ok(relative) = home.strip_prefix("~") {
        user_home()?.join(relative)
    } else {
        home
    };
    let absolute = std::path::absolute(home).map_err(|error| error.to_string())?;
    match absolute.canonicalize() {
        Ok(home) => Ok(home),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(absolute),
        Err(error) => Err(error.to_string()),
    }
}

pub fn run(args: &[OsString]) -> Result<()> {
    let mut args = args.iter();
    let mut home = None;
    let mut apply = false;
    let mut hook = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--home") => home = Some(PathBuf::from(args.next().ok_or("--home требует путь")?)),
            Some("--apply") => apply = true,
            Some("--hook") => hook = true,
            _ => {
                return Err(
                    "Использование: fstack migrate-profile [--home PATH] [--apply | --hook]".into(),
                )
            }
        }
    }
    let home = codex_home(home)?;
    let result = if apply || hook {
        migrate_with(&home, &apply_change)?
    } else {
        preview(&home)?
    };
    if hook {
        if let Some(backup) = result["backup"].as_str() {
            let preserved = result["preserved"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let mut message =
                format!("Миграция профиля fstack завершена. Резервная копия: {backup}.");
            if !preserved.is_empty() {
                message.push_str(&format!(" Сохранены файлы: {}.", preserved.join(", ")));
            }
            if let Some(retained) = result["retained_originals"].as_array() {
                message.push_str(&format!(
                    " Сохранённые оригиналы: {}.",
                    retained
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            println!("{}", json!({"systemMessage": message}));
        } else {
            println!("{{}}");
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(home: &Path, name: &str, bytes: &[u8]) {
        atomic_write(&home.join(name), bytes, true, None).unwrap();
    }

    fn legacy(home: &Path, schema: u64, names: &[&str]) -> Vec<u8> {
        let mut files = BTreeMap::new();
        for &name in names {
            put(home, name, name.as_bytes());
            files.insert(name, digest(name.as_bytes()));
        }
        let mut state = json!({"schema": schema, "version": "8.7.0", "files": files});
        if schema == 1 {
            let block = format!("{START}\r\nПрежние инструкции\n{END}");
            put(
                home,
                AGENTS,
                format!("\u{feff}Личный текст\r\n{block}\nПродолжение\r\n").as_bytes(),
            );
            state["links_sha256"] = json!(digest(block.as_bytes()));
        }
        let bytes = serde_json::to_vec(&state).unwrap();
        put(home, STATE, &bytes);
        bytes
    }

    #[test]
    fn profiles_without_ownership_evidence_are_read_only() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("profile");
        assert_eq!(
            migrate_with(&home, &apply_change).unwrap()["status"],
            "unchanged"
        );
        assert!(!home.exists());

        let role = "agents/coder.toml";
        put(&home, role, b"local role");
        put(&home, AGENTS, b"personal instructions");
        put(&home, "config.toml", b"local settings");
        assert_eq!(
            migrate_with(&home, &apply_change).unwrap()["status"],
            "unchanged"
        );
        assert_eq!(fs::read(home.join(role)).unwrap(), b"local role");
        assert_eq!(
            fs::read(home.join(AGENTS)).unwrap(),
            b"personal instructions"
        );
        assert_eq!(
            fs::read(home.join("config.toml")).unwrap(),
            b"local settings"
        );
        assert!(!home.join("fstack").exists());
    }

    #[test]
    fn migration_uses_each_hash_and_retires_every_supported_legacy_role() {
        let temporary = tempfile::tempdir().unwrap();
        for (index, schema, names) in [
            (0, 1, &LEGACY_ROLES[..5]),
            (
                1,
                2,
                &[
                    "agents/allure_analyst.toml",
                    "agents/auditor.toml",
                    "agents/coder.toml",
                    "agents/reviewer.toml",
                    "agents/verifier.toml",
                ][..],
            ),
            (
                2,
                2,
                &[
                    "agents/auditor.toml",
                    "agents/coder.toml",
                    "agents/verifier.toml",
                ][..],
            ),
        ] {
            let home = temporary.path().join(index.to_string());
            let state = legacy(&home, schema, names);
            let user_role = "agents/coder.toml";
            put(&home, user_role, b"role with a user edit\r\n");
            put(&home, "agents/custom.toml", b"unrelated role");
            put(&home, "config.toml", b"user preferences\r\n");
            put(&home, "AGENTS.override.md", b"user override");
            let missing = "agents/auditor.toml";
            fs::remove_file(home.join(missing)).unwrap();

            let planned = plan(&home).unwrap();
            assert_eq!(planned.preserved, [user_role]);
            assert_eq!(fs::read(home.join(STATE)).unwrap(), state);

            let result = migrate_with(&home, &apply_change).unwrap();
            let backup = Path::new(result["backup"].as_str().unwrap());
            assert_eq!(result["status"], "applied");
            assert_eq!(result["preserved"], json!([user_role]));
            for &name in names {
                if name == user_role || name == missing {
                    assert!(!backup.join(name).exists());
                } else {
                    assert!(!home.join(name).exists(), "{name}");
                    assert_eq!(fs::read(backup.join(name)).unwrap(), name.as_bytes());
                }
            }
            assert_eq!(fs::read(backup.join(STATE)).unwrap(), state);
            assert!(!home.join(STATE).exists());
            if schema == 1 {
                assert_eq!(
                    fs::read(home.join(AGENTS)).unwrap(),
                    "\u{feff}Личный текст\r\n\nПродолжение\r\n".as_bytes()
                );
                assert!(backup.join(AGENTS).is_file());
            } else {
                assert!(!home.join(AGENTS).exists());
            }
            assert_eq!(
                fs::read(home.join(user_role)).unwrap(),
                b"role with a user edit\r\n"
            );
            assert_eq!(
                fs::read(home.join("agents/custom.toml")).unwrap(),
                b"unrelated role"
            );
            assert_eq!(
                fs::read(home.join("config.toml")).unwrap(),
                b"user preferences\r\n"
            );
            assert_eq!(
                fs::read(home.join("AGENTS.override.md")).unwrap(),
                b"user override"
            );
            assert_eq!(
                migrate_with(&home, &apply_change).unwrap()["status"],
                "unchanged"
            );
            assert_eq!(fs::read_dir(backup.parent().unwrap()).unwrap().count(), 1);

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    backup.metadata().unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(
                    backup.join(STATE).metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            #[cfg(windows)]
            {
                let output = std::process::Command::new(system_powershell().unwrap())
                    .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $acl=Get-Acl -LiteralPath $env:FSTACK_TEST_BACKUP; @{Protected=$acl.AreAccessRulesProtected; Owner=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; Rules=@($acl.Access | ForEach-Object { $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value })} | ConvertTo-Json -Compress"])
                    .env_remove("PSModulePath")
                    .env("FSTACK_TEST_BACKUP", backup)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let acl: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(acl["Protected"], true);
                let rules = acl["Rules"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|rule| rule.as_str().unwrap())
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    rules,
                    [acl["Owner"].as_str().unwrap(), "S-1-5-18"]
                        .into_iter()
                        .collect()
                );
            }
        }
    }

    #[test]
    fn modified_block_is_preserved_and_invalid_state_prevents_deletion() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();
        let name = "agents/auditor.toml";
        legacy(home, 1, &[name]);
        let edited = format!("{START}\nЛичное изменение\n{END}");
        put(home, AGENTS, edited.as_bytes());
        assert_eq!(
            migrate_with(home, &apply_change).unwrap()["preserved"],
            json!([AGENTS])
        );
        assert_eq!(fs::read(home.join(AGENTS)).unwrap(), edited.as_bytes());
        assert!(!home.join(name).exists());

        let state = legacy(home, 2, &[name]);
        let mut tampered: Value = serde_json::from_slice(&state).unwrap();
        tampered["files"]["config.toml"] = json!(digest(b"personal preferences"));
        put(home, STATE, &serde_json::to_vec(&tampered).unwrap());
        put(home, "config.toml", b"personal preferences");
        assert!(migrate_with(home, &apply_change).is_err());
        assert_eq!(fs::read(home.join(name)).unwrap(), name.as_bytes());
        assert_eq!(
            fs::read(home.join("config.toml")).unwrap(),
            b"personal preferences"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_roles_and_directories_are_preserved() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("profile");
        let name = "agents/coder.toml";
        legacy(&home, 2, &[name]);
        let outside = temporary.path().join("outside.toml");
        fs::write(&outside, name).unwrap();
        fs::remove_file(home.join(name)).unwrap();
        symlink(&outside, home.join(name)).unwrap();
        let result = migrate_with(&home, &apply_change).unwrap();
        assert_eq!(result["preserved"], json!([name]));
        assert_eq!(fs::read_link(home.join(name)).unwrap(), outside);
        assert_eq!(fs::read(&outside).unwrap(), name.as_bytes());

        fs::remove_file(home.join(name)).unwrap();
        legacy(&home, 2, &[name]);
        let external_agents = temporary.path().join("external-agents");
        fs::rename(home.join("agents"), &external_agents).unwrap();
        symlink(&external_agents, home.join("agents")).unwrap();
        assert_eq!(
            migrate_with(&home, &apply_change).unwrap()["preserved"],
            json!([name])
        );
        assert_eq!(
            fs::read(external_agents.join("coder.toml")).unwrap(),
            name.as_bytes()
        );
    }

    #[test]
    fn interrupted_migration_restores_removed_roles_without_overwriting_a_new_file() {
        let temporary = tempfile::tempdir().unwrap();
        for concurrent in [false, true] {
            let home = temporary.path().join(concurrent.to_string());
            let auditor = "agents/auditor.toml";
            let coder = "agents/coder.toml";
            let state = legacy(&home, 2, &[auditor, coder]);
            let fail_on_state = |home: &Path, name: &str, backup: &Path, change: &Change| {
                if name == STATE {
                    assert!(!home.join(auditor).exists());
                    if concurrent {
                        put(home, auditor, b"new local role");
                    }
                    return Err("test write failure".into());
                }
                apply_change(home, name, backup, change)
            };
            let error = migrate_with(&home, &fail_on_state).unwrap_err();
            assert!(error.contains("test write failure"));
            assert_eq!(fs::read(home.join(STATE)).unwrap(), state);
            assert_eq!(
                fs::read(home.join(auditor)).unwrap(),
                if concurrent {
                    b"new local role"
                } else {
                    auditor.as_bytes()
                },
            );
            assert_eq!(fs::read(home.join(coder)).unwrap(), coder.as_bytes());
            let result = migrate_with(&home, &apply_change).unwrap();
            assert_eq!(result["status"], "applied");
            assert_eq!(home.join(auditor).exists(), concurrent);
        }
    }

    #[test]
    fn legacy_and_current_helpers_share_the_exclusive_profile_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();
        let role = "agents/coder.toml";
        legacy(home, 2, &[role]);
        put(home, JOURNAL, std::process::id().to_string().as_bytes());
        assert!(migrate_with(home, &apply_change)
            .unwrap_err()
            .contains("Прежний помощник"));
        assert_eq!(fs::read(home.join(role)).unwrap(), role.as_bytes());
        fs::remove_file(home.join(JOURNAL)).unwrap();
        let backup = create_backup(home, &plan(home).unwrap()).unwrap();
        let legacy_lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(home.join("fstack/profile.lock"));
        assert_eq!(
            legacy_lock.unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            migrate_with(home, &apply_change).unwrap()["backup"],
            json!(backup)
        );
        assert!(!home.join(JOURNAL).exists());
    }

    #[cfg(unix)]
    #[test]
    fn working_copy_keeps_permissions_and_claims_stay_on_the_source_volume() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let detached = tempfile::tempdir().unwrap();
        let home = home.path();
        let coder = "agents/coder.toml";
        legacy(home, 1, &[coder]);
        fs::set_permissions(home.join(AGENTS), fs::Permissions::from_mode(0o640)).unwrap();
        let changes = plan(home).unwrap();
        let original = create_backup(home, &changes).unwrap();
        let backup = detached.path().join(original.file_name().unwrap());
        fs::rename(original, &backup).unwrap();
        for name in [AGENTS, coder] {
            assert!(apply_change(home, name, &backup, &changes.changes[name]).unwrap());
            let local = home.join(claim_location(&backup, name));
            assert_eq!(local.parent().unwrap().parent(), home.join(name).parent());
            assert!(local.is_file());
            assert!(!backup.join(CLAIMED).join(name).exists());
            rollback(home, name, &backup, &changes.changes[name]).unwrap();
            assert_eq!(
                fs::read(home.join(name)).unwrap(),
                changes.changes[name].before
            );
        }
        apply_change(home, AGENTS, &backup, &changes.changes[AGENTS]).unwrap();
        assert_eq!(
            home.join(AGENTS).metadata().unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            backup.join(AGENTS).metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn preview_and_apply_recover_a_departed_legacy_owner() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();
        let role = "agents/coder.toml";
        legacy(home, 2, &[role]);
        put(home, JOURNAL, b"2147483647");
        assert_eq!(preview(home).unwrap()["status"], "planned");
        assert_eq!(fs::read(home.join(JOURNAL)).unwrap(), b"2147483647");
        let result = migrate_with(home, &apply_change).unwrap();
        assert_eq!(result["status"], "applied");
        assert!(!home.join(role).exists());
        assert!(!home.join(JOURNAL).exists());
    }

    #[test]
    fn preview_lists_live_edits_and_keeps_committed_retries_cleanup_only() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();
        let role = "agents/coder.toml";
        legacy(home, 1, &[role]);
        let original = plan(home).unwrap();
        let backup = create_backup(home, &original).unwrap();
        apply_change(home, AGENTS, &backup, &original.changes[AGENTS]).unwrap();
        apply_change(home, role, &backup, &original.changes[role]).unwrap();
        put(home, AGENTS, b"latest user edition");
        let dry_run = preview(home).unwrap();
        assert_eq!(dry_run["changes"], json!([STATE]));
        assert_eq!(dry_run["preserved"], json!([AGENTS]));
        assert_eq!(dry_run["completed"], json!([role]));
        let result = migrate_with(home, &apply_change).unwrap();
        assert_eq!(result["preserved"], dry_run["preserved"]);
        assert_eq!(fs::read(home.join(AGENTS)).unwrap(), b"latest user edition");
        put(
            home,
            JOURNAL,
            backup.file_name().unwrap().to_str().unwrap().as_bytes(),
        );
        put(home, role, b"new role after completion");
        assert_eq!(preview(home).unwrap()["changes"], json!([]));
        assert_eq!(migrate_with(home, &apply_change).unwrap(), result);
        assert_eq!(
            fs::read(home.join(role)).unwrap(),
            b"new role after completion"
        );
    }

    #[test]
    fn moving_the_actual_file_preserves_an_edit_after_the_ownership_check() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path();
        let role = "agents/coder.toml";
        legacy(home, 1, &[role]);
        let backup = create_backup(home, &plan(home).unwrap()).unwrap();
        for name in [role, AGENTS, STATE] {
            let expected = fs::read(home.join(name)).unwrap();
            check_snapshot(home, name, &expected).unwrap();
            put(home, name, b"user save after the check");
            let moved = backup.join(CLAIMED).join(name);
            fs::create_dir_all(moved.parent().unwrap()).unwrap();
            let error = move_checked(&home.join(name), &moved, &expected).unwrap_err();
            assert!(error.contains(name));
            assert_eq!(
                fs::read(home.join(name)).unwrap(),
                b"user save after the check"
            );
            assert_eq!(fs::read(backup.join(name)).unwrap(), expected);
        }
    }

    #[test]
    fn retry_keeps_both_user_edits_when_the_claimed_file_and_live_path_changed() {
        let temporary = tempfile::tempdir().unwrap();
        for name in ["agents/coder.toml", AGENTS] {
            let home = temporary.path().join(name.replace('/', "_"));
            let auditor = "agents/auditor.toml";
            legacy(&home, 1, &["agents/coder.toml", auditor]);
            let original = plan(&home).unwrap();
            let backup = create_backup(&home, &original).unwrap();
            let moved = backup.join(CLAIMED).join(name);
            fs::create_dir_all(moved.parent().unwrap()).unwrap();
            put(&home, name, b"first save");
            fs::rename(home.join(name), &moved).unwrap();
            put(&home, name, b"second save");

            let result = migrate_with(&home, &apply_change).unwrap();
            assert_eq!(result["backup"], json!(backup));
            assert_eq!(result["preserved"], json!([name]));
            assert_eq!(fs::read(home.join(name)).unwrap(), b"second save");
            assert_eq!(fs::read(moved).unwrap(), b"first save");
            assert_eq!(
                fs::read(backup.join(name)).unwrap(),
                original.changes[name].before
            );
            assert!(!home.join(auditor).exists());
            assert!(!home.join(STATE).exists());
            assert!(!home.join(JOURNAL).exists());
        }
    }

    #[test]
    fn restart_finishes_the_original_backup_after_each_interrupted_move() {
        let temporary = tempfile::tempdir().unwrap();
        let auditor = "agents/auditor.toml";
        let coder = "agents/coder.toml";
        let order = [AGENTS, auditor, coder, STATE];
        for (stop, &name) in order.iter().enumerate() {
            let home = temporary.path().join(stop.to_string());
            let state = legacy(&home, 1, &[auditor, coder]);
            let original = plan(&home).unwrap();
            let backup = create_backup(&home, &original).unwrap();
            for &completed in &order[..stop] {
                assert!(
                    apply_change(&home, completed, &backup, &original.changes[completed]).unwrap()
                );
            }
            let change = &original.changes[name];
            let moved = backup.join(CLAIMED).join(name);
            fs::create_dir_all(moved.parent().unwrap()).unwrap();
            move_checked(&home.join(name), &moved, &change.before).unwrap();
            if name == coder {
                put(&home, coder, b"new role saved during restart");
            }
            let result = migrate_with(&home, &apply_change).unwrap();
            assert_eq!(result["backup"], json!(backup));
            assert_eq!(fs::read(backup.join(STATE)).unwrap(), state);
            assert_eq!(
                fs::read(backup.join(AGENTS)).unwrap(),
                original.changes[AGENTS].before
            );
            for role in [auditor, coder] {
                assert_eq!(fs::read(backup.join(role)).unwrap(), role.as_bytes());
            }
            assert_eq!(
                fs::read(home.join(AGENTS)).unwrap(),
                *original.changes[AGENTS].after.as_ref().unwrap()
            );
            if name == coder {
                assert_eq!(
                    fs::read(home.join(coder)).unwrap(),
                    b"new role saved during restart"
                );
                assert_eq!(result["preserved"], json!([coder]));
            } else {
                assert!(!home.join(coder).exists());
            }
            assert!(!home.join(auditor).exists());
            assert!(!home.join(STATE).exists());
            assert!(!home.join(JOURNAL).exists());
            assert_eq!(fs::read_dir(backup.parent().unwrap()).unwrap().count(), 1);
            assert_eq!(
                migrate_with(&home, &apply_change).unwrap()["status"],
                "unchanged"
            );
        }
    }
}
