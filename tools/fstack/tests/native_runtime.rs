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
    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_fstack"))
        .arg("--package")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["hook", "UserPromptSubmit"])
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
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = json!({"hook_event_name":"UserPromptSubmit","cwd":project,"session_id":"profile","turn_id":"turn","prompt":"/faust-mode"});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("включён faust-mode"));
    assert!(profile.join("fstack/coordinator/sessions").is_dir());
    assert!(cache.join("fstack/coordinator-locks").is_dir());
    assert!(!profile.join("fstack/memory").exists());
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
            context.contains("включён faust-mode") && context.contains("Пакет & пример [1]"),
            "{name}: {context}"
        );
    }
}

#[test]
fn relocated_native_package_runs_without_python_cargo_or_path_tools() {
    let runtime = Runtime::new();
    runtime.call(&["check-package", runtime.root.to_str().unwrap()], None);
    assert!(!runtime.codex_home.exists());
    let output = runtime.json(&["hook", "UserPromptSubmit"], Some(json!({
        "hook_event_name":"UserPromptSubmit","cwd":runtime.project,"session_id":"relocated","turn_id":"turn","prompt":"/faust-mode"})));
    let context = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains(
        runtime
            .root
            .join("skills/faust-mode/SKILL.md")
            .to_str()
            .unwrap()
    ));
    assert!(context.contains("model-check —"));
    assert!(!runtime.codex_home.join("fstack/memory").exists());
}

#[test]
fn failed_disable_reports_only_a_single_turn_choice_through_the_cli() {
    let runtime = Runtime::new();
    let submit = |prompt| {
        runtime.json(&["hook", "UserPromptSubmit"], Some(json!({
        "hook_event_name":"UserPromptSubmit","cwd":runtime.project,"session_id":"locked-mode","turn_id":"turn","prompt":prompt
    })))
    };
    let text = |result: &Value| {
        result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(text(&submit("/faust-mode")).contains("включён faust-mode"));
    let directory = runtime.user_home.join("cache/fstack/coordinator-locks");
    let path = fs::read_dir(directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let failed = text(&submit("/faust-mode-off"));
    assert!(failed.contains("только на этот запрос"));
    assert!(failed.contains("Постоянный выбор не сохранён"));
    drop(lock);
    assert!(text(&submit("Продолжи")).contains("включён faust-mode"));
    let disabled = text(&submit("/faust-mode-off"));
    assert!(disabled.contains("Faust-mode выключен"));
    assert!(!disabled.contains("только на этот запрос"));
    assert!(text(&submit("Продолжи")).contains("Faust-mode выключен"));
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
    let startup = &hooks["hooks"]["SessionStart"][0]["hooks"][0];
    let input = || json!({"hook_event_name": "SessionStart", "cwd": runtime.project, "session_id": "startup"});
    assert!(startup.get("async").is_none());

    assert!(
        runtime.hook(startup, Some(input()))["hookSpecificOutput"]["additionalContext"].is_string()
    );
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

    assert!(
        runtime.hook(startup, Some(input()))["hookSpecificOutput"]["additionalContext"].is_string()
    );
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
                $launcher = [IO.Path]::Combine($env:PLUGIN_ROOT, 'launchers', 'fstack.ps1')
                & ([scriptblock]::Create([IO.File]::ReadAllText($launcher))) -InstallRuntime
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
fn first_prompt_can_activate_after_session_start_downloads_runtime() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    let runtime = Runtime::new();
    let fixtures = tempfile::tempdir().unwrap();
    let bytes = fs::read(&runtime.binary).unwrap();
    fs::remove_file(&runtime.binary).unwrap();
    fs::write(fixtures.path().join("binary"), &bytes).unwrap();
    fs::write(
        fixtures.path().join("digest"),
        format!("{:x}", Sha256::digest(&bytes)),
    )
    .unwrap();
    let curl = fixtures.path().join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
set -eu
while [ "${1#https://}" = "$1" ]; do shift; done
case "$1" in
    *.sha256)
        sleep 1
        name=${1##*/}
        printf '%s  %s\n' "$(cat "$FSTACK_FIXTURES/digest")" "${name%.sha256}" > "$3"
        ;;
    *) cp "$FSTACK_FIXTURES/binary" "$3" ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(curl, fs::Permissions::from_mode(0o700)).unwrap();

    let hooks: Value =
        serde_json::from_slice(&fs::read(runtime.root.join("hooks/hooks.json")).unwrap()).unwrap();
    let handler = &hooks["hooks"]["SessionStart"][0]["hooks"][0];
    assert!(handler.get("async").is_none());
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", handler["command"].as_str().unwrap()])
        .env("PLUGIN_ROOT", &runtime.root)
        .env("PATH", fixtures.path())
        .env("FSTACK_FIXTURES", fixtures.path());
    let session =
        json!({"hook_event_name": "SessionStart", "cwd": runtime.project, "session_id": "cold"});
    let output: Value = serde_json::from_str(&runtime.run(&mut command, Some(session))).unwrap();
    assert!(output["hookSpecificOutput"]["additionalContext"].is_string());
    let submit = |turn, prompt| {
        runtime.hook(&hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0],
        Some(json!({"hook_event_name": "UserPromptSubmit", "cwd": runtime.project, "session_id": "cold", "turn_id": turn, "prompt": prompt})))
    };
    for (turn, prompt) in [("first", "/faust-mode"), ("second", "Продолжи")] {
        let output = submit(turn, prompt);
        assert!(output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("включён faust-mode"));
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
