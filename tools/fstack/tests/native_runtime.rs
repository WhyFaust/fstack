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

    fn call(&self, args: &[&str], input: Option<Value>) -> String {
        let mut child = Command::new(&self.binary)
            .args(args)
            .env("PATH", "")
            .env("CODEX_HOME", &self.home)
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
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn json(&self, args: &[&str], input: Option<Value>) -> Value {
        serde_json::from_str(&self.call(args, input)).unwrap()
    }
}

#[test]
fn relocated_native_package_runs_without_python_cargo_or_path_tools() {
    let runtime = Runtime::new();
    runtime.call(&["check-package", runtime.root.to_str().unwrap()], None);
    let before = runtime.json(&["profile"], None);
    assert_ne!(before["status"], "unchanged");
    runtime.json(&["profile", "--apply"], None);
    assert!(!runtime.home.join("AGENTS.md").exists());
    assert_eq!(runtime.json(&["profile"], None)["status"], "unchanged");
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
