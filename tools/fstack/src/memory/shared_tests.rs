use super::*;
use crate::memory::execute as cli;
use std::path::PathBuf;

struct Fixture {
    base: tempfile::TempDir,
    vault: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let temp_root = std::env::temp_dir().canonicalize().unwrap();
        let base = tempfile::tempdir_in(temp_root).unwrap();
        let vault = base.path().join("vault");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let package = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let store = Store::new(Some(base.path().join("state")), package).unwrap();
        Self { base, vault, store }
    }
    fn project(&self, name: &str, mode: &str) -> (PathBuf, Binding) {
        let cwd = self.base.path().join(name);
        fs::create_dir(&cwd).unwrap();
        let b = self
            .store
            .setup(&self.vault, &cwd, name, Some(mode))
            .unwrap();
        (cwd, b)
    }
    fn cmd(&self, cwd: &Path, args: &[&str]) -> Result<Value> {
        cli(
            &self.store,
            &args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            cwd,
        )
    }
}
fn preference(body: &str) -> Value {
    json!({"kind":"preference","title":"Регламент","body":body})
}

#[test]
fn shared_preferences_are_vault_scoped_and_never_import_project_records() {
    let f = Fixture::new();
    let (a, ba) = f.project("shared", "auto");
    let (b, _) = f.project("beta", "auto");
    let pref = preference("Проверяй релиз дважды");
    let local = f.store.save(&ba, &pref, true, None, None).unwrap();
    assert!(for_recall(&f.store, &ba).unwrap().is_none());
    assert_eq!(
        f.cmd(&b, &["shared", "search", "релиз"]).unwrap(),
        json!([])
    );
    assert!(!ba.root.join("Shared").exists());
    let id = local["id"].as_str().unwrap();
    assert!(f.cmd(&b, &["shared", "show", id]).is_err());
    let shared = remember(&f.store, &ba, &pref).unwrap();
    assert_eq!(shared["scope"], "shared");
    assert_eq!(f.cmd(&b, &["shared", "show", id]).unwrap(), shared);
    assert_eq!(
        f.cmd(&b, &["shared", "search", "релиз"]).unwrap(),
        json!([shared])
    );
    assert_eq!(f.cmd(&b, &["search", "релиз"]).unwrap(), json!([]));
    assert!(f.cmd(&b, &["show", id]).is_err());
    let other_vault = f.base.path().join("other-vault");
    fs::create_dir_all(other_vault.join(".obsidian")).unwrap();
    f.store
        .setup(&other_vault, &b, "beta", Some("auto"))
        .unwrap();
    assert_eq!(
        f.cmd(&b, &["shared", "search", "релиз"]).unwrap(),
        json!([])
    );
    assert!(f
        .cmd(f.base.path(), &["shared", "search", "релиз"])
        .is_err());
    assert_eq!(
        f.cmd(&a, &["shared", "forget", id]).unwrap()["scope"],
        "shared"
    );
    assert_eq!(f.cmd(&a, &["show", id]).unwrap(), local);
}

#[test]
fn shared_inbox_supersession_and_forgetting_are_scoped() {
    let f = Fixture::new();
    let (a, ba) = f.project("pilot", "inbox");
    let (b, _) = f.project("beta", "auto");
    let first = remember(&f.store, &ba, &preference("Сначала журнал")).unwrap();
    assert_eq!(first["status"], "inbox");
    let id = first["id"].as_str().unwrap();
    assert_eq!(f.cmd(&b, &["shared", "inbox"]).unwrap()[0]["id"], id);
    assert_eq!(
        f.cmd(&a, &["shared", "search", "журнал"]).unwrap(),
        json!([])
    );
    let approved = f.cmd(&a, &["shared", "approve", id]).unwrap();
    assert_eq!(approved["status"], "memory");
    let mut replacement = preference("Сначала протокол");
    replacement["supersedes"] = json!(id);
    let second = remember(&f.store, &ba, &replacement).unwrap();
    let second_id = second["id"].as_str().unwrap();
    assert_eq!(
        f.cmd(&b, &["shared", "show", id]).unwrap()["body"],
        first["body"]
    );
    assert!(f.cmd(&a, &["approve", second_id]).is_err());
    f.cmd(&a, &["shared", "approve", second_id]).unwrap();
    assert!(f.cmd(&b, &["shared", "show", id]).is_err());
    let history = f.cmd(&a, &["shared", "history", second_id]).unwrap();
    assert_eq!(history["entries"].as_array().unwrap().len(), 2);
    assert_eq!(history["scope"], "shared");
    f.cmd(&a, &["shared", "forget", second_id]).unwrap();
    assert_eq!(
        f.cmd(&b, &["shared", "search", "протокол"]).unwrap(),
        json!([])
    );
}

#[test]
fn shared_writes_reject_non_preferences_and_unowned_folders() {
    let f = Fixture::new();
    let (cwd, b) = f.project("one", "auto");
    let mut wrong = preference("эту задачу исправили");
    wrong["kind"] = json!("correction");
    assert!(remember(&f.store, &b, &wrong).is_err());
    let root = b.root.join("Shared");
    assert!(!root.exists());
    fs::create_dir_all(root.join("Records")).unwrap();
    fs::write(root.join("Shared.md"), "Вручную созданная заметка.").unwrap();
    assert!(remember(&f.store, &b, &preference("всегда так")).is_err());
    assert_eq!(f.cmd(&cwd, &["shared", "inbox"]).unwrap(), json!([]));
    assert_eq!(
        fs::read_to_string(root.join("Shared.md")).unwrap(),
        "Вручную созданная заметка."
    );
}

#[test]
fn new_chat_in_another_project_receives_only_scoped_approved_ids() {
    let f = Fixture::new();
    let (a, ba) = f.project("one", "auto");
    let (b, _) = f.project("two", "inbox");
    let body = "Пиши комментарии на русском.";
    let pref = remember(&f.store, &ba, &preference(body)).unwrap();
    let id = pref["id"].as_str().unwrap();
    let (_, bc) = f.project("three", "inbox");
    let pending = remember(&f.store, &bc, &preference("Черновик из inbox")).unwrap();
    let payload = |cwd: &Path, prompt: &str, mode: &str| {
        json!({
            "session_id":"fresh-thread", "turn_id":"one", "hook_event_name":"UserPromptSubmit",
            "cwd":cwd, "permission_mode":mode, "prompt":prompt
        })
    };
    let hook = |cwd: &Path, prompt: &str, mode: &str| {
        crate::memory::hooks::hook(
            &f.store,
            "UserPromptSubmit",
            &payload(cwd, prompt, mode),
            cwd,
        )
        .unwrap()
    };
    let active = hook(&b, "/faust-mode Почини сетевое соединение.", "default");
    let context = active["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains(id));
    assert!(context.contains("shared show ID"));
    assert!(!context.contains(body));
    assert!(!context.contains(pending["id"].as_str().unwrap()));
    assert!(!hook(&b, "Проанализируй код.", "plan")
        .to_string()
        .contains(id));
    f.cmd(&a, &["shared", "forget", id]).unwrap();
    assert!(!hook(&b, "Продолжай.", "default").to_string().contains(id));
}

#[test]
fn staged_shared_action_waits_for_stop_and_retries_without_duplicates() {
    let f = Fixture::new();
    let (cwd, proj) = f.project("pilot", "auto");
    let (other, _) = f.project("next", "auto");
    let event = |event: &str, turn: &str, mut fields: Value| {
        fields["cwd"] = json!(cwd);
        fields["session_id"] = json!("session");
        fields["turn_id"] = json!(turn);
        fields["hook_event_name"] = json!(event);
        crate::memory::hooks::hook(&f.store, event, &fields, &cwd).unwrap()
    };
    let mut request = json!({"action":"remember","scope":"shared","candidate":preference("Всегда прикладывай журнал.")});
    request["candidate"]["kind"] = json!("correction");
    assert!(crate::memory::action(&request).is_err());
    request["candidate"]["kind"] = json!("preference");
    let encoded = crate::memory::encode(&crate::memory::action(&request).unwrap()).unwrap();
    assert!(crate::memory::action(&json!({"action":"skip","scope":"shared"})).is_err());
    assert!(
        crate::memory::action(&json!({"action":"forget","scope":"typo","id":"a".repeat(64)}))
            .is_err()
    );
    let prompt = "/faust-mode Во всех проектах всегда прикладывай журнал.";
    let stage = |turn: &str| {
        let start = event("UserPromptSubmit", turn, json!({"prompt":prompt}));
        let context = start["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        let re = regex::Regex::new(r#"("[^"]+" memory stage --turn [0-9a-f]{64}\.[0-9a-f]{32})"#)
            .unwrap();
        let command = format!(
            "{}{} --payload {encoded}",
            if cfg!(windows) { "& " } else { "" },
            &re.captures(context).unwrap()[1]
        );
        event(
            "PostToolUse",
            turn,
            json!({"tool_name":"Bash","tool_use_id":"call-1","tool_input":{"command":command},"tool_response":"{\"staged\": true}\n"}),
        );
    };
    stage("cancelled");
    event(
        "Stop",
        "cancelled",
        json!({"permission_mode":"plan","stop_hook_active":false}),
    );
    assert!(for_recall(&f.store, &proj).unwrap().is_none());
    stage("normal");
    assert!(for_recall(&f.store, &proj).unwrap().is_none());
    for _ in 0..2 {
        event("Stop", "normal", json!({"stop_hook_active":false}));
    }
    assert_eq!(f.cmd(&cwd, &["diagnostics"]).unwrap()["phase"], "applied");
    let found = f.cmd(&other, &["shared", "search", "журнал"]).unwrap();
    assert_eq!(found.as_array().unwrap().len(), 1);
    assert_eq!(found[0]["turn_id"], "normal");
    assert_eq!(found[0]["scope"], "shared");
    assert_eq!(f.cmd(&cwd, &["search", "журнал"]).unwrap(), json!([]));
}
