use crate::{json, package, Result};
use package::{json_file, require, resources};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub(crate) mod doctor;

fn default_executable() -> OsString {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path).filter(|dir| !dir.as_os_str().is_empty()) {
            for name in ["codex.exe", "codex.cmd", "codex.bat"] {
                let candidate = directory.join(name);
                if !candidate.is_file() {
                    continue;
                }
                if let Ok(path) = std::path::absolute(candidate) {
                    return path.into_os_string();
                }
            }
        }
    }
    OsString::from("codex")
}

struct Client {
    child: Child,
    messages: Receiver<std::result::Result<Value, String>>,
    errors: fs::File,
}

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    fn start(executable: &OsString, home: Option<&Path>, directory: &Path) -> Result<Self> {
        let mut command = Command::new(executable);
        command.args([
            "app-server",
            "--stdio",
            "-c",
            "check_for_update_on_startup=false",
        ]);
        if let Some(home) = home {
            command.env("CODEX_HOME", home).args([
                "--strict-config",
                "--disable",
                "apps",
                "--disable",
                "remote_plugin",
            ]);
        }
        let errors = tempfile::tempfile().map_err(|e| e.to_string())?;
        let mut child = command
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(errors.try_clone().map_err(|e| e.to_string())?)
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
        Ok(Self {
            child,
            messages,
            errors,
        })
    }

    fn transport_error(&mut self, method: &str, cause: impl std::fmt::Display) -> String {
        let mut message = format!("{method}: {cause}");
        if let Ok(Some(status)) = self.child.try_wait() {
            message.push_str(&format!(" (Codex: {status})"));
        }
        let mut tail = Vec::new();
        if let Ok(meta) = self.errors.metadata() {
            let start = meta.len().saturating_sub(4096);
            if self.errors.seek(SeekFrom::Start(start)).is_ok()
                && Read::by_ref(&mut self.errors)
                    .take(4096)
                    .read_to_end(&mut tail)
                    .is_ok()
            {
                let text = String::from_utf8_lossy(&tail);
                if !text.trim().is_empty() {
                    message.push_str(&format!("\n{}", text.trim()));
                }
            }
        }
        message
    }

    fn send(&mut self, value: Value) -> Result<()> {
        let input = self.child.stdin.as_mut().ok_or("Нет stdin app-server")?;
        writeln!(input, "{value}")
            .and_then(|_| input.flush())
            .map_err(|e| e.to_string())
    }

    fn initialize(&mut self, name: &str) -> Result<Value> {
        let info = self.request(1, "initialize", json!({"clientInfo":{"name":name,"version":"1.0"},"capabilities":{"experimentalApi":true,"explicitGatewayOauth":true}}))?;
        self.send(json!({"method":"initialized"}))?;
        Ok(info)
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        self.send(json!({"id":id,"method":method,"params":params}))
            .map_err(|e| self.transport_error(method, e))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let value = self
                .messages
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|e| self.transport_error(method, e))?
                .map_err(|e| self.transport_error(method, e))?;
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

fn check_installed_methods(source: &Path, installed: &Path) -> Result<()> {
    for installed_file in package::descendants(&installed.join("skills"), &[])? {
        if installed_file.is_file() {
            let relative = installed_file
                .strip_prefix(installed)
                .map_err(|e| e.to_string())?;
            require(
                source.join(relative).is_file(),
                format!("Лишний файл установленного метода: {}", relative.display()),
            )?;
        }
    }
    let mut files = vec![source.join("docs/skills.md")];
    files.extend(package::descendants(&source.join("skills"), &[])?);
    for file in files.into_iter().filter(|path| path.is_file()) {
        let relative = file.strip_prefix(source).map_err(|e| e.to_string())?;
        let target = package::contained(installed, installed, relative, false)?;
        require(
            fs::read(&file).map_err(|e| e.to_string())?
                == fs::read(&target).map_err(|e| e.to_string())?,
            format!("Установленная копия отличается: {}", relative.display()),
        )?;
    }
    Ok(())
}

pub fn run(args: &[OsString], root: &Path) -> Result<()> {
    let executable = match args {
        [] => default_executable(),
        [flag, path] if flag == "--codex" => path.clone(),
        _ => return Err("check-installation [--codex PATH]".into()),
    };
    package::check(root)?;
    let version = json_file(&root.join(".codex-plugin/plugin.json"))?["version"].clone();
    let expected: std::collections::BTreeSet<_> = package::skill_names(root)?
        .into_iter()
        .map(|n| format!("fstack:{n}"))
        .collect();
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let directory = temporary.path();
    let home = directory.join("codex");
    fs::create_dir(&home).map_err(|e| e.to_string())?;
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
            ".".into(),
        ],
        &home,
        root,
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
    check_installed_methods(root, &installed_path)?;
    for path in package::entries(&installed_path.join("skills"))? {
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
        "Для проверки клиента подготовьте полный локальный пакет с бинарником",
    )?;
    require(
        ["AGENTS.md", "agents"]
            .iter()
            .all(|path| !home.join(path).exists()),
        "Пакет создал пользовательские инструкции или агентов",
    )?;
    let mut client = Client::start(&executable, Some(&home), directory)?;
    client.initialize("fstack-installation-check")?;
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
        actual == expected && skills.iter().all(|v| v["enabled"] == true),
        "Обнаруженные навыки не совпадают с пакетом",
    )?;
    let hooks = flatten(hooks, "hooks")?;
    let expected_handlers = BTreeMap::from([
        ("sessionStart", 2),
        ("subagentStart", 1),
        ("userPromptSubmit", 1),
    ]);
    let mut actual_handlers = BTreeMap::new();
    for hook in &hooks {
        let event = hook["eventName"].as_str().ok_or("Нет eventName")?;
        *actual_handlers.entry(event).or_insert(0) += 1;
    }
    require(
        actual_handlers == expected_handlers,
        format!("Обнаруженные обработчики не совпадают с пакетом: {actual_handlers:?}"),
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
    let events: Vec<_> = expected_handlers.keys().copied().collect();
    println!(
        "{}",
        json!({"status":"passed","client":client_version.trim(),"version":version,"skills_discovered":expected.len(),"skill_files_and_catalog":"matched_source","hooks_discovered":events,"profile_instructions_created":false,"global_roles_installed":false,"model_availability":"not_checked","implicit_invocation":"not_verified","model_tasks":"not_run","role_execution":"not_run"})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_templates_must_match_the_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let installed = temp.path().join("installed");
        let template = Path::new("skills/example/assets/template.md");
        for root in [&source, &installed] {
            fs::create_dir_all(root.join(template).parent().unwrap()).unwrap();
            fs::create_dir_all(root.join("docs")).unwrap();
            fs::write(root.join("docs/skills.md"), "catalog").unwrap();
            fs::write(root.join(template), "current template").unwrap();
        }
        check_installed_methods(&source, &installed).unwrap();
        let obsolete = installed.join("skills/example/assets/retired.md");
        fs::write(&obsolete, "retired template").unwrap();
        assert!(check_installed_methods(&source, &installed).is_err());
        fs::remove_file(obsolete).unwrap();
        fs::write(installed.join(template), "previous template").unwrap();
        assert!(check_installed_methods(&source, &installed).is_err());
        fs::remove_file(installed.join(template)).unwrap();
        assert!(check_installed_methods(&source, &installed).is_err());
    }
}
