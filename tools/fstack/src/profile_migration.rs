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

fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().ok_or("Нет родительского каталога")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
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

fn check_snapshot(home: &Path, name: &str, expected: Option<&[u8]>) -> Result<()> {
    let matches = match snapshot(home, name)? {
        FileState::Missing => expected.is_none(),
        FileState::Content(bytes) => expected == Some(&bytes),
        FileState::Unsafe => false,
    };
    if matches {
        Ok(())
    } else {
        Err(format!("Файл изменился во время миграции: {name}"))
    }
}

fn apply_change(path: &Path, after: Option<&[u8]>) -> Result<()> {
    if let Some(bytes) = after {
        atomic_write(path, bytes, true)
    } else {
        fs::remove_file(path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

fn report(plan: &Plan, status: &str) -> Value {
    json!({
        "status": status,
        "changes": plan.changes.keys().collect::<Vec<_>>(),
        "removed": plan.changes.iter().filter(|(_, change)| change.after.is_none()).map(|(name, _)| name).collect::<Vec<_>>(),
        "preserved": plan.preserved,
    })
}

fn migrate_with(home: &Path, apply: &impl Fn(&Path, Option<&[u8]>) -> Result<()>) -> Result<Value> {
    match snapshot(home, STATE)? {
        FileState::Missing => return Ok(report(&Plan::default(), "unchanged")),
        FileState::Unsafe => return Err(format!("Ожидался обычный файл {STATE}")),
        FileState::Content(_) => {}
    }
    let _lock = lock(home)?;
    if snapshot(home, "fstack/profile.lock")? != FileState::Missing {
        return Err("Прежний помощник удерживает fstack/profile.lock".into());
    }
    let plan = plan(home)?;
    if plan.changes.is_empty() {
        return Ok(report(&plan, "unchanged"));
    }
    let backup_dir = home.join("fstack/backups");
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
    let backup = backup_dir.join(format!("profile-migration-{stamp}-{}", std::process::id()));
    private_backup(&backup)?;
    for (name, change) in &plan.changes {
        atomic_write(&backup.join(name), &change.before, false)?;
    }
    let mut receipt = report(&plan, "backup");
    receipt["files"] = json!(plan
        .changes
        .iter()
        .map(|(name, change)| (*name, digest(&change.before)))
        .collect::<BTreeMap<_, _>>());
    let manifest = serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?;
    atomic_write(&backup.join("manifest.json"), &manifest, false)?;

    let mut completed: Vec<&str> = Vec::new();
    let ordered = plan
        .changes
        .keys()
        .copied()
        .filter(|&name| name != STATE)
        .chain([STATE]);
    for name in ordered {
        let change = &plan.changes[name];
        let result = check_snapshot(home, name, Some(&change.before))
            .and_then(|()| apply(&home.join(name), change.after.as_deref()));
        if let Err(error) = result {
            let mut errors = vec![error];
            for name in completed.into_iter().rev() {
                let change = &plan.changes[name];
                let result = check_snapshot(home, name, change.after.as_deref()).and_then(|()| {
                    atomic_write(&home.join(name), &change.before, change.after.is_some())
                });
                if let Err(error) = result {
                    errors.push(error);
                }
            }
            return Err(format!(
                "{}\nРезервная копия: {}",
                errors.join("\n"),
                backup.display()
            ));
        }
        completed.push(name);
    }
    let mut result = report(&plan, "applied");
    result["backup"] = json!(backup);
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
        let plan = plan(&home)?;
        report(
            &plan,
            if plan.changes.is_empty() {
                "unchanged"
            } else {
                "planned"
            },
        )
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
        atomic_write(&home.join(name), bytes, true).unwrap();
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
            let fail_on_state = |path: &Path, content: Option<&[u8]>| {
                if path == home.join(STATE) {
                    assert!(!home.join(auditor).exists());
                    if concurrent {
                        put(&home, auditor, b"new local role");
                    }
                    return Err("test write failure".into());
                }
                apply_change(path, content)
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
}
