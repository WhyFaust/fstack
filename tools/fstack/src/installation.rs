use super::{json, json_file, require, resources, Result, ROLES};
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Client {
    child: Child,
    messages: Receiver<std::result::Result<Value, String>>,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    fn start(executable: &OsString, home: &Path, directory: &Path) -> Result<Self> {
        let log = fs::File::create(directory.join("app-server.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(executable)
            .args([
                "app-server",
                "--stdio",
                "--strict-config",
                "--disable",
                "apps",
                "--disable",
                "remote_plugin",
                "-c",
                "check_for_update_on_startup=false",
            ])
            .env("CODEX_HOME", home)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().ok_or("Нет stdout app-server")?;
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = line
                    .map_err(|e| e.to_string())
                    .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()));
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, messages })
    }

    fn send(&mut self, value: Value) -> Result<()> {
        let input = self.child.stdin.as_mut().ok_or("Нет stdin app-server")?;
        writeln!(input, "{value}")
            .and_then(|_| input.flush())
            .map_err(|e| e.to_string())
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        self.send(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let value = self
                .messages
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| format!("{method}: {e}"))??;
            if value["id"] == id {
                require(
                    value.get("error").is_none(),
                    format!("{method}: {}", value["error"]),
                )?;
                return Ok(value["result"].clone());
            }
        }
    }
}

fn command(
    executable: &OsString,
    args: &[OsString],
    home: &Path,
    directory: &Path,
) -> Result<String> {
    let output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let errors = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = Command::new(executable)
        .args(args)
        .env("CODEX_HOME", home)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(output.try_clone().map_err(|e| e.to_string())?)
        .stderr(errors.try_clone().map_err(|e| e.to_string())?)
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Истёк таймаут команды: {args:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    use std::io::{Read, Seek};
    let mut output = output;
    let mut errors = errors;
    output.rewind().map_err(|e| e.to_string())?;
    errors.rewind().map_err(|e| e.to_string())?;
    let mut text = String::new();
    let mut error = String::new();
    output
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    errors
        .read_to_string(&mut error)
        .map_err(|e| e.to_string())?;
    require(status.success(), format!("{args:?}: {error}{text}"))?;
    Ok(text)
}

pub fn run(args: &[OsString], root: &Path) -> Result<()> {
    let executable = match args {
        [] => OsString::from("codex"),
        [flag, path] if flag == "--codex" => path.clone(),
        _ => return Err("check-installation [--codex PATH]".into()),
    };
    super::check(root)?;
    let version = json_file(&root.join(".codex-plugin/plugin.json"))?["version"].clone();
    let expected: std::collections::BTreeSet<_> = super::skill_names(root)?
        .into_iter()
        .map(|n| format!("fstack:{n}"))
        .collect();
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let directory = temporary.path();
    let home = directory.join("codex");
    fs::create_dir(&home).map_err(|e| e.to_string())?;
    fs::copy(
        root.join("profile/config.example.toml"),
        home.join("config.toml"),
    )
    .map_err(|e| e.to_string())?;
    let execute = |args: &[&str]| {
        command(
            &executable,
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &home,
            directory,
        )
    };
    let client_version = execute(&["--version"])?;
    command(
        &executable,
        &[
            "plugin".into(),
            "marketplace".into(),
            "add".into(),
            root.as_os_str().to_owned(),
        ],
        &home,
        directory,
    )?;
    let installed: Value =
        serde_json::from_str(&execute(&["plugin", "add", "fstack@fstack", "--json"])?)
            .map_err(|e| e.to_string())?;
    let installed_path = PathBuf::from(
        installed["installedPath"]
            .as_str()
            .ok_or("Нет installedPath")?,
    )
    .canonicalize()
    .map_err(|e| e.to_string())?;
    require(
        installed_path.starts_with(home.canonicalize().map_err(|e| e.to_string())?)
            && installed["version"] == version,
        "Установка не соответствует временному профилю или версии",
    )?;
    let listing: Value = serde_json::from_str(&execute(&["plugin", "list", "--json"])?)
        .map_err(|e| e.to_string())?;
    require(
        listing["installed"].as_array().is_some_and(|items| {
            items.iter().any(|v| {
                v["pluginId"] == "fstack@fstack" && v["enabled"] == true && v["installed"] == true
            })
        }),
        "Нет включённого плагина",
    )?;
    for path in super::entries(&installed_path.join("skills"))? {
        if path.is_dir() {
            resources(&path, &installed_path)?;
        }
    }
    let native_path = installed_path.join(if cfg!(windows) {
        "bin/fstack.exe"
    } else {
        "bin/fstack"
    });
    require(
        native_path.is_file(),
        "В установленном пакете нет платформенного bin/fstack; подготовьте полный пакет",
    )?;
    let native = native_path.into_os_string();
    let mut profile_args: Vec<OsString> = vec![
        "profile".into(),
        "--home".into(),
        home.as_os_str().into(),
        "--apply".into(),
    ];
    command(&native, &profile_args, &home, directory)?;
    require(
        !home.join("AGENTS.md").exists(),
        "Установка создала общий AGENTS.md",
    )?;
    profile_args.pop();
    let repeat: Value = serde_json::from_str(&command(&native, &profile_args, &home, directory)?)
        .map_err(|e| e.to_string())?;
    require(
        repeat["status"] == "unchanged",
        "Повторная синхронизация меняет профиль",
    )?;
    let state = json_file(&home.join("fstack/profile.json"))?;
    let files = state["files"].as_object().ok_or("Нет files состояния")?;
    let expected_files: std::collections::BTreeSet<_> =
        ROLES.iter().map(|n| format!("agents/{n}.toml")).collect();
    require(
        files
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            == expected_files
            && state["schema"] == 2
            && state.get("links_sha256").is_none(),
        "Состояние содержит другой набор файлов",
    )?;
    let installed_roles: std::collections::BTreeSet<_> = super::entries(&home.join("agents"))?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    require(
        installed_roles == ROLES.iter().map(|n| n.to_string()).collect(),
        "Установлен другой набор ролей",
    )?;
    for name in ROLES {
        let relative = format!("agents/{name}.toml");
        let source =
            fs::read(installed_path.join("profile").join(&relative)).map_err(|e| e.to_string())?;
        require(
            fs::read(home.join(&relative)).map_err(|e| e.to_string())? == source,
            format!("Роль отличается: {name}"),
        )?;
        use sha2::{Digest, Sha256};
        require(
            files[&relative] == format!("{:x}", Sha256::digest(source)),
            "Хеш роли отличается",
        )?;
    }
    let mut client = Client::start(&executable, &home, directory)?;
    client.request(1, "initialize", json!({"clientInfo":{"name":"fstack-installation-check","version":"1.0"},"capabilities":{"experimentalApi":true,"explicitGatewayOauth":true}}))?;
    client.send(json!({"method":"initialized"}))?;
    let skills = client.request(
        2,
        "skills/list",
        json!({"cwds":[directory],"forceReload":true}),
    )?;
    let hooks = client.request(3, "hooks/list", json!({"cwds":[directory]}))?;
    let flatten = |value: Value, key: &str| -> Result<Vec<Value>> {
        let entries = value["data"].as_array().ok_or("Нет data обнаружения")?;
        let mut result = Vec::new();
        for entry in entries {
            require(
                entry["errors"].as_array().is_some_and(Vec::is_empty),
                format!("Ошибки обнаружения: {}", entry["errors"]),
            )?;
            result.extend(
                entry[key]
                    .as_array()
                    .ok_or("Нет результатов обнаружения")?
                    .iter()
                    .filter(|v| v["pluginId"] == "fstack@fstack")
                    .cloned(),
            );
        }
        Ok(result)
    };
    let skills = flatten(skills, "skills")?;
    let actual: std::collections::BTreeSet<_> = skills
        .iter()
        .filter_map(|v| v["name"].as_str().map(str::to_owned))
        .collect();
    require(
        actual == expected
            && skills.iter().all(|v| {
                v["enabled"] == true
                    && v["interface"]["defaultPrompt"].as_str().is_some_and(|p| {
                        p.contains(&format!("${}", v["name"].as_str().unwrap_or("")))
                    })
            }),
        "Обнаруженные навыки не совпадают с пакетом",
    )?;
    let hooks = flatten(hooks, "hooks")?;
    let events: std::collections::BTreeSet<_> =
        ["sessionStart", "userPromptSubmit", "postToolUse", "stop"]
            .into_iter()
            .collect();
    let actual: std::collections::BTreeSet<_> = hooks
        .iter()
        .filter_map(|v| v["eventName"].as_str())
        .collect();
    require(
        hooks.len() == events.len() && actual == events,
        "Обнаруженные события не совпадают с пакетом",
    )?;
    for hook in hooks {
        let source = Path::new(hook["sourcePath"].as_str().ok_or("Нет sourcePath")?)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        require(
            hook["source"] == "plugin"
                && hook["handlerType"] == "command"
                && hook["trustStatus"] == "untrusted"
                && source.starts_with(&installed_path),
            "Источник или доверие хука не совпадают",
        )?;
    }
    println!(
        "{}",
        json!({"status":"passed","client":client_version.trim(),"version":version,"skills_discovered":expected.len(),"hooks_discovered":events,"profile_roundtrip":"unchanged","roles_installed":ROLES,"model_availability":"not_checked","implicit_invocation":"not_verified","model_tasks":"not_run","role_execution":"not_run"})
    );
    Ok(())
}
