use super::{hook_counts, wait, Client};
use crate::{package, version, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const PLUGIN: &str = "fstack@fstack";

#[derive(Default, Serialize)]
struct Report {
    status: &'static str,
    project: PathBuf,
    client: Option<String>,
    profile: Option<PathBuf>,
    installed: Option<bool>,
    enabled: Option<bool>,
    version: Option<String>,
    package: Option<PathBuf>,
    runtime: Option<PathBuf>,
    runtime_present: Option<bool>,
    runtime_healthy: Option<bool>,
    enabled_skills: Option<usize>,
    hooks: BTreeMap<String, Vec<String>>,
    issues: Vec<String>,
}

fn launcher_file(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes())
            .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 == 0
    }
}

fn target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => {
            let translated = std::process::Command::new("/usr/sbin/sysctl")
                .args(["-in", "sysctl.proc_translated"])
                .output()
                .is_ok_and(|o| o.status.success() && o.stdout.trim_ascii() == b"1");
            Ok(if translated {
                "aarch64-apple-darwin"
            } else {
                "x86_64-apple-darwin"
            })
        }
        _ => Err("Для этой платформы нет готового runtime fstack".into()),
    }
}

fn probe_runtime(binary: &Path, package: &Path) -> Result<()> {
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = Command::new(binary)
        .arg("--package")
        .arg(package)
        .args(["hook", "UserPromptSubmit"])
        .current_dir(package)
        .stdin(Stdio::null())
        .stdout(output.try_clone().map_err(|e| e.to_string())?)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let status = wait(&mut child, Duration::from_secs(2))?;
    package::require(status.success(), format!("запуск завершился с {status}"))?;
    output.rewind().map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    output
        .take(16)
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    package::require(response.trim_ascii() == b"{}", "не получен ответ хука")
}

fn runtime(package: &Path, home: &Path) -> Result<PathBuf> {
    let binary = if cfg!(windows) {
        "fstack.exe"
    } else {
        "fstack"
    };
    let bundled = package.join("bin").join(binary);
    if launcher_file(&bundled) {
        return Ok(bundled);
    }
    let launcher = fs::read_to_string(package.join(package::PACKAGE_VERSION))
        .map_err(|e| format!("{}: {e}", package::PACKAGE_VERSION))?;
    let number = launcher
        .strip_suffix('\n')
        .ok_or("Некорректный launchers/package-version")?;
    version::parse(number)?;
    let build = fs::read_to_string(package.join("launchers/runtime-id"))
        .map_err(|e| format!("launchers/runtime-id: {e}"))?;
    let id = build
        .strip_suffix('\n')
        .ok_or("Некорректный launchers/runtime-id")?;
    package::require(
        id.len() == 64
            && id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "Некорректный launchers/runtime-id",
    )?;
    Ok(home
        .join("fstack/runtime")
        .join(number)
        .join(id)
        .join(target()?)
        .join(binary))
}

fn plugin(listing: &Value) -> Result<Option<&Value>> {
    let markets = listing["marketplaces"]
        .as_array()
        .ok_or("Codex не вернул список marketplace")?;
    for market in markets {
        let plugins = market["plugins"]
            .as_array()
            .ok_or("Codex не вернул плагины marketplace")?;
        if let Some(found) = plugins.iter().find(|item| item["id"] == PLUGIN) {
            return Ok(Some(found));
        }
    }
    if listing["marketplaceLoadErrors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(
            "Codex не загрузил один из marketplace. Проверьте codex plugin marketplace list."
                .into(),
        );
    }
    Ok(None)
}

fn cached_package(home: &Path) -> Result<PathBuf> {
    let cache = home.join("plugins/cache/fstack/fstack");
    let versions: Vec<_> = package::entries(&cache)?
        .into_iter()
        .filter(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
        .collect();
    // Codex предпочитает local, затем старшую версию в кэше.
    let selected = if let Some(path) = versions.iter().find(|p| p.ends_with("local")) {
        path
    } else {
        let mut ordered = BTreeMap::new();
        for path in &versions {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let parts = version::parse(name).map_err(|_| {
                format!(
                    "Не удалось определить установленную версию fstack в {}.",
                    cache.display()
                )
            })?;
            ordered.insert(parts, path);
        }
        ordered
            .pop_last()
            .ok_or("Кэш установленного fstack пуст. Выполните codex plugin add fstack@fstack.")?
            .1
    };
    selected.canonicalize().map_err(|e| e.to_string())
}

fn discovery(reply: &Value, key: &str) -> Result<Vec<Value>> {
    let groups = reply["data"]
        .as_array()
        .ok_or_else(|| format!("Codex не вернул data для {key}"))?;
    let mut items = Vec::new();
    for group in groups {
        items.extend(
            group[key]
                .as_array()
                .ok_or_else(|| format!("Нет массива {key}"))?
                .iter()
                .filter(|entry| entry["pluginId"] == PLUGIN)
                .cloned(),
        );
    }
    Ok(items)
}

impl Report {
    fn scan(&mut self, executable: &OsString) -> Result<()> {
        let mut client = Client::start(executable, None, &self.project).map_err(|e| {
            format!(
                "Не удалось запустить Codex ({}): {e}",
                executable.to_string_lossy()
            )
        })?;
        let init = client.initialize("fstack-doctor")?;
        self.client = init["userAgent"].as_str().map(str::to_owned);
        let home = PathBuf::from(
            init["codexHome"]
                .as_str()
                .ok_or("Codex не сообщил путь профиля")?,
        );
        self.profile = Some(home.clone());
        let query = json!({"cwds":[self.project]});
        let listing = client.request(2, "plugin/installed", query.clone())?;
        let Some(entry) = plugin(&listing)? else {
            self.installed = Some(false);
            return Err("Плагин fstack не установлен. Проверьте marketplace и выполните codex plugin add fstack@fstack.".into());
        };
        self.installed = entry["installed"].as_bool();
        package::require(
            self.installed == Some(true),
            "Codex не подтвердил установку fstack. Проверьте codex plugin list --json.",
        )?;
        self.enabled = entry["enabled"].as_bool();
        if self.enabled != Some(true) {
            self.issues.push("Плагин выключен или его состояние неизвестно. Проверьте fstack в настройках плагинов Codex.".into());
        }
        let mut read = |id, key, method| {
            client
                .request(id, method, query.clone())
                .and_then(|reply| discovery(&reply, key))
                .map_err(|e| self.issues.push(format!("{method}: {e}")))
                .ok()
        };
        let skills = read(3, "skills", "skills/list");
        let hooks = read(4, "hooks", "hooks/list");
        self.inspect_installed(&home, skills.as_deref(), hooks.as_deref())
    }

    fn inspect_installed(
        &mut self,
        home: &Path,
        skills: Option<&[Value]>,
        hooks: Option<&[Value]>,
    ) -> Result<()> {
        if let Some(skills) = skills {
            self.enabled_skills = Some(
                skills
                    .iter()
                    .filter(|skill| skill["enabled"] == true)
                    .count(),
            );
        }
        if let Some(hooks) = hooks {
            for hook in hooks {
                let event = hook["eventName"].as_str().ok_or("Нет имени события хука")?;
                let trust = hook["trustStatus"].as_str().unwrap_or("unknown");
                let status = if hook["enabled"] == true {
                    trust.to_owned()
                } else {
                    format!("disabled ({trust})")
                };
                self.hooks.entry(event.to_owned()).or_default().push(status);
            }
            for statuses in self.hooks.values_mut() {
                statuses.sort();
            }
            if self
                .hooks
                .values()
                .flatten()
                .any(|status| !matches!(status.as_str(), "trusted" | "managed"))
            {
                self.issues.push("У fstack есть выключенные или неодобренные хуки. Проверьте их в Settings → Hooks.".into());
            }
        }
        let paths = skills
            .iter()
            .flat_map(|v| v.iter().filter_map(|e| e["path"].as_str()))
            .chain(
                hooks
                    .iter()
                    .flat_map(|v| v.iter().filter_map(|e| e["sourcePath"].as_str())),
            );
        let mut roots = BTreeSet::new();
        for path in paths {
            if let Some(root) = crate::enclosing_package(Path::new(path)) {
                roots.insert(root.canonicalize().map_err(|e| e.to_string())?);
            }
        }
        package::require(
            roots.len() <= 1,
            "Codex загрузил навыки и хуки fstack из разных каталогов. Переустановите плагин.",
        )?;
        let root = match roots.pop_first() {
            Some(root) => root,
            None => cached_package(home)?,
        };
        self.package = Some(root.clone());
        let manifest = package::json_file(&root.join(".codex-plugin/plugin.json"))?;
        package::require(
            manifest["name"] == "fstack",
            "Имя установленного пакета отличается от fstack",
        )?;
        let number = manifest["version"]
            .as_str()
            .ok_or("В установленном манифесте нет версии")?;
        self.version = Some(number.into());
        if let Err(e) = package::check_versions(&root, number) {
            self.issues.push(e);
        }
        match runtime(&root, home) {
            Ok(path) => {
                let present =
                    launcher_file(&path) && fs::metadata(&path).is_ok_and(|m| m.len() > 0);
                self.runtime_present = Some(present);
                if present {
                    let probe = probe_runtime(&path, &root);
                    self.runtime_healthy = Some(probe.is_ok());
                    if let Err(error) = probe {
                        self.issues.push(format!("Бинарник runtime найден, но проверка запуска завершилась ошибкой: {error}. Восстановите runtime по инструкции установки."));
                    }
                } else {
                    self.issues.push("Бинарник runtime недоступен. Откройте чат с одобренным хуком SessionStart или установите runtime запускателем пакета.".into());
                }
                self.runtime = Some(path);
            }
            Err(e) => self.issues.push(e),
        }
        if let Some(skills) = skills {
            self.check_skills(&root, skills)?;
        }
        if let Some(hooks) = hooks {
            let relative = manifest["hooks"]
                .as_str()
                .ok_or("Нет пути hooks в манифесте")?;
            self.check_hooks(&package::contained(&root, &root, relative, false)?, hooks)?;
        }
        Ok(())
    }

    fn check_skills(&mut self, root: &Path, skills: &[Value]) -> Result<()> {
        let expected = package::skill_names(root)?;
        let mut found = BTreeSet::new();
        for skill in skills.iter().filter(|s| s["enabled"] == true) {
            let name = skill["name"]
                .as_str()
                .and_then(|n| n.strip_prefix("fstack:"))
                .unwrap_or("?");
            let path = skill["path"]
                .as_str()
                .and_then(|p| Path::new(p).canonicalize().ok());
            let instruction = root.join("skills").join(name).join("SKILL.md");
            if !expected.contains(name)
                || path.as_deref() != Some(&instruction)
                || !instruction.is_file()
            {
                self.issues.push(format!(
                    "Неверный путь к инструкции навыка {name}. Переустановите fstack."
                ));
            }
            found.insert(name.to_owned());
        }
        for name in expected.difference(&found) {
            self.issues
                .push(format!("Codex не загрузил навык fstack:{name}."));
        }
        Ok(())
    }

    fn check_hooks(&mut self, config: &Path, hooks: &[Value]) -> Result<()> {
        if hooks.iter().any(|entry| {
            entry["source"] != "plugin"
                || entry["handlerType"] != "command"
                || entry["sourcePath"]
                    .as_str()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .as_deref()
                    != Some(config)
        }) {
            self.issues.push(
                "Источник обработчика отличается от файла хуков установленного fstack.".into(),
            );
        }
        let expected = hook_counts(config)?;
        let actual: BTreeMap<_, _> = self
            .hooks
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect();
        if actual != expected {
            self.issues.push("Список событий и обработчиков Codex отличается от hooks.json. Проверьте включение хуков в текущем профиле.".into());
        }
        Ok(())
    }

    fn display(&self) {
        println!(
            "{}",
            if self.issues.is_empty() {
                "Установка fstack исправна."
            } else {
                "Установка fstack требует внимания."
            }
        );
        if let Some(client) = &self.client {
            println!("Codex: {client}");
        }
        if let Some(home) = &self.profile {
            println!("Профиль: {}", home.display());
        }
        println!("Проект: {}", self.project.display());
        if self.installed == Some(true) {
            let state = match self.enabled {
                Some(true) => "включён",
                Some(false) => "выключен",
                None => "состояние неизвестно",
            };
            println!(
                "fstack {} ({state})",
                self.version.as_deref().unwrap_or("?")
            );
        }
        if let Some(path) = &self.package {
            println!("Пакет: {}", path.display());
        }
        if let Some(path) = &self.runtime {
            println!("Runtime: {}", path.display());
        }
        if let Some(count) = self.enabled_skills {
            println!("Включено навыков: {count}");
        }
        for (event, states) in &self.hooks {
            println!("  {event}: {}", states.join(", "));
        }
        for issue in &self.issues {
            println!("• {issue}");
        }
    }
}

fn options(args: &[OsString]) -> Result<(OsString, bool)> {
    let mut codex = None;
    let mut json = false;
    let mut args = args.iter();
    while let Some(option) = args.next() {
        match option.to_str() {
            Some("--codex") if codex.is_none() => {
                let path = args
                    .next()
                    .filter(|v| !v.is_empty() && !v.to_string_lossy().starts_with("--"))
                    .ok_or("После --codex нужен путь")?;
                codex = Some(path.clone());
            }
            Some("--json") if !json => json = true,
            _ => return Err("Использование: fstack doctor [--codex PATH] [--json]".into()),
        }
    }
    Ok((codex.unwrap_or_else(super::default_executable), json))
}

pub(crate) fn run(args: &[OsString]) -> Result<()> {
    let (codex, json) = options(args)?;
    let mut report = Report {
        project: std::env::current_dir().map_err(|e| e.to_string())?,
        ..Report::default()
    };
    if let Err(error) = report.scan(&codex) {
        report.issues.push(error);
    }
    let ok = report.issues.is_empty();
    report.status = if ok { "ok" } else { "attention" };
    if json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        );
    } else {
        report.display();
    }
    package::require(ok, "Диагностика fstack обнаружила проблему")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn reports_installed_package_and_actual_hook_trust() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let root = home.join("plugins/cache/fstack/fstack/0.2.0");
        let skill = root.join("skills/review/SKILL.md");
        let config = root.join("hooks/hooks.json");
        for dir in [".codex-plugin", "launchers", "skills/review", "hooks"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(
            root.join(".codex-plugin/plugin.json"),
            r#"{"name":"fstack","version":"0.2.0","hooks":"./hooks/hooks.json"}"#,
        )
        .unwrap();
        fs::write(root.join("launchers/package-major"), "0\n").unwrap();
        fs::write(root.join(package::PACKAGE_VERSION), "0.2.0\n").unwrap();
        fs::write(
            root.join("launchers/runtime-id"),
            format!("{}\n", "a".repeat(64)),
        )
        .unwrap();
        fs::write(&skill, "# Review").unwrap();
        fs::write(
            &config,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command"}]}]}}"#,
        )
        .unwrap();
        let bundled = root
            .join("bin")
            .join(format!("fstack{}", std::env::consts::EXE_SUFFIX));
        executable(&bundled, b"native");
        let skills = [json!({"name":"fstack:review","path":skill,"enabled":true})];
        let mut hooks = [
            json!({"source":"plugin","handlerType":"command","sourcePath":config,"eventName":"sessionStart","enabled":true,"trustStatus":"trusted"}),
        ];
        let scan = |skills: &[Value], hooks: &[Value]| {
            let mut report = Report::default();
            report
                .inspect_installed(home, Some(skills), Some(hooks))
                .unwrap();
            report
        };
        let bundled_report = scan(&skills, &hooks);
        assert_eq!(bundled_report.issues.len(), 1);
        assert!(bundled_report.issues[0].contains("проверка запуска"));
        assert_eq!(bundled_report.version.as_deref(), Some("0.2.0"));
        assert_eq!(bundled_report.enabled_skills, Some(1));
        assert_eq!(
            bundled_report.package.unwrap(),
            root.canonicalize().unwrap()
        );
        assert_eq!(
            bundled_report.runtime.unwrap(),
            bundled.canonicalize().unwrap()
        );
        assert_eq!(bundled_report.runtime_present, Some(true));
        assert_eq!(bundled_report.runtime_healthy, Some(false));

        fs::remove_file(bundled).unwrap();
        fs::write(root.join(package::PACKAGE_VERSION), "0.2.1\n").unwrap();
        hooks[0]["trustStatus"] = json!("untrusted");
        let problem = scan(&skills, &hooks);
        let path = problem.runtime.unwrap();
        assert!(path.starts_with(home.join("fstack/runtime/0.2.1")));
        assert_eq!(problem.runtime_present, Some(false));
        assert_eq!(problem.runtime_healthy, None);
        assert_eq!(problem.hooks["sessionStart"], ["untrusted"]);
        assert!(problem
            .issues
            .iter()
            .any(|s| s.contains("launchers/package-version")));
        assert!(problem.issues.iter().any(|s| s.contains("неодобренные")));

        fs::write(root.join(package::PACKAGE_VERSION), "0.2.0\n").unwrap();
        let cache = runtime(&root, home).unwrap();
        executable(&cache, b"native");
        hooks[0]["trustStatus"] = json!("managed");
        let cached = scan(&skills, &hooks);
        assert_eq!(cached.issues.len(), 1);
        assert!(cached.issues[0].contains("проверка запуска"));
        assert_eq!(cached.runtime.unwrap(), cache);
        assert_eq!(cached.runtime_present, Some(true));
        assert_eq!(cached.runtime_healthy, Some(false));
        let undiscovered = scan(&[], &[]);
        assert_eq!(undiscovered.package.unwrap(), root.canonicalize().unwrap());
        assert_eq!(undiscovered.enabled_skills, Some(0));
        assert!(undiscovered.issues.iter().any(|s| s.contains("review")));
        assert!(undiscovered
            .issues
            .iter()
            .any(|s| s.contains("обработчиков")));
        let wrong = [json!({"name":"fstack:review","path":config,"enabled":true})];
        assert!(scan(&wrong, &hooks)
            .issues
            .iter()
            .any(|s| s.contains("путь к инструкции")));
    }

    #[test]
    fn matches_installed_plugin_by_its_marketplace_id() {
        let elsewhere = json!({"marketplaces":[{"plugins":[{"id":"fstack@another"}]}]});
        assert!(plugin(&elsewhere).unwrap().is_none());
        let listing = json!({"marketplaces":[{"plugins":[{"id":"fstack@fstack","localVersion":"0.1.5","version":"0.2.0","enabled":false}]}]});
        let item = plugin(&listing).unwrap().unwrap();
        assert_eq!(item["localVersion"], "0.1.5");
        assert_eq!(item["enabled"], false);
        let unavailable =
            json!({"marketplaces":[],"marketplaceLoadErrors":[{"message":"offline"}]});
        assert!(plugin(&unavailable).is_err());
    }

    #[test]
    fn disabled_plugin_uses_the_active_cache_directory() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("plugins/cache/fstack/fstack");
        for number in ["0.2.0", "0.10.0", "0.1.5"] {
            fs::create_dir_all(cache.join(number)).unwrap();
        }
        assert!(cached_package(temp.path()).unwrap().ends_with("0.10.0"));
        fs::create_dir(cache.join("local")).unwrap();
        assert!(cached_package(temp.path()).unwrap().ends_with("local"));
    }
}
