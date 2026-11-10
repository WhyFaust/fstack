use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

const START: &str = "<!-- fstack:profile:start -->";
const END: &str = "<!-- fstack:profile:end -->";
const STATE: &str = "fstack/profile.json";
const RETIRED: &str = "agents/python_coder.toml";
type Files = BTreeMap<String, Option<Vec<u8>>>;
type Result<T> = std::result::Result<T, String>;
fn digest(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}
fn conflict(paths: Vec<String>) -> String {
    let mut paths = paths;
    paths.sort();
    paths.dedup();
    json!({"status":"conflict","paths":paths}).to_string()
}
fn target(home: &Path, name: &str) -> Result<PathBuf> {
    let mut path = home.to_path_buf();
    for part in Path::new(name).components() {
        if !matches!(part, Component::Normal(_)) {
            return Err(conflict(vec![name.into()]));
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Err(conflict(vec![name.into()]));
                    }
                }
                if meta.file_type().is_symlink() {
                    return Err(conflict(vec![name.into()]));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(path)
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn merge(
    content: Option<&[u8]>,
    links: &str,
    expected: Option<&str>,
    adopt: bool,
) -> Result<(Vec<u8>, String)> {
    let text = std::str::from_utf8(content.unwrap_or_default()).map_err(|e| e.to_string())?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block = format!(
        "{START}{newline}{}{newline}{END}",
        links.replace("\r\n", "\n").replace('\n', newline)
    );
    let updated = match (text.matches(START).count(), text.matches(END).count()) {
        (0, 0) => {
            if expected.is_some() {
                return Err(conflict(vec!["AGENTS.md:managed-block".into()]));
            }
            format!(
                "{}{}{block}{newline}",
                text,
                if text.is_empty() {
                    String::new()
                } else if text.ends_with('\n') {
                    newline.into()
                } else {
                    newline.repeat(2)
                }
            )
        }
        (1, 1) if text.find(START) < text.find(END) => {
            let begin = text.find(START).unwrap();
            let end = text.find(END).unwrap() + END.len();
            let current = &text[begin..end];
            if expected.is_some_and(|v| digest(current.as_bytes()) != v) {
                return Err(conflict(vec!["AGENTS.md:managed-block".into()]));
            }
            if expected.is_none() && current != block && !adopt {
                return Err(conflict(vec!["AGENTS.md:unmanaged-block".into()]));
            }
            format!("{}{block}{}", &text[..begin], &text[end..])
        }
        _ => return Err(conflict(vec!["AGENTS.md:invalid-markers".into()])),
    };
    Ok((updated.into_bytes(), digest(block.as_bytes())))
}
#[derive(Debug)]
struct Plan {
    version: String,
    before: Files,
    changes: Files,
}
fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    Ok((serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n").into_bytes())
}
fn plan(home: &Path, root: &Path, adopt: bool) -> Result<Plan> {
    let manifest: Value = serde_json::from_slice(
        &fs::read(root.join(".codex-plugin/plugin.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let version = manifest["version"]
        .as_str()
        .filter(|v| {
            regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+\z")
                .unwrap()
                .is_match(v)
        })
        .ok_or("Некорректная версия установленного плагина")?
        .to_owned();
    let mut payloads = Files::new();
    for entry in fs::read_dir(root.join("profile/agents")).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or("Некорректное имя роли")?;
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        let data: toml::Value =
            toml::from_str(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if data.get("name").and_then(|v| v.as_str()) != Some(name)
            || ["description", "developer_instructions"].iter().any(|key| {
                data.get(key)
                    .and_then(|v| v.as_str())
                    .is_none_or(|v| v.is_empty())
            })
        {
            return Err(format!("Некорректная роль в пакете: {name}"));
        }
        payloads.insert(format!("agents/{name}.toml"), Some(bytes));
    }
    if payloads.is_empty() {
        return Err("Нет ролей в пакете".into());
    }
    let links = fs::read_to_string(root.join("profile/AGENTS.md")).map_err(|e| e.to_string())?;
    let links = links.trim();
    if links.is_empty() || links.contains(START) || links.contains(END) {
        return Err("Некорректный управляемый блок в пакете".into());
    }
    let state_content = read(&target(home, STATE)?)?;
    let state: Value = state_content
        .as_ref()
        .map(|v| serde_json::from_slice(v))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(Value::Null);
    let mut owned = BTreeMap::new();
    if state_content.is_some() {
        if state["schema"].as_u64() != Some(1) {
            return Err("Некорректное состояние fstack/profile.json".into());
        }
        let files = state["files"]
            .as_object()
            .ok_or("Некорректное состояние fstack/profile.json")?;
        let keys: std::collections::BTreeSet<_> = files.keys().cloned().collect();
        let current: std::collections::BTreeSet<_> = payloads.keys().cloned().collect();
        let legacy = [
            "auditor",
            "coder",
            "python_coder",
            "reviewer",
            "allure_analyst",
        ]
        .map(|n| format!("agents/{n}.toml"))
        .into_iter()
        .collect();
        if keys != current && keys != legacy {
            return Err("Состав управляемого профиля отличается от поддерживаемого".into());
        }
        for value in files
            .values()
            .chain(std::iter::once(&state["links_sha256"]))
        {
            if value.as_str().is_none_or(|v| {
                v.len() != 64
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) {
                return Err("Некорректные хеши управляемого профиля".into());
            }
        }
        owned.extend(
            files
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned())),
        );
    }
    let hashes: BTreeMap<_, _> = payloads
        .iter()
        .map(|(k, v)| (k.clone(), digest(v.as_ref().unwrap())))
        .collect();
    if owned.contains_key(RETIRED) && !payloads.contains_key(RETIRED) {
        payloads.insert(RETIRED.into(), None);
    }
    let mut before = Files::new();
    let mut conflicts = Vec::new();
    for (name, content) in &payloads {
        let current = read(&target(home, name)?)?;
        if let Some(hash) = owned.get(name) {
            if current.as_ref().is_none_or(|v| digest(v) != *hash) {
                conflicts.push(name.clone());
            }
        } else if current.is_some() && current != *content && !adopt {
            conflicts.push(name.clone());
        }
        before.insert(name.clone(), current);
    }
    let agents = read(&target(home, "AGENTS.md")?)?;
    let merged = merge(
        agents.as_deref(),
        links,
        state["links_sha256"].as_str(),
        adopt,
    );
    if let Err(error) = &merged {
        if let Ok(value) = serde_json::from_str::<Value>(error) {
            if let Some(paths) = value["paths"].as_array() {
                conflicts.extend(paths.iter().filter_map(|v| v.as_str().map(str::to_owned)));
            } else {
                return Err(error.clone());
            }
        } else {
            return Err(error.clone());
        }
    }
    if !conflicts.is_empty() {
        return Err(conflict(conflicts));
    }
    let (updated, hash) = merged?;
    before.insert("AGENTS.md".into(), agents);
    before.insert(STATE.into(), state_content);
    payloads.insert("AGENTS.md".into(), Some(updated));
    payloads.insert(
        STATE.into(),
        Some(json_bytes(
            &json!({"files":hashes,"links_sha256":hash,"schema":1,"version":version}),
        )?),
    );
    payloads.retain(|name, bytes| before.get(name) != Some(bytes));
    Ok(Plan {
        version,
        before,
        changes: payloads,
    })
}
fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("Нет родительского каталога")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(content)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
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
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|e| e.to_string())?;
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
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn result(home: &Path, plan: &Plan, status: &str) -> Value {
    json!({"home":home,"status":status,"version":plan.version,"changes":plan.changes.keys().collect::<Vec<_>>(),"removed":plan.changes.iter().filter(|(_,v)|v.is_none()).map(|(k,_)|k).collect::<Vec<_>>(),"unmanaged_retired":if !plan.before.contains_key(RETIRED) && fs::symlink_metadata(home.join(RETIRED)).is_ok() { vec![RETIRED] } else { vec![] }})
}
fn apply_with(
    home: &Path,
    root: &Path,
    adopt: bool,
    write: &impl Fn(&Path, &[u8]) -> Result<()>,
    protect: &impl Fn(&Path) -> Result<()>,
) -> Result<Value> {
    let initial = plan(home, root, adopt)?;
    if initial.changes.is_empty() {
        return Ok(result(home, &initial, "unchanged"));
    }
    let path = target(home, "fstack/profile.lock")?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            conflict(vec!["fstack/profile.lock".into()])
        } else {
            e.to_string()
        }
    })?;
    let _lock = Lock(path);
    write!(file, "{}", std::process::id()).map_err(|e| e.to_string())?;
    let plan = plan(home, root, adopt)?;
    if plan.changes.is_empty() {
        return Ok(result(home, &plan, "unchanged"));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let backup = target(
        home,
        &format!("fstack/backups/profile-{stamp}-{}", std::process::id()),
    )?;
    protect(&backup)?;
    for name in plan.changes.keys() {
        if let Some(bytes) = &plan.before[name] {
            atomic_write(&backup.join(name), bytes)?;
        }
    }
    let hashes: BTreeMap<_, _> = plan
        .changes
        .keys()
        .filter_map(|name| plan.before[name].as_ref().map(|v| (name, digest(v))))
        .collect();
    atomic_write(
        &backup.join("manifest.json"),
        &json_bytes(
            &json!({"version":plan.version,"files":hashes,"created":plan.changes.keys().filter(|n|plan.before[*n].is_none()).collect::<Vec<_>>(),"removed":plan.changes.iter().filter(|(_,v)|v.is_none()).map(|(n,_)|n).collect::<Vec<_>>()}),
        )?,
    )?;
    let mut written: Vec<&String> = Vec::new();
    let mut ordered: Vec<_> = plan
        .changes
        .keys()
        .filter(|name| name.as_str() != STATE)
        .collect();
    let state_name = STATE.to_owned();
    if plan.changes.contains_key(STATE) {
        ordered.push(&state_name);
    }
    for name in ordered {
        let operation = (|| {
            let path = target(home, name)?;
            if read(&path)? != plan.before[name] {
                return Err(conflict(vec![name.clone()]));
            }
            if let Some(content) = &plan.changes[name] {
                write(&path, content)
            } else {
                fs::remove_file(path).map_err(|e| e.to_string())
            }
        })();
        if let Err(original) = operation {
            let mut errors = vec![original];
            for name in written.into_iter().rev() {
                let restored = (|| {
                    let path = target(home, name)?;
                    if read(&path)? != plan.changes[name] {
                        return Err(conflict(vec![name.clone()]));
                    }
                    if let Some(bytes) = &plan.before[name] {
                        atomic_write(&path, bytes)
                    } else {
                        fs::remove_file(path).map_err(|e| e.to_string())
                    }
                })();
                if let Err(error) = restored {
                    errors.push(error);
                }
            }
            return Err(if errors.len() == 1 {
                errors.remove(0)
            } else {
                json!({"status":"error","message":"Ошибка синхронизации и восстановления; используйте резервную копию","causes":errors,"backup":backup}).to_string()
            });
        }
        written.push(name);
    }
    let mut output = result(home, &plan, "applied");
    output["backup"] = json!(backup);
    Ok(output)
}
pub fn run(args: &[OsString], package: &Path) -> Result<()> {
    let mut home = None;
    let mut apply = false;
    let mut adopt = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--home") => home = Some(PathBuf::from(iter.next().ok_or("--home требует путь")?)),
            Some("--apply") => apply = true,
            Some("--adopt") => adopt = true,
            _ => {
                return Err(
                    "Использование: fstack profile [--home PATH] [--apply [--adopt]]".into(),
                )
            }
        }
    }
    if adopt && !apply {
        return Err("--adopt допустим только вместе с --apply".into());
    }
    let home = home
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|v| PathBuf::from(v).join(".codex"))
        })
        .ok_or("Не найден домашний каталог")?;
    let home = if let Ok(relative) = home.strip_prefix("~") {
        PathBuf::from(
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .ok_or("Не найден домашний каталог")?,
        )
        .join(relative)
    } else {
        home
    };
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(home)
    };
    let home = if home.exists() {
        fs::canonicalize(home).map_err(|e| e.to_string())?
    } else {
        home
    };
    let output = if apply {
        apply_with(&home, package, adopt, &atomic_write, &private_backup)?
    } else {
        let plan = plan(&home, package, false)?;
        result(
            &home,
            &plan,
            if plan.changes.is_empty() {
                "unchanged"
            } else {
                "planned"
            },
        )
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("package");
        let home = dir.path().join("home");
        fs::create_dir_all(root.join("profile/agents")).unwrap();
        fs::create_dir_all(root.join(".codex-plugin")).unwrap();
        fs::write(
            root.join(".codex-plugin/plugin.json"),
            r#"{"version":"2.0.0"}"#,
        )
        .unwrap();
        fs::write(root.join("profile/AGENTS.md"), "Canonical instructions\n").unwrap();
        for name in ["auditor", "coder", "reviewer", "verifier", "allure_analyst"] {
            fs::write(root.join(format!("profile/agents/{name}.toml")),format!("name = \"{name}\"\ndescription = \"Role\"\ndeveloper_instructions = \"Instructions\"\n")).unwrap();
        }
        (dir, root, home)
    }
    fn apply(home: &Path, root: &Path, adopt: bool) -> Result<Value> {
        apply_with(home, root, adopt, &atomic_write, &private_backup)
    }
    fn snapshot(home: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(path: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            if !path.exists() {
                return;
            }
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().unwrap() == "backups" {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(
                        path.strip_prefix(root).unwrap().into(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(home, home, &mut out);
        out
    }
    fn legacy(home: &Path) {
        let mut hashes = BTreeMap::new();
        for name in [
            "auditor",
            "coder",
            "python_coder",
            "reviewer",
            "allure_analyst",
        ] {
            let relative = format!("agents/{name}.toml");
            let bytes = format!("Legacy {name}\n").into_bytes();
            atomic_write(&home.join(&relative), &bytes).unwrap();
            hashes.insert(relative, digest(&bytes));
        }
        let block = format!("{START}\r\nLegacy links\r\n{END}");
        atomic_write(
            &home.join("AGENTS.md"),
            format!("User text\r\n\r\n{block}\r\nUser tail\r\n").as_bytes(),
        )
        .unwrap();
        atomic_write(&home.join(STATE),&json_bytes(&json!({"schema":1,"version":"1.0.0","files":hashes,"links_sha256":digest(block.as_bytes())})).unwrap()).unwrap();
    }
    #[test]
    fn dry_run_roundtrip_private_backup_and_adoption() {
        let (_dir, root, home) = fixture();
        let planned = plan(&home, &root, false).unwrap();
        assert!(!planned.changes.is_empty());
        assert!(!home.exists());
        atomic_write(&home.join("AGENTS.md"), b"User instructions\r\n").unwrap();
        atomic_write(&home.join("agents/coder.toml"), b"User-owned role").unwrap();
        atomic_write(&home.join(RETIRED), b"User-owned Python role").unwrap();
        let before = snapshot(&home);
        assert!(apply(&home, &root, false).is_err());
        assert_eq!(snapshot(&home), before);
        let result = apply(&home, &root, true).unwrap();
        let backup = Path::new(result["backup"].as_str().unwrap());
        assert_eq!(
            fs::read(backup.join("agents/coder.toml")).unwrap(),
            b"User-owned role"
        );
        assert_eq!(result["unmanaged_retired"], json!([RETIRED]));
        assert_eq!(
            fs::read(home.join(RETIRED)).unwrap(),
            b"User-owned Python role"
        );
        assert!(!fs::read_to_string(home.join("AGENTS.md"))
            .unwrap()
            .replace("\r\n", "")
            .contains('\n'));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(backup.join("AGENTS.md"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        {
            let output=std::process::Command::new(system_powershell().unwrap()).args(["-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop'; $acl=Get-Acl -LiteralPath $env:FSTACK_TEST_BACKUP; @{Protected=$acl.AreAccessRulesProtected; Owner=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; Rules=@($acl.Access | ForEach-Object { $_.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value })} | ConvertTo-Json -Compress"]).env("FSTACK_TEST_BACKUP",backup).output().unwrap();
            assert!(output.status.success());
            let acl: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(acl["Protected"], true);
            let rules: std::collections::BTreeSet<_> = acl["Rules"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(
                rules,
                [acl["Owner"].as_str().unwrap(), "S-1-5-18"]
                    .into_iter()
                    .collect()
            );
        }
        let before = snapshot(&home);
        assert_eq!(apply(&home, &root, false).unwrap()["status"], "unchanged");
        assert_eq!(snapshot(&home), before);
        assert_eq!(fs::read_dir(backup.parent().unwrap()).unwrap().count(), 1);
        atomic_write(&home.join("agents/coder.toml"), b"Manual change").unwrap();
        let before = snapshot(&home);
        for adopt in [false, true] {
            assert!(apply(&home, &root, adopt)
                .unwrap_err()
                .contains("agents/coder.toml"));
            assert_eq!(snapshot(&home), before);
        }
    }
    #[test]
    fn legacy_migration_and_invalid_states_are_transactional() {
        let (_dir, root, home) = fixture();
        legacy(&home);
        let before = snapshot(&home);
        let planned = plan(&home, &root, false).unwrap();
        assert_eq!(planned.changes[RETIRED], None);
        assert_eq!(snapshot(&home), before);
        let result = apply(&home, &root, false).unwrap();
        assert_eq!(result["removed"], json!([RETIRED]));
        assert!(!home.join(RETIRED).exists());
        let backup = Path::new(result["backup"].as_str().unwrap());
        assert_eq!(
            fs::read(backup.join(RETIRED)).unwrap(),
            before[Path::new(RETIRED)]
        );
        let receipt: Value =
            serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(receipt["created"], json!(["agents/verifier.toml"]));
        assert_eq!(receipt["removed"], json!([RETIRED]));
        let state: Value = serde_json::from_slice(&fs::read(home.join(STATE)).unwrap()).unwrap();
        assert_eq!(state["schema"], 1);
        assert!(state["files"].get(RETIRED).is_none());
        assert_eq!(apply(&home, &root, false).unwrap()["status"], "unchanged");
        for malformed in [
            Value::Null,
            json!({"schema":true,"files":{}}),
            json!({"schema":1,"files":[]}),
            {
                let mut v = state.clone();
                v["files"]["../outside.toml"] = json!("0".repeat(64));
                v
            },
            {
                let mut v = state.clone();
                v["links_sha256"] = json!("z".repeat(64));
                v
            },
            {
                let mut v = state.clone();
                v["files"]["agents/coder.toml"] = json!(7);
                v
            },
        ] {
            atomic_write(&home.join(STATE), &json_bytes(&malformed).unwrap()).unwrap();
            let before = snapshot(&home);
            assert!(apply(&home, &root, true).is_err());
            assert_eq!(snapshot(&home), before);
        }
    }
    #[test]
    fn legacy_edits_missing_files_and_markers_block_adoption() {
        for relative in ["agents/coder.toml", RETIRED, "AGENTS.md"] {
            for missing in [false, true] {
                let (_dir, root, home) = fixture();
                legacy(&home);
                if missing {
                    fs::remove_file(home.join(relative)).unwrap();
                } else {
                    atomic_write(&home.join(relative), b"Edited").unwrap();
                }
                let before = snapshot(&home);
                for adopt in [false, true] {
                    assert!(apply(&home, &root, adopt).is_err());
                    assert_eq!(snapshot(&home), before);
                    assert!(!home.join("fstack/backups").exists());
                }
            }
        }
        for text in [
            START.to_owned(),
            format!("{END}{START}"),
            format!("{START}{END}{START}{END}"),
        ] {
            assert!(merge(Some(text.as_bytes()), "new", None, true).is_err());
        }
    }
    #[test]
    fn rollback_restores_retirement_and_preserves_concurrent_edits() {
        for concurrent in [false, true] {
            let (_dir, root, home) = fixture();
            legacy(&home);
            let mut before = snapshot(&home);
            let write = |path: &Path, bytes: &[u8]| {
                if path == home.join(STATE) {
                    assert!(!home.join(RETIRED).exists());
                    if concurrent {
                        atomic_write(&home.join(RETIRED), b"Concurrent recreation")?;
                    }
                    return Err("Injected write failure".into());
                }
                atomic_write(path, bytes)
            };
            let error = apply_with(&home, &root, false, &write, &private_backup).unwrap_err();
            assert!(error.contains("Injected write failure"));
            if concurrent {
                before.insert(RETIRED.into(), b"Concurrent recreation".to_vec());
                assert!(error.contains("causes"));
            }
            assert_eq!(snapshot(&home), before);
            assert!(!home.join("fstack/profile.lock").exists());
        }
        let (_dir, root, home) = fixture();
        atomic_write(&home.join("AGENTS.md"), b"Private original").unwrap();
        let before = snapshot(&home);
        assert!(apply_with(&home, &root, false, &atomic_write, &|_| Err(
            "Protection failed".into()
        ))
        .is_err());
        assert_eq!(snapshot(&home), before);
    }
    #[test]
    fn locks_and_symlinks_prevent_writes() {
        let (_dir, root, home) = fixture();
        atomic_write(&home.join("fstack/profile.lock"), b"Other process").unwrap();
        let before = snapshot(&home);
        assert!(apply(&home, &root, false)
            .unwrap_err()
            .contains("profile.lock"));
        assert_eq!(snapshot(&home), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(home.join("fstack/profile.lock")).unwrap();
            symlink(root.join("profile/agents"), home.join("agents")).unwrap();
            assert!(plan(&home, &root, true).unwrap_err().contains("agents/"));
        }
    }
    #[test]
    fn failed_install_restores_user_files_without_overwriting_concurrent_changes() {
        for concurrent in [false, true] {
            let (_dir, root, home) = fixture();
            atomic_write(&home.join("AGENTS.md"), b"User original").unwrap();
            let mut before = snapshot(&home);
            let write = |path: &Path, bytes: &[u8]| {
                if path == home.join("agents/coder.toml") {
                    if concurrent {
                        atomic_write(&home.join("agents/auditor.toml"), b"Concurrent edit")?;
                    }
                    return Err("Injected failure".into());
                }
                atomic_write(path, bytes)
            };
            assert!(apply_with(&home, &root, false, &write, &private_backup).is_err());
            if concurrent {
                before.insert("agents/auditor.toml".into(), b"Concurrent edit".to_vec());
            }
            assert_eq!(snapshot(&home), before);
            assert!(!home.join("fstack/profile.lock").exists());
        }
    }
}
