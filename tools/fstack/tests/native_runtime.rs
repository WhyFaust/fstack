use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn copy_package(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap();
        if [".git", "target", "bin", "dist", "__pycache__"]
            .iter()
            .any(|n| name == *n)
        {
            continue;
        }
        let destination = to.join(name);
        if path.is_dir() {
            copy_package(&path, &destination);
        } else {
            fs::copy(&path, &destination).unwrap();
        }
    }
}

struct Runtime {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    binary: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Runtime {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().canonicalize().unwrap();
        let root = directory.join("relocated-package");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        copy_package(source, &root);
        fs::create_dir(root.join("bin")).unwrap();
        let binary = root.join("bin").join(if cfg!(windows) {
            "fstack.exe"
        } else {
            "fstack"
        });
        fs::copy(env!("CARGO_BIN_EXE_fstack"), &binary).unwrap();
        let home = directory.join("profile");
        let project = directory.join("project");
        fs::create_dir(&project).unwrap();
        Self {
            _temporary: temporary,
            root,
            binary,
            home,
            project,
        }
    }

    fn hook(&self, handler: &Value, input: Option<Value>) -> Value {
        let mut command = if cfg!(windows) {
            let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let mut command = Command::new(powershell);
            command
                .args(["-NoProfile", "-NonInteractive", "-Command"])
                .arg(handler["commandWindows"].as_str().unwrap());
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", handler["command"].as_str().unwrap()])
                .env("PATH", "");
            command
        };
        command.env("PLUGIN_ROOT", &self.root);
        serde_json::from_str(&self.run(&mut command, input)).unwrap()
    }

    fn call(&self, args: &[&str], input: Option<Value>) -> String {
        let mut command = Command::new(&self.binary);
        command
            .arg("--package")
            .arg(&self.root)
            .args(args)
            .env("PATH", "");
        self.run(&mut command, input)
    }

    fn run(&self, command: &mut Command, input: Option<Value>) -> String {
        let description = format!("{command:?}");
        let mut child = command
            .env("CODEX_HOME", &self.home)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("config"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("LOCALAPPDATA", self.home.join("cache"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .current_dir(&self.project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{description}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn json(&self, args: &[&str], input: Option<Value>) -> Value {
        serde_json::from_str(&self.call(args, input)).unwrap()
    }
}

#[cfg(windows)]
#[test]
fn windows_shells_preserve_hook_directory_and_utf8() {
    use std::os::windows::process::CommandExt;
    let mut runtime = Runtime::new();
    let base = runtime._temporary.path();
    let package = base.join("Пакет & пример [1]");
    fs::rename(&runtime.root, &package).unwrap();
    runtime.binary = package.join("bin/fstack.exe");
    runtime.root = package;
    runtime.project = base.join("Проект & пример [2]");
    fs::create_dir(&runtime.project).unwrap();
    let vault = base.join("vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    runtime.json(
        &[
            "memory",
            "init",
            "--repo",
            runtime.project.to_str().unwrap(),
            "--vault",
            vault.to_str().unwrap(),
            "--project",
            "test",
            "--mode",
            "auto",
        ],
        None,
    );
    let known=runtime.json(&["memory","remember"],Some(json!({"kind":"preference","title":"Кварцевит","body":"Кварцевит служит контрольным словом."})));
    let id = known["id"].as_str().unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(runtime.root.join("hooks/hooks.json")).unwrap()).unwrap();
    let event = "UserPromptSubmit";
    let launcher = manifest["hooks"][event][0]["hooks"][0]["commandWindows"]
        .as_str()
        .unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    for name in ["cmd", "powershell"] {
        let mut command = if name == "cmd" {
            let mut cmd = Command::new(system.join("cmd.exe"));
            cmd.args(["/D", "/S", "/C"])
                .raw_arg(format!(r#""{launcher}""#));
            cmd
        } else {
            let mut ps = Command::new(system.join("WindowsPowerShell/v1.0/powershell.exe"));
            ps.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                launcher,
            ]);
            ps
        };
        let mut child = command
            .current_dir(&runtime.project)
            .env("CODEX_HOME", &runtime.home)
            .env("PLUGIN_ROOT", &runtime.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = json!({"session_id":name,"turn_id":"turn","cwd":runtime.project,"hook_event_name":event,"permission_mode":"default","model":"fixture","transcript_path":null,"prompt":"/faust-mode Кварцевит"});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let response: Value = serde_json::from_slice(&result.stdout).unwrap_or_else(|err| {
            panic!("{name}: {err}, {}", String::from_utf8_lossy(&result.stdout))
        });
        let context = response["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {response}"));
        assert!(
            context.contains(id) && context.contains("Память"),
            "{name}: {context}"
        );
    }
}

#[test]
fn relocated_native_package_runs_without_python_cargo_or_path_tools() {
    let runtime = Runtime::new();
    runtime.call(&["check-package", runtime.root.to_str().unwrap()], None);
    assert_eq!(
        runtime.json(&["migrate-profile", "--apply"], None)["status"],
        "unchanged"
    );
    assert!(!runtime.home.exists());
    let vault = runtime.root.parent().unwrap().join("vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    runtime.json(
        &[
            "memory",
            "init",
            "--vault",
            vault.to_str().unwrap(),
            "--repo",
            runtime.project.to_str().unwrap(),
            "--project",
            "native",
            "--mode",
            "auto",
        ],
        None,
    );
    let saved = runtime.json(&["memory", "remember"], Some(json!({"kind":"decision","title":"Heliotrope runtime","body":"Use the bundled native runtime for this project."})));
    let id = saved["id"].as_str().unwrap();
    let hits = runtime.json(&["memory", "search", "Heliotrope"], None);
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["id"], id);
    runtime.json(&["memory", "forget", id], None);
    assert!(runtime
        .json(&["memory", "search", "Heliotrope"], None)
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn correction_is_captured_during_a_vault_outage_then_recalled_from_a_new_chat() {
    let mut runtime = Runtime::new();
    let manifest = runtime.root.join(".codex-plugin/plugin.json");
    let manifest: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    let cache = runtime
        .home
        .join("fstack/runtime")
        .join(manifest["version"].as_str().unwrap())
        .join(platform_target());
    fs::create_dir_all(&cache).unwrap();
    let cached = cache.join(runtime.binary.file_name().unwrap());
    fs::rename(&runtime.binary, &cached).unwrap();
    runtime.binary = cached;
    let vault = runtime.root.parent().unwrap().join("vault");
    let offline = vault.with_file_name("offline");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    runtime.json(
        &[
            "memory",
            "init",
            "--vault",
            vault.to_str().unwrap(),
            "--repo",
            runtime.project.to_str().unwrap(),
            "--project",
            "native",
        ],
        None,
    );
    let event = |name: &str, session: &str, turn: &str, fields: Value| {
        let mut payload = json!({"hook_event_name":name, "session_id":session, "turn_id":turn,
            "cwd":runtime.project, "model":"codex", "permission_mode":"default",
            "transcript_path":null});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        runtime.json(&["memory", "hook", name], Some(payload))
    };
    let stop = || json!({"stop_hook_active":false, "last_assistant_message":"Готово."});
    fs::rename(&vault, &offline).unwrap();
    let prompt =
        json!({"prompt":"Поправка на будущее. Для проверок проекта используй uv run pytest."});
    let stage = |turn: &str, predecessor: Option<&str>, body: &str| {
        let issued = event("UserPromptSubmit", "one", turn, prompt.clone());
        let context = issued["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        let pattern = regex::Regex::new(
            r#"((?:& )?"[^"]+" --package "[^"]+" memory stage --turn [0-9a-f]{64}\.[0-9a-f]{32}) --correction"#,
        )
        .unwrap();
        let prefix = &pattern.captures(context).unwrap()[1];
        let mut command = format!(r#"{prefix} --correction "Запуск тестов" "{body}""#);
        if let Some(id) = predecessor {
            command.push_str(&format!(" --supersedes {id}"));
        }
        let mut shell = if cfg!(windows) {
            let mut exe = Command::new("powershell");
            exe.args(["-NoProfile", "-NonInteractive", "-Command"]);
            exe
        } else {
            let mut exe = Command::new("/bin/sh");
            exe.arg("-c");
            exe
        };
        let output = shell
            .arg(&command)
            .current_dir(&runtime.project)
            .env("CODEX_HOME", &runtime.home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt = String::from_utf8(output.stdout).unwrap();
        assert_eq!(receipt.trim(), "{\"staged\": true}");
        event(
            "PostToolUse",
            "one",
            turn,
            json!({"tool_name":"Bash",
              "tool_use_id":"call-native-stage", "tool_input":{"command":command},
              "tool_response":receipt}),
        );
    };
    let rule = "В разделе «Проверки» запускай uv run pytest.";
    stage("offline", None, rule);
    assert!(event("Stop", "one", "offline", stop())["systemMessage"].is_string());
    let diagnostics = || runtime.json(&["memory", "diagnostics"], None);
    assert_eq!(diagnostics()["pending"], 1);
    fs::rename(&offline, &vault).unwrap();
    let search = || runtime.json(&["memory", "search", "pytest"], None);
    assert_eq!(search(), json!([]));
    event("UserPromptSubmit", "two", "on", json!({"prompt":"pytest"}));
    event("Stop", "two", "on", stop());
    assert_eq!(search(), json!([]));
    event("Stop", "one", "offline", stop());
    let entry = search()[0].clone();
    assert_eq!(entry["body"], rule);
    assert_eq!(entry["kind"], "correction");
    assert_eq!(entry["session_id"], "one");
    assert_eq!(entry["turn_id"], "offline");
    assert_eq!(diagnostics()["pending"], 0);
    stage("repeat", None, rule);
    event("Stop", "one", "repeat", stop());
    assert_eq!(search().as_array().unwrap().len(), 1);
    let old_id = entry["id"].as_str().unwrap();
    let new_rule = "Для проверок проекта используй uv run pytest -q.";
    stage("revised", Some(old_id), new_rule);
    event("Stop", "one", "revised", stop());
    let latest = search();
    assert_eq!(latest.as_array().unwrap().len(), 1);
    assert_eq!(latest[0]["body"], new_rule);
    assert_eq!(latest[0]["supersedes"], old_id);
    let recall = event(
        "UserPromptSubmit",
        "two",
        "question",
        json!({"prompt":"pytest"}),
    );
    assert!(recall["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains(latest[0]["id"].as_str().unwrap()));
    let context = recall["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains(" show ID"));
    let installed_skill = regex::Regex::new(
        r#"((?:[A-Za-z]:[\\/]|\\\\|/)[^"\n]*?[\\/]skills[\\/]use-memory[\\/]SKILL\.md)"#,
    )
    .unwrap()
    .captures(context)
    .and_then(|m| m.get(1))
    .expect("the hook supplies a path to the installed memory skill")
    .as_str();
    assert_eq!(
        Path::new(installed_skill).canonicalize().unwrap(),
        runtime
            .root
            .join("skills/use-memory/SKILL.md")
            .canonicalize()
            .unwrap()
    );
    let (command, token) = regex::Regex::new(
        r#"((?:& )?"[^"]+" --package "[^"]+" memory stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})) --correction"#,
    )
    .unwrap()
    .captures(context)
    .map(|m| (format!("{} --skip", &m[1]), m[2].to_owned()))
    .unwrap();
    let receipt = runtime.call(&["memory", "stage", "--turn", &token, "--skip"], None);
    event(
        "PostToolUse",
        "two",
        "question",
        json!({"tool_name":"Bash","tool_use_id":"call-skip","tool_input":{"command":command},"tool_response":receipt}),
    );
    assert_eq!(event("Stop", "two", "question", stop()), json!({}));
    assert_eq!(search(), latest);
}

#[test]
fn relocated_runtime_discovers_obsidian_registry_and_creates_the_project_on_demand() {
    first_ordinary_turn(true);
    first_ordinary_turn(false);
}

fn first_ordinary_turn(save_correction: bool) {
    let runtime = Runtime::new();
    let vault = runtime.root.parent().unwrap().join("My Work Vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    let config = if cfg!(target_os = "macos") {
        runtime.home.join("Library/Application Support")
    } else {
        runtime.home.join("config")
    }
    .join("obsidian");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("obsidian.json"),
        json!({"vaults":{"local":{"path":vault,"ts":10}}}).to_string(),
    )
    .unwrap();
    assert_eq!(
        runtime.json(&["memory", "vault"], None),
        json!({"vault":vault,"source":"obsidian"})
    );
    assert_eq!(
        runtime.json(&["memory", "status"], None),
        json!({"enabled":false})
    );
    assert!(!vault.join("Codex Memory").exists());
    let event = |name: &str, turn: &str, fields: Value| {
        let mut payload = json!({"hook_event_name":name,"session_id":"ordinary","turn_id":turn,
              "cwd":runtime.project,"model":"codex","permission_mode":"default","transcript_path":null});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        runtime.json(&["memory", "hook", name], Some(payload))
    };
    let prompt = json!({"prompt":"Для проверок этого проекта всегда используй uv run pytest."});
    assert_eq!(
        event(
            "UserPromptSubmit",
            "explicit-plan",
            json!({"prompt":"План","collaboration_mode":"plan"})
        ),
        json!({})
    );
    assert!(!vault.join("Codex Memory").exists());
    let stop = || json!({"stop_hook_active":false,"last_assistant_message":"Готово."});
    let steps = event("UserPromptSubmit", "implicit-plan", prompt.clone());
    assert!(steps["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("ensure --turn"));
    assert_eq!(event("Stop", "implicit-plan", stop()), json!({}));
    assert!(!vault.join("Codex Memory").exists());

    let ordinary = event("UserPromptSubmit", "first", prompt);
    let context = ordinary["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let ensure =
        regex::Regex::new(r#"((?:& )?"[^"]+" memory ensure --turn ([0-9a-f]{64}\.[0-9a-f]{32}))"#)
            .unwrap()
            .captures(context)
            .unwrap();
    let command = &ensure[1];
    let token = &ensure[2];
    let receipt = runtime.call(&["memory", "ensure", "--turn", token], None);
    let post = |cmd: &str, reply: &str| json!({"tool_name":"Bash","tool_use_id":"call-test","tool_input":{"command":cmd},"tool_response":reply});
    assert_eq!(
        event(
            "PostToolUse",
            "first",
            post(&format!("{command}; true"), &receipt)
        ),
        json!({})
    );
    assert_eq!(
        event("PostToolUse", "implicit-plan", post(command, &receipt)),
        json!({})
    );
    assert!(!vault.join("Codex Memory").exists());
    let issued = event("PostToolUse", "first", post(command, &receipt));
    assert_eq!(event("Stop", "first", stop())["decision"], "block");
    let instructions = issued["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let stage = regex::Regex::new(
        r#"((?:& )?"[^"]+" memory stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})) --correction"#,
    )
    .unwrap()
    .captures(instructions)
    .unwrap();
    let rule = "Для проверок этого проекта всегда используй uv run pytest.";
    let (command, receipt) = if save_correction {
        let command = format!("{} --correction 'Тесты' '{rule}'", &stage[1]);
        let receipt = runtime.call(
            &[
                "memory",
                "stage",
                "--turn",
                &stage[2],
                "--correction",
                "Тесты",
                rule,
            ],
            None,
        );
        (command, receipt)
    } else {
        let command = format!("{} --skip", &stage[1]);
        let receipt = runtime.call(&["memory", "stage", "--turn", &stage[2], "--skip"], None);
        (command, receipt)
    };
    event("PostToolUse", "first", post(&command, &receipt));
    assert_eq!(event("Stop", "first", stop()), json!({}));
    let records = runtime.json(&["memory", "search", "pytest"], None);
    if save_correction {
        assert_eq!(records[0]["body"], rule);
    } else {
        assert_eq!(records, json!([]));
    }
    let connected = runtime.json(&["memory", "status"], None);
    assert_eq!(connected["mode"], "auto");
    assert_eq!(runtime.json(&["memory", "status"], None), connected);
    assert_eq!(runtime.json(&["memory", "ensure"], None), connected);
    assert!(vault
        .join("Codex Memory/Projects")
        .join(connected["project"].as_str().unwrap())
        .join("Records")
        .is_dir());
}

fn platform_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        pair => panic!("unsupported test platform: {pair:?}"),
    }
}

#[test]
fn startup_from_main_uses_the_cached_runtime_and_retires_only_owned_roles() {
    use sha2::{Digest, Sha256};

    let runtime = Runtime::new();
    let manifest = runtime.root.join(".codex-plugin/plugin.json");
    let package: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let version = package["version"].as_str().unwrap();
    let cache = runtime
        .home
        .join("fstack/runtime")
        .join(version)
        .join(platform_target());
    fs::create_dir_all(&cache).unwrap();
    let binary = cache.join(runtime.binary.file_name().unwrap());
    fs::rename(&runtime.binary, &binary).unwrap();

    let hooks: Value =
        serde_json::from_slice(&fs::read(runtime.root.join("hooks/hooks.json")).unwrap()).unwrap();
    let bootstrap = &hooks["hooks"]["SessionStart"][0]["hooks"][1];
    assert_eq!(bootstrap["async"], true);
    let invoke = || runtime.hook(bootstrap, None);

    assert_eq!(invoke(), json!({}));
    assert!(!runtime.home.join("agents").exists());
    assert!(!runtime.home.join("fstack/profile.json").exists());
    assert!(!runtime.home.join("fstack/backups").exists());

    let agents = runtime.home.join("agents");
    fs::create_dir(&agents).unwrap();
    let owned = b"name = \"auditor\"\n";
    fs::write(agents.join("auditor.toml"), owned).unwrap();
    let edited = b"name = \"coder\"\nmodel = \"my-model\"\n";
    fs::write(agents.join("coder.toml"), edited).unwrap();
    let state_path = runtime.home.join("fstack/profile.json");
    fs::write(
        &state_path,
        json!({
            "schema": 2,
            "version": "9.0.0",
            "files": {
                "agents/auditor.toml": format!("{:x}", Sha256::digest(owned)),
                "agents/coder.toml": format!("{:x}", Sha256::digest(b"old coder")),
            }
        })
        .to_string(),
    )
    .unwrap();
    let response = invoke();
    assert!(response["systemMessage"]
        .as_str()
        .unwrap()
        .contains("agents/coder.toml"));
    assert!(!agents.join("auditor.toml").exists());
    assert_eq!(fs::read(agents.join("coder.toml")).unwrap(), edited);
    assert!(!state_path.exists());
    assert_eq!(invoke(), json!({}));
}

#[test]
fn prompt_hook_reports_cold_runtime_and_accepts_activation_after_installation() {
    let runtime = Runtime::new();
    let hooks: Value =
        serde_json::from_slice(&fs::read(runtime.root.join("hooks/hooks.json")).unwrap()).unwrap();
    let handler = &hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0];
    let prompt = |turn: &str, text: &str| {
        runtime.hook(handler, Some(json!({
            "hook_event_name": "UserPromptSubmit", "cwd": runtime.project,
            "session_id": "startup", "turn_id": turn, "collaboration_mode": "plan", "prompt": text
        })))
    };
    let bundled = runtime._temporary.path().join("runtime");
    fs::rename(&runtime.binary, &bundled).unwrap();
    let unavailable = prompt("before", "/faust-mode");
    assert!(unavailable["systemMessage"]
        .as_str()
        .unwrap()
        .contains("/faust-mode"));
    assert!(unavailable.get("decision").is_none());
    assert!(!runtime.home.exists());

    let manifest: Value =
        serde_json::from_slice(&fs::read(runtime.root.join(".codex-plugin/plugin.json")).unwrap())
            .unwrap();
    let cache = runtime
        .home
        .join("fstack/runtime")
        .join(manifest["version"].as_str().unwrap())
        .join(platform_target());
    fs::create_dir_all(&cache).unwrap();
    fs::rename(bundled, cache.join(runtime.binary.file_name().unwrap())).unwrap();
    let skill = runtime.root.join("skills/faust-mode/SKILL.md");
    for (turn, text) in [("retry", "/faust-mode"), ("next", "continue")] {
        let result = prompt(turn, text);
        assert!(result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains(skill.to_str().unwrap()));
    }
}

#[cfg(unix)]
#[test]
fn launcher_verifies_the_selected_release_asset_before_installing() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    let runtime = Runtime::new();
    let target = platform_target();
    let version = "9.123.456";
    fs::write(
        runtime.root.join(".codex-plugin/plugin.json"),
        format!("{{\n  \"version\": \"{version}\"\n}}\n"),
    )
    .unwrap();
    fs::remove_file(&runtime.binary).unwrap();
    let fixtures = runtime._temporary.path().join("fixtures");
    fs::create_dir(&fixtures).unwrap();
    let asset = format!("fstack-{version}-{target}");
    let bytes = b"fixture runtime";
    fs::write(fixtures.join(&asset), bytes).unwrap();
    let curl = fixtures.join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
while [ "${1#https://}" = "$1" ]; do
    [ "$#" -gt 0 ] || exit 2
    shift
done
case "$1" in
    "https://github.com/WhyFaust/fstack/releases/download/v$FSTACK_TEST_VERSION/$FSTACK_TEST_ASSET"*) ;;
    *) exit 3 ;;
esac
[ "$2" = --output ] || exit 4
cp "$FSTACK_TEST_FIXTURES/${1##*/}" "$3"
"#,
    )
    .unwrap();
    fs::set_permissions(curl, fs::Permissions::from_mode(0o700)).unwrap();

    let install = || {
        Command::new("/bin/sh")
            .arg(runtime.root.join("launchers/fstack.sh"))
            .arg("--install-runtime")
            .env("PATH", format!("{}:/usr/bin:/bin", fixtures.display()))
            .env("CODEX_HOME", &runtime.home)
            .env("FSTACK_TEST_FIXTURES", &fixtures)
            .env("FSTACK_TEST_VERSION", version)
            .env("FSTACK_TEST_ASSET", &asset)
            .output()
            .unwrap()
    };
    let checksum = fixtures.join(format!("{asset}.sha256"));
    let cache = runtime
        .home
        .join("fstack/runtime")
        .join(version)
        .join(target);
    fs::write(&checksum, "bad checksum\n").unwrap();
    assert!(!install().status.success());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);

    fs::write(&checksum, format!("{:x}  {asset}\n", Sha256::digest(bytes))).unwrap();
    assert!(install().status.success());
    assert_eq!(fs::read(cache.join("fstack")).unwrap(), bytes);
    fs::remove_file(fixtures.join("curl")).unwrap();
    assert!(install().status.success());
}
