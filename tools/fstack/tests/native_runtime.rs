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

#[cfg(unix)]
fn fixture_binary() -> std::sync::Arc<tempfile::TempPath> {
    use std::sync::{Arc, Mutex, Weak};

    static BINARY: Mutex<Weak<tempfile::TempPath>> = Mutex::new(Weak::new());
    let mut shared = BINARY.lock().unwrap();
    if let Some(staged) = shared.upgrade() {
        return staged;
    }
    let path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    fs::copy(env!("CARGO_BIN_EXE_fstack"), &path).unwrap();
    let staged = Arc::new(path);
    *shared = Arc::downgrade(&staged);
    staged
}

struct Runtime {
    #[cfg(unix)]
    _fixture_binary: std::sync::Arc<tempfile::TempPath>,
    _temporary: tempfile::TempDir,
    root: PathBuf,
    binary: PathBuf,
    codex_home: PathBuf,
    user_home: PathBuf,
    project: PathBuf,
}

impl Runtime {
    fn new() -> Self {
        Self::from_temp(tempfile::tempdir().unwrap())
    }

    fn from_temp(temporary: tempfile::TempDir) -> Self {
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
        #[cfg(unix)]
        let fixture_binary = fixture_binary();
        #[cfg(unix)]
        if fs::hard_link(&*fixture_binary, &binary).is_err() {
            fs::copy(&*fixture_binary, &binary).unwrap();
        }
        #[cfg(windows)]
        fs::copy(env!("CARGO_BIN_EXE_fstack"), &binary).unwrap();
        let codex_home = directory.join("codex");
        let user_home = directory.join("user");
        fs::create_dir(&user_home).unwrap();
        let project = directory.join("project");
        fs::create_dir(&project).unwrap();
        Self {
            #[cfg(unix)]
            _fixture_binary: fixture_binary,
            _temporary: temporary,
            root,
            binary,
            codex_home,
            user_home,
            project,
        }
    }

    fn runtime_cache(&self) -> PathBuf {
        let manifest: Value =
            serde_json::from_slice(&fs::read(self.root.join(".codex-plugin/plugin.json")).unwrap())
                .unwrap();
        let id = fs::read_to_string(self.root.join("launchers/runtime-id")).unwrap();
        self.codex_home
            .join("fstack/runtime")
            .join(manifest["version"].as_str().unwrap())
            .join(id.trim_end())
            .join(platform_target())
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
            .env("CODEX_HOME", &self.codex_home)
            .env("HOME", &self.user_home)
            .env("USERPROFILE", &self.user_home)
            .env("APPDATA", self.user_home.join("config"))
            .env("XDG_CONFIG_HOME", self.user_home.join("config"))
            .env("LOCALAPPDATA", self.user_home.join("cache"))
            .env("XDG_CACHE_HOME", self.user_home.join("cache"))
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

#[test]
fn explicit_profile_and_cache_work_without_a_home_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let profile = root.join("profile");
    let cache = root.join("cache");
    let vault = root.join("vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_fstack"))
        .arg("--package")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["memory", "vault", "--set"])
        .arg(&vault)
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env("CODEX_HOME", &profile)
        .env(
            if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "XDG_CACHE_HOME"
            },
            &cache,
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(profile.join("fstack/memory/vault.json").is_file());
    assert!(cache.join("fstack/memory-locks").is_dir());
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
            .env("CODEX_HOME", &runtime.codex_home)
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
    assert!(!runtime.codex_home.exists());
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
#[cfg(unix)]
fn exec_command_skip_with_literal_paths_finishes_on_first_stop() {
    let mut runtime = Runtime::from_temp(tempfile::tempdir_in("/tmp").unwrap());
    let cache = runtime.runtime_cache();
    fs::create_dir_all(&cache).unwrap();
    let cached = cache.join(runtime.binary.file_name().unwrap());
    fs::rename(&runtime.binary, &cached).unwrap();
    runtime.binary = cached;
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
            "exec",
        ],
        None,
    );
    let hooks: Value =
        serde_json::from_slice(&fs::read(runtime.root.join("hooks/hooks.json")).unwrap()).unwrap();
    let event = |name: &str, fields: Value| {
        let mut payload = json!({"hook_event_name":name,"session_id":"exec-session","turn_id":"skip-turn",
            "cwd":runtime.project,"permission_mode":"default","transcript_path":null});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        runtime.hook(&hooks["hooks"][name][0]["hooks"][0], Some(payload))
    };
    let issued = event("UserPromptSubmit", json!({"prompt":"Объясни результат"}));
    let context = issued["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let token = regex::Regex::new(r"memory stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})")
        .unwrap()
        .captures(context)
        .unwrap()
        .get(1)
        .unwrap()
        .as_str();
    let command = format!(
        "{} --package {} memory stage --turn {token} --skip",
        runtime.binary.display(),
        runtime.root.display()
    );
    let receipt = runtime.run(Command::new("/bin/sh").args(["-c", &command]), None);
    assert_eq!(receipt.trim(), "{\"staged\": true}");
    let post = |command: &str| {
        json!({"tool_name":"Bash","tool_use_id":"call-exec",
        "tool_input":{"command":command},"tool_response":receipt})
    };
    event("PostToolUse", post(&format!("{command}; true")));
    assert_eq!(runtime.json(&["memory", "diagnostics"], None)["staged"], 0);
    event("PostToolUse", post(&command));
    assert_eq!(runtime.json(&["memory", "diagnostics"], None)["staged"], 1);
    assert_eq!(event("Stop", json!({"stop_hook_active":false})), json!({}));
    assert_eq!(
        runtime.json(&["memory", "diagnostics"], None)["phase"],
        "skipped"
    );
}

#[test]
fn correction_is_captured_during_a_vault_outage_then_recalled_from_a_new_chat() {
    let mut runtime = Runtime::new();
    let package = runtime.root.with_file_name("package $HOME's `docs`");
    fs::rename(&runtime.root, &package).unwrap();
    runtime.binary = package
        .join("bin")
        .join(runtime.binary.file_name().unwrap());
    runtime.root = package;
    runtime.codex_home = runtime.codex_home.with_file_name("profile $value's `data`");
    let cache = runtime.runtime_cache();
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
    let stage = |turn: &str, predecessor: Option<&str>, body: &str, single_quoted_paths| {
        let issued = event("UserPromptSubmit", "one", turn, prompt.clone());
        let context = issued["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        let pattern = regex::Regex::new(
            r#"((?:& )?"[^"]+" --package "[^"]+" memory stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})) --correction"#,
        )
        .unwrap();
        let captures = pattern.captures(context).unwrap();
        let mut command = if cfg!(windows) && single_quoted_paths {
            let literal_path = |path: &Path| {
                path.canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\'', "''")
            };
            format!(
                "& '{}' --package '{}' memory stage --turn {} --correction 'Запуск тестов' '{body}'",
                literal_path(&runtime.binary),
                literal_path(&runtime.root),
                &captures[2]
            )
        } else {
            format!(r#"{} --correction "Запуск тестов" "{body}""#, &captures[1])
        };
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
            .env("CODEX_HOME", &runtime.codex_home)
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
    stage("offline", None, rule, false);
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
    stage("repeat", None, rule, false);
    event("Stop", "one", "repeat", stop());
    assert_eq!(search().as_array().unwrap().len(), 1);
    let old_id = entry["id"].as_str().unwrap();
    let new_rule = "Для проверок проекта используй uv run pytest -q.";
    stage("revised", Some(old_id), new_rule, true);
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
    assert!(context.contains("Faust-mode выключен"));
    assert!(!context.contains("SKILL.md"));
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
fn codex_task_records_a_verified_project_decision_without_user_input() {
    let runtime = Runtime::new();
    let vault = runtime.root.parent().unwrap().join("vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    runtime.json(&["memory", "vault", "--set", vault.to_str().unwrap()], None);
    let session = "00000000-0000-7000-8000-000000000001";
    let turn = "00000000-0000-7000-8000-000000000002";
    let parent = "00000000-0000-7000-8000-000000000003";
    let follow_up = "00000000-0000-7000-8000-000000000004";
    let plan = "00000000-0000-7000-8000-000000000005";
    let switch_to_plan = "00000000-0000-7000-8000-000000000006";
    let inter_agent = "00000000-0000-7000-8000-000000000007";
    let dir = runtime.codex_home.join("sessions/2026/01/01");
    fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"));
    let mut rollout = fs::File::create(&transcript).unwrap();
    let meta = json!({"type":"session_meta","payload":{"id":session,"source":"vscode","originator":"Codex Desktop"}});
    writeln!(rollout, "{meta}").unwrap();
    let write_turn = |rollout: &mut fs::File, turn: &str, trigger: &str, mode: &str| {
        for item in [
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn,"root_turn_id":parent,
               "collaboration_mode_kind":"default","turn_attribution":{"turn_id":turn,"turn_trigger":trigger,
               "parent_turn_id":parent,"root_turn_id":parent,"initiating_agent_path":null}}}),
            json!({"type":"turn_context","payload":{"turn_id":turn,"cwd":runtime.project,"collaboration_mode":{"mode":mode}}}),
            json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FunctionCallOutput"}}}),
        ] {
            writeln!(rollout, "{item}").unwrap();
        }
    };
    write_turn(&mut rollout, turn, "app_tool_create_thread", "default");
    let event = |name: &str, turn: &str, fields: Value| {
        let mut payload = json!({"hook_event_name":name,"session_id":session,"turn_id":turn,
            "cwd":runtime.project,"permission_mode":"default","transcript_path":transcript});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        runtime.json(&["memory", "hook", name], Some(payload))
    };
    let stop =
        |active| json!({"stop_hook_active":active,"last_assistant_message":"Проверил итог."});
    let project_copy = runtime.project.join(transcript.file_name().unwrap());
    fs::copy(&transcript, &project_copy).unwrap();
    let mut untrusted = stop(false);
    untrusted["transcript_path"] = json!(project_copy);
    assert_eq!(event("Stop", turn, untrusted), json!({}));
    assert_eq!(
        runtime.json(&["memory", "status"], None),
        json!({"enabled":false})
    );
    let startup = event("SessionStart", turn, json!({"source":"startup"}));
    assert!(startup["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("memory search"));
    let initial = event("Stop", turn, stop(false));
    assert_eq!(initial["decision"], "block");
    let reason = initial["reason"].as_str().unwrap();
    assert!(!reason.contains("--turn"));
    assert!(!reason.contains("SKILL.md"));
    let prefix =
        regex::Regex::new(r#"((?:& )?"[^"\n]+"(?: --package "[^"\n]+")? memory report) --payload"#)
            .unwrap()
            .captures(reason)
            .unwrap()[1]
            .to_owned();
    let report = json!({"action":"remember","candidate":{"kind":"decision","title":"Кварцевый регламент",
        "body":"Запускай проверку кварцевого регламента до упаковки.","source":"Проверенный результат поручения"}});
    let encoded = runtime.json(&["memory", "pack"], Some(report));
    let encoded = encoded.as_str().unwrap();
    let receipt = runtime.call(&["memory", "report", "--payload", encoded], None);
    assert_eq!(event("Stop", turn, stop(false)), json!({}));
    let post = json!({"tool_name":"Bash","tool_use_id":"call-report",
        "tool_input":{"command":format!("{prefix} --payload {encoded}")},"tool_response":receipt});
    let search = || runtime.json(&["memory", "search", "кварцевого"], None);
    assert_eq!(event("PostToolUse", parent, post.clone()), json!({}));
    assert_eq!(event("PostToolUse", turn, post), json!({}));
    assert_eq!(search(), json!([]));
    assert_eq!(event("Stop", turn, stop(true)), json!({}));
    let records = search();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["title"], "Кварцевый регламент");
    assert_eq!(records[0]["turn_id"], turn);
    assert_eq!(records[0]["session_id"], session);
    assert_eq!(records[0]["status"], "memory");
    let binding = runtime.json(&["memory", "status"], None);
    let path = vault
        .join("Codex Memory/Projects")
        .join(binding["project"].as_str().unwrap())
        .join("Records")
        .join(format!("{}.md", records[0]["id"].as_str().unwrap()));
    assert!(fs::read_to_string(path)
        .unwrap()
        .contains("Запускай проверку кварцевого регламента"));
    assert_eq!(event("Stop", turn, stop(false)), json!({}));
    assert_eq!(search(), records);

    write_turn(&mut rollout, follow_up, "app_tool_send_message", "default");
    assert_eq!(event("Stop", follow_up, stop(false))["decision"], "block");
    let receipt = runtime.call(&["memory", "report", "--skip"], None);
    let skip = json!({"tool_name":"Bash","tool_use_id":"call-skip",
        "tool_input":{"command":format!("{prefix} --skip")},"tool_response":receipt});
    assert_eq!(event("PostToolUse", follow_up, skip.clone()), json!({}));
    assert_eq!(event("Stop", follow_up, stop(true)), json!({}));
    assert_eq!(
        runtime.json(&["memory", "diagnostics"], None)["phase"],
        "skipped"
    );
    assert_eq!(search(), records);

    write_turn(&mut rollout, inter_agent, "remote_ios", "default");
    let wake =
        json!({"type":"inter_agent_communication_metadata","payload":{"trigger_turn":false}});
    let message = json!({"type":"response_item","payload":{"type":"agent_message",
        "author":"/root/worker","recipient":"/root","content":[]}});
    writeln!(rollout, "{wake}\n{message}").unwrap();
    assert_eq!(event("Stop", inter_agent, stop(false))["decision"], "block");
    assert_eq!(event("PostToolUse", inter_agent, skip.clone()), json!({}));
    assert_eq!(event("Stop", inter_agent, stop(true)), json!({}));
    assert_eq!(search(), records);

    write_turn(&mut rollout, plan, "app_tool_send_message", "plan");
    assert_eq!(event("Stop", plan, stop(false)), json!({}));

    write_turn(
        &mut rollout,
        switch_to_plan,
        "app_tool_send_message",
        "default",
    );
    assert_eq!(
        event("Stop", switch_to_plan, stop(false))["decision"],
        "block"
    );
    assert_eq!(event("PostToolUse", switch_to_plan, skip), json!({}));
    let plan_context = json!({"type":"turn_context","payload":{"turn_id":switch_to_plan,
        "cwd":runtime.project,"collaboration_mode":{"mode":"plan"}}});
    writeln!(rollout, "{plan_context}").unwrap();
    assert_eq!(event("Stop", switch_to_plan, stop(true)), json!({}));
    assert_eq!(
        runtime.json(&["memory", "diagnostics"], None)["phase"],
        "cancelled"
    );
    assert_eq!(search(), records);
}

#[test]
fn desktop_hooks_use_recorded_plan_mode_and_capture_native_agent_decisions() {
    let runtime = Runtime::new();
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
        ],
        None,
    );
    let parent = "00000000-0000-7000-8000-000000000011";
    let child = "00000000-0000-7000-8000-000000000012";
    let parent_turn = "00000000-0000-7000-8000-000000000013";
    let child_turn = "00000000-0000-7000-8000-000000000014";
    let plan_turn = "00000000-0000-7000-8000-000000000015";
    let aborted_turn = "00000000-0000-7000-8000-000000000016";
    let next_turn = "00000000-0000-7000-8000-000000000017";
    let dir = runtime.codex_home.join("sessions/2026/01/01");
    fs::create_dir_all(&dir).unwrap();
    let transcript = |session| dir.join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"));
    let write_header = |session, source: Value| {
        let mut file = fs::File::create(transcript(session)).unwrap();
        let meta = json!({"type":"session_meta","payload":{"id":session,"source":source,"originator":"Codex Desktop"}});
        writeln!(file, "{meta}").unwrap();
        file
    };
    let write_turn = |file: &mut fs::File, turn: &str, mode: &str| {
        let start = json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn,"collaboration_mode_kind":mode}});
        let context = json!({"type":"turn_context","payload":{"turn_id":turn,"cwd":runtime.project,
            "collaboration_mode":{"mode":mode}}});
        writeln!(file, "{start}\n{context}").unwrap();
    };
    let run_hook = |name: &str, session, turn, fields: Value| {
        let mut payload = json!({"hook_event_name":name,"session_id":session,"turn_id":turn,
             "cwd":runtime.project,"permission_mode":"default","transcript_path":transcript(session)});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        runtime.json(&["memory", "hook", name], Some(payload))
    };
    let mut root = write_header(parent, json!("vscode"));
    write_turn(&mut root, plan_turn, "plan");
    let plan = run_hook(
        "UserPromptSubmit",
        parent,
        plan_turn,
        json!({"prompt":"Нужен план"}),
    );
    assert!(!plan.to_string().contains("stage --turn"));

    let mut agent = write_header(
        child,
        json!({"subagent":{"thread_spawn":{"parent_thread_id":parent,"depth":1,"agent_path":"/root/worker"}}}),
    );
    write_turn(&mut agent, child_turn, "default");
    let agent_start = run_hook(
        "SubagentStart",
        child,
        child_turn,
        json!({"agent_id":child,"agent_type":"worker"}),
    );
    assert!(agent_start["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("memory search"));
    let agent_stop = |continued| {
        json!({"agent_id":child,"agent_type":"worker",
        "transcript_path":transcript(parent),"agent_transcript_path":transcript(child),
        "last_assistant_message":"Проверено","stop_hook_active":continued})
    };
    assert_eq!(
        run_hook("SubagentStop", child, child_turn, agent_stop(false)),
        json!({})
    );
    write_turn(&mut root, parent_turn, "default");
    let request = run_hook("SubagentStop", child, child_turn, agent_stop(false));
    assert_eq!(request["decision"], "block");
    let prefix =
        regex::Regex::new(r#"((?:& )?"[^"\n]+"(?: --package "[^"\n]+")? memory report) --payload"#)
            .unwrap()
            .captures(request["reason"].as_str().unwrap())
            .unwrap()[1]
            .to_owned();
    let action = json!({"action":"remember","candidate":{"kind":"decision","title":"Гранатовая проверка",
        "body":"Проверяй гранатовый режим при сборке.","source":"Результат теста подагента"}});
    let encoded = runtime.json(&["memory", "pack"], Some(action));
    let encoded = encoded.as_str().unwrap();
    let receipt = runtime.call(&["memory", "report", "--payload", encoded], None);
    let post = json!({"agent_id":child,"agent_type":"worker","tool_name":"Bash","tool_use_id":"agent-report",
        "tool_input":{"command":format!("{prefix} --payload {encoded}")},"tool_response":receipt});
    assert_eq!(run_hook("PostToolUse", child, child_turn, post), json!({}));
    assert_eq!(
        run_hook("SubagentStop", child, child_turn, agent_stop(true)),
        json!({})
    );
    let records = runtime.json(&["memory", "search", "гранатовый"], None);
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["session_id"], child);
    assert_eq!(records[0]["turn_id"], child_turn);
    assert_eq!(
        run_hook("SubagentStop", child, child_turn, agent_stop(true)),
        json!({})
    );
    assert_eq!(
        runtime.json(&["memory", "search", "гранатовый"], None),
        records
    );

    write_turn(&mut agent, aborted_turn, "default");
    assert_eq!(
        run_hook("SubagentStop", child, aborted_turn, agent_stop(false))["decision"],
        "block"
    );
    let operation = json!({"action":"remember","candidate":{"kind":"decision","title":"Отменённая проверка",
       "body":"Исход отменённой проверки.","source":"Результат подагента"}});
    let encoded = runtime.json(&["memory", "pack"], Some(operation));
    let encoded = encoded.as_str().unwrap();
    let receipt = runtime.call(&["memory", "report", "--payload", encoded], None);
    let interrupted_post = json!({"agent_id":child,"agent_type":"worker","tool_name":"Bash",
        "tool_use_id":"aborted-report","tool_input":{"command":format!("{prefix} --payload {encoded}")},
        "tool_response":receipt});
    run_hook("PostToolUse", child, aborted_turn, interrupted_post);
    assert_eq!(runtime.json(&["memory", "diagnostics"], None)["staged"], 1);
    let aborted =
        json!({"type":"event_msg","payload":{"type":"turn_aborted","turn_id":aborted_turn}});
    writeln!(agent, "{aborted}").unwrap();
    write_turn(&mut agent, next_turn, "default");
    assert_eq!(
        run_hook("SubagentStop", child, next_turn, agent_stop(false))["decision"],
        "block"
    );
    let receipt = runtime.call(&["memory", "report", "--skip"], None);
    let skip = json!({"agent_id":child,"agent_type":"worker","tool_name":"Bash","tool_use_id":"next-report",
        "tool_input":{"command":format!("{prefix} --skip")},"tool_response":receipt});
    run_hook("PostToolUse", child, next_turn, skip);
    assert_eq!(
        run_hook("SubagentStop", child, next_turn, agent_stop(true)),
        json!({})
    );
    assert_eq!(
        runtime.json(&["memory", "diagnostics"], None)["phase"],
        "cancelled"
    );
    assert_eq!(
        runtime.json(&["memory", "search", "отменённой"], None),
        json!([])
    );
}

#[test]
fn relocated_runtime_discovers_obsidian_registry_and_creates_the_project_on_demand() {
    first_ordinary_turn(Runtime::new(), true);
    first_ordinary_turn(Runtime::new(), false);
}

#[test]
fn empty_git_directory_does_not_block_memory_capture() {
    let runtime = Runtime::new();
    fs::create_dir(runtime.project.parent().unwrap().join(".git")).unwrap();
    first_ordinary_turn(runtime, true);
}

fn first_ordinary_turn(runtime: Runtime, save_correction: bool) {
    let vault = runtime.root.parent().unwrap().join("My Work Vault");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    let config = if cfg!(target_os = "macos") {
        runtime.user_home.join("Library/Application Support")
    } else {
        runtime.user_home.join("config")
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
    let plan = event(
        "UserPromptSubmit",
        "explicit-plan",
        json!({"prompt":"План","collaboration_mode":"plan"}),
    );
    let plan = plan["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(plan.contains("Faust-mode выключен"));
    assert!(!plan.contains("--turn"));
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
    let instructions = issued["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(!instructions.contains("SKILL.md"));
    let stage = regex::Regex::new(
        r#"((?:& )?"[^"]+" memory stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})) --correction"#,
    )
    .unwrap()
    .captures(instructions)
    .unwrap();
    let rule = "Для проверок этого проекта всегда используй uv run pytest.";
    if save_correction {
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
        event("PostToolUse", "first", post(&command, &receipt));
    }
    assert_eq!(event("Stop", "first", stop()), json!({}));
    let records = runtime.json(&["memory", "search", "pytest"], None);
    if save_correction {
        assert_eq!(records[0]["body"], rule);
    } else {
        assert_eq!(records, json!([]));
        assert_eq!(
            runtime.json(&["memory", "diagnostics"], None)["phase"],
            "skipped"
        );
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
fn startup_uses_the_cached_runtime_without_editing_personal_instructions() {
    let runtime = Runtime::new();
    let cache = runtime.runtime_cache();
    fs::create_dir_all(&cache).unwrap();
    let binary = cache.join(runtime.binary.file_name().unwrap());
    fs::rename(&runtime.binary, &binary).unwrap();

    let hooks: Value =
        serde_json::from_slice(&fs::read(runtime.root.join("hooks/hooks.json")).unwrap()).unwrap();
    let bootstrap = &hooks["hooks"]["SessionStart"][0]["hooks"][0];
    assert_eq!(bootstrap["async"], true);

    assert_eq!(runtime.hook(bootstrap, None), json!({}));
    let instructions = runtime.codex_home.join("AGENTS.md");
    let agents = runtime.codex_home.join("agents");
    assert!(!instructions.exists());
    assert!(!agents.exists());
    fs::write(&instructions, b"# User instructions\n").unwrap();
    let config = runtime.codex_home.join("config.toml");
    fs::write(&config, b"model = \"user-choice\"\n").unwrap();
    fs::create_dir(&agents).unwrap();
    let agent = agents.join("custom.toml");
    fs::write(&agent, b"name = \"custom\"\n").unwrap();

    assert_eq!(runtime.hook(bootstrap, None), json!({}));
    assert_eq!(fs::read(instructions).unwrap(), b"# User instructions\n");
    assert_eq!(fs::read(config).unwrap(), b"model = \"user-choice\"\n");
    assert_eq!(fs::read(agent).unwrap(), b"name = \"custom\"\n");
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
    assert!(!runtime.codex_home.exists());

    let cache = runtime.runtime_cache();
    fs::create_dir_all(&cache).unwrap();
    fs::rename(bundled, cache.join(runtime.binary.file_name().unwrap())).unwrap();
    let id_file = runtime.root.join("launchers/runtime-id");
    let original_id = fs::read(&id_file).unwrap();
    let other_id = if original_id[0] == b'a' { "b" } else { "a" }.repeat(64);
    for id in [format!("{other_id}\n"), format!("{}!\n", &other_id[1..])] {
        fs::write(&id_file, id).unwrap();
        let unavailable = prompt("other-build", "/faust-mode");
        assert!(unavailable["systemMessage"].is_string());
        assert!(unavailable.get("hookSpecificOutput").is_none());
    }
    fs::write(&id_file, original_id).unwrap();
    let skill = runtime.root.join("skills/faust-mode/SKILL.md");
    let catalog = runtime.root.join("docs/skills.md");
    let description = "Сигнал из перемещённой копии";
    let original = fs::read_to_string(&catalog).unwrap();
    let edited = original.replace(
        "[model-check](../skills/model-check/SKILL.md) | ",
        &format!("[model-check](../skills/model-check/SKILL.md) | {description}. "),
    );
    fs::write(&catalog, edited).unwrap();
    for (turn, text) in [("retry", "/faust-mode"), ("next", "continue")] {
        let result = prompt(turn, text);
        let context = result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains(skill.to_str().unwrap()));
        assert_eq!(context.contains(description), turn == "retry");
    }
    fs::remove_file(&catalog).unwrap();
    let result = prompt("missing-catalog", "/faust-mode");
    let context = result["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("Хук не смог прочитать индекс"));
    assert!(context.contains(catalog.to_str().unwrap()));
}

#[cfg(windows)]
#[test]
fn launcher_verifies_the_selected_release_asset_before_installing() {
    use sha2::{Digest, Sha256};

    let runtime = Runtime::new();
    fs::remove_file(&runtime.binary).unwrap();
    let cache = runtime.runtime_cache();
    let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let install = |digest: &str| {
        Command::new(&shell)
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(r#"
                $curl = [IO.Path]::Combine($env:SystemRoot, 'System32', 'curl.exe')
                Set-Item -LiteralPath ('function:' + $curl) -Value {
                    $path = $args[-1]
                    $content = if ($path.EndsWith('.sha256')) {
                        $env:FSTACK_TEST_DIGEST + '  ' + [IO.Path]::GetFileNameWithoutExtension($path)
                    } else { 'fixture runtime' }
                    [IO.File]::WriteAllText($path, $content)
                    $global:LASTEXITCODE = 0
                }
                $PSModuleAutoLoadingPreference = 'None'
                $hook = [IO.Path]::Combine($env:PLUGIN_ROOT, 'hooks', 'memory.ps1')
                & ([scriptblock]::Create([IO.File]::ReadAllText($hook))) Bootstrap
            "#)
            .env("CODEX_HOME", &runtime.codex_home)
            .env("PLUGIN_ROOT", &runtime.root)
            .env("FSTACK_TEST_DIGEST", digest)
            .output()
            .unwrap()
    };
    let rejected = install(&"0".repeat(64));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("runtime checksum mismatch"));
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);

    let bytes = b"fixture runtime";
    let accepted = install(&format!("{:x}", Sha256::digest(bytes)));
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert_eq!(fs::read(cache.join("fstack.exe")).unwrap(), bytes);
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
        json!({"version":version}).to_string(),
    )
    .unwrap();
    fs::remove_file(&runtime.binary).unwrap();
    fs::write(
        runtime.root.join("launchers/package-version"),
        format!("{version}\n"),
    )
    .unwrap();
    let fixtures = runtime._temporary.path().join("fixtures");
    fs::create_dir(&fixtures).unwrap();
    let id_file = runtime.root.join("launchers/runtime-id");
    let first_id = "a".repeat(64);
    let second_id = "b".repeat(64);
    fs::write(&id_file, format!("{first_id}\n")).unwrap();
    let asset = format!("fstack-{version}-{first_id}-{target}");
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

    let install = |asset: &str, path: &std::ffi::OsStr| {
        Command::new("/bin/sh")
            .arg(runtime.root.join("launchers/fstack.sh"))
            .arg("--install-runtime")
            .env("PATH", path)
            .env("CODEX_HOME", &runtime.codex_home)
            .env("FSTACK_TEST_FIXTURES", &fixtures)
            .env("FSTACK_TEST_VERSION", version)
            .env("FSTACK_TEST_ASSET", asset)
            .output()
            .unwrap()
    };
    let checksum = fixtures.join(format!("{asset}.sha256"));
    let cache = runtime.runtime_cache();
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(&cache, "occupied").unwrap();
    let empty_path = install(&asset, std::ffi::OsStr::new(""));
    assert!(!empty_path.status.success());
    assert_ne!(empty_path.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&empty_path.stderr).contains(cache.to_str().unwrap()));
    fs::remove_file(&cache).unwrap();
    fs::write(&checksum, "bad checksum\n").unwrap();
    assert!(!install(&asset, fixtures.as_os_str()).status.success());
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);

    fs::write(&checksum, format!("{:x}  {asset}\n", Sha256::digest(bytes))).unwrap();
    let occupied = cache.join("fstack");
    fs::create_dir(&occupied).unwrap();
    assert!(!install(&asset, fixtures.as_os_str()).status.success());
    fs::remove_dir(occupied).unwrap();
    assert!(install(&asset, fixtures.as_os_str()).status.success());
    assert_eq!(fs::read(cache.join("fstack")).unwrap(), bytes);
    fs::write(&id_file, format!("{second_id}\n")).unwrap();
    let second_asset = format!("fstack-{version}-{second_id}-{target}");
    let second_cache = runtime.runtime_cache().join("fstack");
    assert!(!install(&second_asset, fixtures.as_os_str())
        .status
        .success());
    assert!(!second_cache.exists());
    let second_bytes = b"updated fixture runtime";
    fs::write(fixtures.join(&second_asset), second_bytes).unwrap();
    fs::write(
        fixtures.join(format!("{second_asset}.sha256")),
        format!("{:x}  {second_asset}\n", Sha256::digest(second_bytes)),
    )
    .unwrap();
    assert!(install(&second_asset, fixtures.as_os_str())
        .status
        .success());
    assert_eq!(fs::read(&second_cache).unwrap(), second_bytes);
    assert_eq!(fs::read(cache.join("fstack")).unwrap(), bytes);
    for malformed in [
        first_id.clone(),
        format!("{first_id}\n\n"),
        format!("{}\n", first_id.to_uppercase()),
    ] {
        fs::write(&id_file, malformed).unwrap();
        assert!(!install(&asset, fixtures.as_os_str()).status.success());
    }
    fs::write(&id_file, format!("{first_id}\n")).unwrap();
    fs::remove_file(fixtures.join("curl")).unwrap();
    assert!(install(&asset, fixtures.as_os_str()).status.success());
}
