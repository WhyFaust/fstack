use super::*;
use std::{collections::BTreeMap, fs, sync::OnceLock};
use store::{Binding, Store};
pub(super) fn cache() -> PathBuf {
    static CACHE: OnceLock<PathBuf> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("fstack-memory-tests-{}", io::nonce().unwrap()))
        })
        .clone()
}
struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    vault: PathBuf,
    store: Store,
    binding: Binding,
}
impl Fixture {
    fn new(mode: &str, slug: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        let vault = root.join("vault");
        fs::create_dir(&repo).unwrap();
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let package = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let store = Store::new(Some(root.join("state")), package).unwrap();
        let binding = store.setup(&vault, &repo, slug, Some(mode)).unwrap();
        Self {
            _temp: temp,
            base: root,
            repo,
            vault,
            store,
            binding,
        }
    }
    fn root(&self) -> PathBuf {
        self.store.root(&self.binding).unwrap()
    }
    fn save(&self, body: &str, approved: bool) -> Value {
        self.store
            .save(&self.binding, &candidate(body), approved, None, None)
            .unwrap()
    }
    fn legacy(&self, folder: &str, id: &str, predecessor: Option<&str>) -> PathBuf {
        let p = self.root().join(folder).join(format!("{id}.md"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = BTreeMap::from([("id".into(), id.into())]);
        if let Some(s) = predecessor {
            f.insert("supersedes".into(), s.into());
        }
        fs::write(
            &p,
            format::markdown(
                if folder == "Approved" {
                    format::APPROVED
                } else {
                    format::FORGOTTEN
                },
                &f,
                "",
            ),
        )
        .unwrap();
        p
    }
    fn pending(&self) -> pending::Pending {
        pending::Pending::new(&self.store.state, &self.repo)
    }
    fn hook(&self, event: &str, turn: &str, extra: Value) -> Value {
        let mut v = json!({"hook_event_name":event,"session_id":"session","turn_id":turn,"cwd":self.repo,"stop_hook_active":false});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        hooks::hook(&self.store, event, &v, &self.repo).unwrap()
    }
    fn token(&self, turn: &str) -> String {
        let v = self.hook("UserPromptSubmit", turn, json!({"prompt":"heliotrope"}));
        let text = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        regex::Regex::new(r"stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})")
            .unwrap()
            .captures(text)
            .unwrap()[1]
            .into()
    }
}
fn candidate(body: &str) -> Value {
    json!({"kind":"decision","title":"Heliotrope convention","body":body})
}
fn id(v: &Value) -> &str {
    v["id"].as_str().unwrap()
}
#[test]
fn validation_and_stable_identity() {
    let c = json!({"kind":"decision","title":"Порог ожидания","body":"Использовать три секунды для чтения конфигурации."});
    let fields = format::candidate(&c).unwrap();
    assert_eq!(
        format::stable_id(&fields),
        "8acdbb7d4e00bec1161538761006b168f1cf58c7244bf0f0dcecb7ca0f98a7b8"
    );
    let mut sourced = fields.clone();
    sourced.insert("source".into(), "another source".into());
    assert_eq!(format::stable_id(&fields), format::stable_id(&sourced));
    for text in [
        "token=secret123",
        "sk-1234567890123456",
        "![image](url)",
        "<script>x</script>",
        "```python\nx\n```",
        "x\u{0080}",
    ] {
        assert!(format::candidate(&candidate(text)).is_err(), "{text}");
    }
    for slug in ["con", "COM1", "com1", "../escape", "a/b", ""] {
        assert!(format::project(slug).is_err());
    }
    assert!(format::project("token-authority").is_ok());
    for raw in [
        r#"{"action":"skip","action":"skip"}"#,
        r#"{"x":{"a":1,"a":2}}"#,
        r#"[]"#,
    ] {
        assert!(read_json(raw.as_bytes(), 20000).is_err());
    }
    assert!(read_json(&b"{} "[..], 2).is_err());
    for a in [
        json!({"action":"skip"}),
        json!({"action":"remember","candidate":c}),
        json!({"action":"forget","id":"a".repeat(64)}),
    ] {
        let canonical = action(&a).unwrap();
        assert_eq!(decode(&encode(&canonical).unwrap()).unwrap(), canonical);
    }
    assert!(action(&json!({"action":"skip","reason":"none"})).is_err());
    assert!(decode("e30=").is_err());
}
#[test]
fn inbox_manual_edits_dedup_and_scope() {
    let f = Fixture::new("inbox", "pilot");
    let v = f.save("Keep heliotrope", true);
    assert_eq!(v["status"], "inbox");
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );
    let p = store::record(&f.root(), id(&v));
    let raw = fs::read_to_string(&p)
        .unwrap()
        .replace("Keep heliotrope", "Edited heliotrope");
    fs::write(&p, &raw).unwrap();
    let a = f.store.approve(&f.binding, id(&v)).unwrap();
    assert_eq!(a["body"], "Edited heliotrope");
    assert_eq!(f.save("Keep heliotrope", true), a);
    let other = Store::new(Some(f.base.join("second")), &f.store.package).unwrap();
    let b = other
        .setup(&f.vault, &f.repo, "pilot", Some("auto"))
        .unwrap();
    assert_eq!(
        other
            .save(&b, &candidate("Keep heliotrope"), true, None, None)
            .unwrap(),
        a
    );
    let alternate = other
        .setup(&f.vault, &f.repo, "other", Some("auto"))
        .unwrap();
    assert_eq!(
        other.search(&alternate, "heliotrope", 5).unwrap(),
        json!([])
    );
    assert!(other.get(&alternate, id(&v)).is_err());
    assert_eq!(
        f.store
            .search(&f.binding, "why should we keep heliotrope?", 5)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn superseding_forgetting_sync_and_damaged_records() {
    let f = Fixture::new("auto", "pilot");
    let first = f.save("First heliotrope", true);
    let mut c = candidate("Second heliotrope");
    c["supersedes"] = first["id"].clone();
    let second = f.store.save(&f.binding, &c, true, None, None).unwrap();
    assert!(f.store.get(&f.binding, id(&first)).is_err());
    let p = store::record(&f.root(), id(&second));
    let raw = fs::read_to_string(&p).unwrap();
    f.store.forget(&f.binding, id(&second)).unwrap();
    fs::write(&p, &raw).unwrap();
    assert!(f.store.get(&f.binding, id(&first)).is_err());
    assert!(f.store.get(&f.binding, id(&second)).is_err());
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );
    f.store.forget(&f.binding, id(&second)).unwrap();
    assert!(!p.exists());
    assert!(f.store.save(&f.binding, &c, true, None, None).is_err());
    let damaged = f.save("Damaged heliotrope", true);
    let p = store::record(&f.root(), id(&damaged));
    fs::write(&p, "not a memory note").unwrap();
    f.store.forget(&f.binding, id(&damaged)).unwrap();
    assert!(!p.exists());
}
#[test]
fn tombstone_recovery_keeps_approved_predecessor_hidden() {
    for captured in [false, true] {
        let f = Fixture::new("auto", "pilot");
        let first = f.save("First heliotrope", true);
        let mut c = candidate("Second heliotrope");
        c["supersedes"] = first["id"].clone();
        let second = f.store.save(&f.binding, &c, true, None, None).unwrap();
        let p = store::record(&f.root(), id(&second));
        let tomb = p.with_file_name(format!("{}-forgotten.json", id(&second)));
        fs::write(&tomb, json!({"id":id(&second)}).to_string()).unwrap();
        if captured {
            fs::rename(
                &p,
                p.with_file_name(format!(".fstack-forget-{}", id(&second))),
            )
            .unwrap();
        }
        assert!(f.store.get(&f.binding, id(&first)).is_err());
        f.store.forget(&f.binding, id(&second)).unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(tomb).unwrap()).unwrap();
        assert_eq!(value["supersedes"], first["id"]);
        assert!(!p.exists());
        assert!(f.store.get(&f.binding, id(&first)).is_err());
    }
}
#[test]
fn draft_noise_and_large_views_do_not_block_approved_status() {
    let f = Fixture::new("auto", "token-authority");
    let first = f.save("First heliotrope", true);
    let mut c = candidate("Second heliotrope");
    c["supersedes"] = first["id"].clone();
    let second = f.store.save(&f.binding, &c, true, None, None).unwrap();
    let p = store::record(&f.root(), id(&second));
    let raw = fs::read_to_string(&p)
        .unwrap()
        .replacen("---\n", "---\ntags: [manual]\n", 1);
    fs::write(&p, raw).unwrap();
    assert!(f.store.get(&f.binding, id(&first)).is_err());
    let draft = f.save("Draft heliotrope", false);
    fs::write(store::record(&f.root(), id(&draft)), "arbitrary note").unwrap();
    assert!(f.store.get(&f.binding, id(&first)).is_err());
    fs::write(f.binding.root.join("Library.base"), "x".repeat(40000)).unwrap();
    let page = views::project_page(&f.root());
    let mut text = fs::read_to_string(&page).unwrap();
    text.push_str(&"Edited note ".repeat(10000));
    fs::write(&page, &text).unwrap();
    f.save("Third heliotrope", true);
    assert_eq!(fs::read_to_string(&page).unwrap(), text);
}
#[test]
fn migration_preserves_legacy_orders_conflicts_and_prune() {
    for marker_first in [false, true] {
        let f = Fixture::new("inbox", "pilot");
        let v = f.save("Legacy heliotrope", false);
        let p = store::record(&f.root(), id(&v));
        let raw = fs::read_to_string(&p).unwrap();
        if marker_first {
            fs::remove_file(&p).unwrap();
        }
        let marker = f.legacy("Approved", id(&v), None);
        if marker_first {
            assert_eq!(f.store.migrate(&f.binding, false).unwrap()["skipped"], 1);
            assert_eq!(f.store.migrate(&f.binding, true).unwrap()["skipped"], 1);
            fs::write(&p, &raw).unwrap();
        }
        assert_eq!(f.store.get(&f.binding, id(&v)).unwrap()["status"], "memory");
        f.store.migrate(&f.binding, false).unwrap();
        assert_eq!(f.store.migrate(&f.binding, false).unwrap()["updated"], 0);
        assert_eq!(f.store.migrate(&f.binding, true).unwrap()["pruned"], 1);
        assert!(!marker.exists());
        assert_eq!(f.store.get(&f.binding, id(&v)).unwrap()["status"], "memory");
        let old = f.legacy("Forgotten", id(&v), None);
        f.store.migrate(&f.binding, false).unwrap();
        assert!(f.store.get(&f.binding, id(&v)).is_err());
        assert_eq!(f.store.migrate(&f.binding, true).unwrap()["pruned"], 1);
        assert!(!old.exists());
        assert!(p.exists());
    }
}
#[test]
fn legacy_tombstone_and_stale_approval_never_override_confirmed_predecessor() {
    let f = Fixture::new("auto", "pilot");
    let a = f.save("Alpha heliotrope", true);
    let b = f.save("Beta heliotrope", true);
    let mut c = candidate("Current heliotrope");
    c["supersedes"] = a["id"].clone();
    let current = f.store.save(&f.binding, &c, true, None, None).unwrap();
    f.store.forget(&f.binding, id(&current)).unwrap();
    let old = f.legacy("Approved", id(&current), Some(id(&b)));
    let forgot = f.legacy("Forgotten", id(&current), Some(id(&b)));
    f.store.migrate(&f.binding, false).unwrap();
    assert!(f.store.get(&f.binding, id(&a)).is_err());
    assert!(f.store.get(&f.binding, id(&b)).is_ok());
    let result = f.store.migrate(&f.binding, true).unwrap();
    assert_eq!(result["skipped"], 2);
    assert!(old.exists() && forgot.exists());
}
#[test]
fn project_page_migration_preserves_edits_and_collisions() {
    let f = Fixture::new("auto", "pilot");
    let named = views::project_page(&f.root());
    let old = f.root().join("Project.md");
    let raw = fs::read_to_string(&named).unwrap().replace(
        "[[Codex Memory/Projects|Все проекты]]",
        "[[Codex Memory/Home|Все проекты]]",
    ) + "\nMy own notes\n";
    fs::remove_file(&named).unwrap();
    fs::write(&old, &raw).unwrap();
    let v = f.save("Legacy-linked heliotrope", true);
    let record = store::record(&f.root(), id(&v));
    assert!(fs::read_to_string(&record)
        .unwrap()
        .contains("/Project|pilot]]"));
    f.store.migrate(&f.binding, false).unwrap();
    assert!(fs::read_to_string(&named)
        .unwrap()
        .ends_with("My own notes\n"));
    assert!(fs::read_to_string(&record)
        .unwrap()
        .contains("/pilot|pilot]]"));
    f.store.migrate(&f.binding, true).unwrap();
    assert!(!old.exists());
    fs::write(&old, &raw).unwrap();
    fs::write(&named, "Occupied user note").unwrap();
    assert_eq!(f.store.migrate(&f.binding, false).unwrap()["skipped"], 1);
    assert_eq!(fs::read_to_string(&named).unwrap(), "Occupied user note");
    assert_eq!(f.store.migrate(&f.binding, true).unwrap()["skipped"], 1);
    assert!(old.exists());
    fs::write(
        &named,
        "---\ntitle: mine\n---\n\nfstack_memory_project: pilot\n",
    )
    .unwrap();
    assert_eq!(views::current_page(&f.root()).unwrap(), Some(old));
}
#[test]
fn missing_pages_and_unmanaged_hub_fail_without_losing_records() {
    let f = Fixture::new("auto", "pilot");
    let v = f.save("Existing heliotrope", true);
    let page = views::project_page(&f.root());
    fs::remove_file(page).unwrap();
    assert!(f.store.get(&f.binding, id(&v)).is_ok());
    assert!(f
        .store
        .save(&f.binding, &candidate("New heliotrope"), true, None, None)
        .is_err());
    assert!(f.store.approve(&f.binding, id(&v)).is_err());
    assert_eq!(f.store.migrate(&f.binding, false).unwrap()["skipped"], 1);
    fs::write(f.binding.root.join("Projects.md"), "my hub").unwrap();
    assert!(f.store.migrate(&f.binding, false).is_err());
    assert!(f.store.setup(&f.vault, &f.repo, "other", None).is_err());
    assert!(f.store.get(&f.binding, id(&v)).is_ok());
}
#[test]
fn concurrent_mutations_and_lock_failure_preserve_records() {
    let f = Fixture::new("inbox", "pilot");
    let v = f.save("Concurrent heliotrope", false);
    std::thread::scope(|scope| {
        let mut threads = Vec::new();
        for _ in 0..8 {
            threads.push(scope.spawn(|| f.store.approve(&f.binding, id(&v)).unwrap()));
        }
        for t in threads {
            assert_eq!(t.join().unwrap()["status"], "memory");
        }
    });
    assert_eq!(
        store::files(&f.root().join("Records"), false)
            .unwrap()
            .len(),
        1
    );
    let held = io::lock(&f.root(), &f.vault).unwrap();
    assert!(f.store.approve(&f.binding, id(&v)).is_err());
    drop(held);
    assert!(f.store.get(&f.binding, id(&v)).is_ok());
    let path = store::record(&f.root(), id(&v));
    let raw = fs::read_to_string(&path).unwrap();
    assert!(store::update(
        &path,
        "stale",
        &BTreeMap::new(),
        &BTreeMap::from([("fstack_status".into(), Some("approved".into()))])
    )
    .is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}
fn stage_command(token: &str, value: &Value) -> String {
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    format!(
        "{}\"{}\" memory stage --turn {token} --payload {}",
        if cfg!(windows) { "& " } else { "" },
        exe.display(),
        encode(&action(value).unwrap()).unwrap()
    )
}
fn post(f: &Fixture, turn: &str, command: String, extra: Value) -> Value {
    let mut v = json!({"tool_name":"Bash","tool_use_id":"call-stage","tool_input":{"command":command},"tool_response":"{\"staged\": true}\n"});
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    f.hook("PostToolUse", turn, v)
}
#[test]
fn hook_reminder_skip_exact_invocation_and_once_only_commit() {
    let f = Fixture::new("auto", "pilot");
    let token = f.token("one");
    let request = json!({"action":"remember","candidate":candidate("Keep heliotrope")});
    let command = stage_command(&token, &request);
    for bad in [
        format!(" {command}"),
        format!("{command}; echo done"),
        format!("{command}\n"),
        command.replace(" stage ", " stage  "),
    ] {
        assert_eq!(post(&f, "one", bad, json!({})), json!({}));
    }
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );
    assert_eq!(f.hook("Stop", "one", json!({}))["decision"], "block");
    for overrides in [
        json!({"tool_name":"exec_command"}),
        json!({"agent_id":"child"}),
        json!({"tool_response":{"staged":true}}),
        json!({"tool_input":{"command":command,"timeout":10}}),
        json!({"tool_use_id":"bad id"}),
    ] {
        assert_eq!(post(&f, "one", command.clone(), overrides), json!({}));
    }
    assert_eq!(post(&f, "one", command.clone(), json!({})), json!({}));
    let different = stage_command(&token, &json!({"action":"skip"}));
    assert_eq!(post(&f, "one", different, json!({}))["decision"], "block");
    assert_eq!(
        f.hook("Stop", "one", json!({"stop_hook_active":true})),
        json!({})
    );
    assert_eq!(
        f.store
            .search(&f.binding, "heliotrope", 5)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(f.hook("Stop", "one", json!({})), json!({}));
    let token = f.token("skip");
    post(
        &f,
        "skip",
        stage_command(&token, &json!({"action":"skip"})),
        json!({}),
    );
    assert_eq!(f.hook("Stop", "skip", json!({})), json!({}));
    f.token("empty");
    assert_eq!(f.hook("Stop", "empty", json!({}))["decision"], "block");
    assert!(f
        .hook("Stop", "empty", json!({"stop_hook_active":true}))
        .get("systemMessage")
        .is_some());
    assert_eq!(f.hook("Stop", "empty", json!({})), json!({}));
}
#[test]
fn hook_guards_binding_changes_and_expired_turns() {
    let f = Fixture::new("auto", "pilot");
    for extra in [
        json!({"permission_mode":"plan"}),
        json!({"collaboration_mode":{"mode":"plan"}}),
        json!({"mode":"plan"}),
        json!({"agent_type":"worker"}),
        json!({"cwd":f.vault}),
        json!({"session_id":"bad id"}),
    ] {
        assert_eq!(f.hook("UserPromptSubmit", "guard", extra), json!({}));
    }
    assert!(!f.store.state.join("pending.sqlite3").exists());
    let token = f.token("one");
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert!(f.pending().is_current(&token, &scope).unwrap());
    let request = json!({"action":"remember","candidate":candidate("Not saved heliotrope")});
    post(&f, "one", stage_command(&token, &request), json!({}));
    f.hook("Stop", "one", json!({"permission_mode":"plan"}));
    assert!(!f.pending().is_current(&token, &scope).unwrap());
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );
    let old = f.token("steered");
    post(&f, "steered", stage_command(&old, &request), json!({}));
    let new = f.token("steered");
    assert_ne!(old, new);
    post(&f, "steered", stage_command(&old, &request), json!({}));
    assert_eq!(f.hook("Stop", "steered", json!({}))["decision"], "block");
    let token = f.token("rebound");
    post(&f, "rebound", stage_command(&token, &request), json!({}));
    f.store
        .setup(&f.vault, &f.repo, "other", Some("auto"))
        .unwrap();
    assert_eq!(f.hook("Stop", "rebound", json!({})), json!({}));
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );
}
#[test]
fn inbox_hooks_approval_forget_and_readonly_stage() {
    let f = Fixture::new("inbox", "pilot");
    let token = f.token("draft");
    let request = json!({"action":"remember","candidate":candidate("Review heliotrope")});
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert!(f.pending().is_current(&token, &scope).unwrap());
    assert_eq!(
        f.hook("Stop", "draft", json!({"last_assistant_message":request}))["decision"],
        "block"
    );
    post(&f, "draft", stage_command(&token, &request), json!({}));
    f.hook("Stop", "draft", json!({"stop_hook_active":true}));
    let inbox = f.store.inbox(&f.binding).unwrap();
    let id = inbox[0]["id"].as_str().unwrap();
    assert_eq!(inbox[0]["status"], "inbox");
    for (a, t) in [("approve", "promotion"), ("forget", "removal")] {
        let token = f.token(t);
        post(
            &f,
            t,
            stage_command(&token, &json!({"action":a,"id":id})),
            json!({}),
        );
        f.hook("Stop", t, json!({}));
        if a == "approve" {
            assert!(f.store.get(&f.binding, id).is_ok());
        } else {
            assert!(f.store.get(&f.binding, id).is_err());
        }
    }
}
#[test]
fn steering_revocation_survives_locked_database_and_failed_generation_replace() {
    let f = Fixture::new("auto", "pilot");
    let key = pending::digest(&["session", "one"]);
    let scope = hooks::scope_id(&f.binding, &f.repo);
    let token = f.token("one");
    f.pending()
        .stage(&key, &token, &scope, &json!({"action":"skip"}))
        .unwrap();
    let conn = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let next = f.pending().invalidate(&key).unwrap();
    assert!(next.is_some());
    conn.execute_batch("ROLLBACK").unwrap();
    assert!(!f.pending().is_current(&token, &scope).unwrap());
    assert_eq!(f.pending().consume(&key, &scope, false).unwrap(), None);
    let token = f.token("blocked");
    let dir = f.store.state.join("pending-revocations");
    fs::remove_dir_all(&dir).unwrap();
    fs::write(&dir, "occupied").unwrap();
    assert_eq!(
        f.hook("UserPromptSubmit", "blocked", json!({}))["decision"],
        "block"
    );
    assert!(f.pending().is_current(&token, &scope).is_err());
}
#[test]
fn pending_limits_expiry_private_storage_and_scope() {
    let f = Fixture::new("auto", "pilot");
    let token = f.token("one");
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert!(!f.pending().is_current(&token, "other").unwrap());
    let conn = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
    conn.execute("UPDATE pending SET expires=0", []).unwrap();
    assert!(!f.pending().is_current(&token, &scope).unwrap());
    assert!(pending::Pending::new(&f.repo.join("state"), &f.repo)
        .issue(&"a".repeat(64), "scope", None)
        .is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            f.store.state.join("pending.sqlite3"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(f.pending().is_current(&token, &scope).is_err());
    }
}
#[test]
fn filesystem_rejects_links_and_atomic_replace_preserves_expected_content() {
    let f = Fixture::new("auto", "pilot");
    let p = f.root().join("safe.md");
    assert!(io::write(&p, "original", false, None).unwrap());
    assert!(!io::write(&p, "replacement", false, None).unwrap());
    assert!(io::write(&p, "replacement", true, Some("wrong")).is_err());
    assert_eq!(io::read(&p).unwrap().unwrap(), "original");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = f.base.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, f.root().join("linked")).unwrap();
        assert!(io::directory(&f.root().join("linked"), true, false).is_err());
        symlink(&p, f.root().join("link.md")).unwrap();
        assert!(io::read(&f.root().join("link.md")).is_err());
        assert!(io::write(&f.root().join("link.md"), "bad", true, None).is_err());
        let token = f.token("one");
        let db = f.store.state.join("pending.sqlite3");
        fs::hard_link(&db, f.base.join("copied-db")).unwrap();
        assert!(f
            .pending()
            .is_current(&token, &hooks::scope_id(&f.binding, &f.repo))
            .is_err());
    }
}
#[test]
fn git_registered_worktrees_share_binding_and_forged_pointers_fail() {
    let f = Fixture::new("auto", "pilot");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&f.repo)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&[
        "-c",
        "user.name=Memory Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "init",
    ]);
    let linked = f.base.join("linked worktree");
    let git_linked = io::normalized(&linked);
    git(&["worktree", "add", "--detach", &git_linked]);
    let nested = linked.join("nested");
    fs::create_dir(&nested).unwrap();
    f.store
        .setup(&f.vault, &nested, "pilot", Some("auto"))
        .unwrap();
    assert_eq!(f.store.resolve(&f.repo).unwrap().unwrap().project, "pilot");
    assert_eq!(
        scope::identity(&f.repo).unwrap(),
        scope::identity(&nested).unwrap()
    );
    let forged = f.base.join("forged");
    fs::create_dir(&forged).unwrap();
    fs::copy(linked.join(".git"), forged.join(".git")).unwrap();
    assert!(f.store.resolve(&forged).is_err());
    assert!(f.store.disable(&linked).unwrap());
    assert!(f.store.resolve(&f.repo).unwrap().is_none());
}
#[test]
fn independent_entry_limits_count_canonical_and_legacy_tombstones_once() {
    let f = Fixture::new("auto", "pilot");
    let records = f.root().join("Records");
    let forgotten = f.root().join("Forgotten");
    fs::create_dir(&forgotten).unwrap();
    for n in 0..4096 {
        let id = format!("{n:064x}");
        fs::write(
            records.join(format!("{id}-forgotten.json")),
            json!({"id":id}).to_string(),
        )
        .unwrap();
    }
    assert_eq!(store::tombstone_ids(&f.root()).unwrap().len(), 4096);
    let first = format!("{:064x}", 0);
    f.legacy("Forgotten", &first, None);
    assert_eq!(store::tombstone_ids(&f.root()).unwrap().len(), 4096);
    fs::write(records.join(format!("{}.md", "f".repeat(64))), "record").unwrap();
    assert_eq!(store::files(&records, false).unwrap().len(), 1);
    let next = format!("{:064x}", 4096);
    f.legacy("Forgotten", &next, None);
    assert!(store::tombstone_ids(&f.root()).is_err());
    fs::remove_file(forgotten.join(format!("{next}.md"))).unwrap();
    fs::write(records.join(format!("{next}-forgotten.json")), "{}").unwrap();
    assert!(store::files(&records, true).is_err());
}
#[test]
fn promotion_rejects_oversize_without_losing_editable_inbox() {
    let f = Fixture::new("inbox", "pilot");
    let v = f.save("Keep editable heliotrope", false);
    let p = store::record(&f.root(), id(&v));
    let mut raw = fs::read_to_string(&p).unwrap();
    raw.push_str(&"\n".repeat(32760 - raw.len()));
    fs::write(&p, &raw).unwrap();
    assert!(f.store.approve(&f.binding, id(&v)).is_err());
    assert_eq!(fs::read_to_string(&p).unwrap(), raw);
    assert_eq!(f.store.inbox(&f.binding).unwrap()[0]["status"], "inbox");
    fs::write(&p, raw.trim_end().to_owned() + "\n").unwrap();
    assert!(f.store.approve(&f.binding, id(&v)).is_ok());
    fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
    f.store.forget(&f.binding, id(&v)).unwrap();
    assert!(!p.exists());
}
#[test]
fn confirmed_legacy_tombstones_upgrade_without_overriding_new_confirmation() {
    let f = Fixture::new("auto", "pilot");
    let first = f.save("First heliotrope", true);
    let second = f.save("Second heliotrope", true);
    let tomb = store::record(&f.root(), id(&second))
        .with_file_name(format!("{}-forgotten.json", id(&second)));
    fs::write(&tomb, json!({"id":id(&second)}).to_string()).unwrap();
    f.legacy("Forgotten", id(&second), Some(id(&first)));
    f.store.migrate(&f.binding, false).unwrap();
    let v: Value = serde_json::from_str(&fs::read_to_string(&tomb).unwrap()).unwrap();
    assert_eq!(v["supersedes"], first["id"]);
    assert!(f.store.get(&f.binding, id(&first)).is_err());
    fs::write(
        &tomb,
        json!({"id":id(&second),"supersedes":null}).to_string(),
    )
    .unwrap();
    f.store.migrate(&f.binding, false).unwrap();
    let v: Value = serde_json::from_str(&fs::read_to_string(&tomb).unwrap()).unwrap();
    assert!(v["supersedes"].is_null());
    assert!(f.store.get(&f.binding, id(&first)).is_ok());
}
#[test]
fn forgotten_records_relink_and_missing_approval_records_retain_legacy_page() {
    let f = Fixture::new("inbox", "pilot");
    let named = views::project_page(&f.root());
    let old = f.root().join("Project.md");
    fs::rename(&named, &old).unwrap();
    let v = f.save("Late heliotrope", false);
    let p = store::record(&f.root(), id(&v));
    let raw = fs::read_to_string(&p).unwrap();
    f.store.forget(&f.binding, id(&v)).unwrap();
    fs::write(&p, &raw).unwrap();
    f.store.migrate(&f.binding, false).unwrap();
    assert!(fs::read_to_string(&p).unwrap().contains("/pilot|pilot]]"));
    assert!(f.store.get(&f.binding, id(&v)).is_err());
    let missing = "e".repeat(64);
    let marker = f.legacy("Approved", &missing, None);
    let r = f.store.migrate(&f.binding, true).unwrap();
    assert!(r["skipped"].as_u64().unwrap() >= 2);
    assert!(marker.exists() && old.exists());
}
#[test]
fn legacy_project_slug_updates_in_place_and_keeps_unmanaged_hub() {
    let f = Fixture::new("auto", "project");
    assert_eq!(views::project_page(&f.root()), f.root().join("Project.md"));
    let page = views::project_page(&f.root());
    let raw = fs::read_to_string(&page).unwrap().replace(
        "[[Codex Memory/Projects|Все проекты]]",
        "[[Codex Memory/Home|Все проекты]]",
    );
    fs::write(&page, raw).unwrap();
    f.store.migrate(&f.binding, false).unwrap();
    assert!(page.exists());
    let project_pages = fs::read_dir(f.root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().eq_ignore_ascii_case("project.md"))
        .collect::<Vec<_>>();
    assert_eq!(project_pages, vec![OsString::from("Project.md")]);
    assert_eq!(f.store.migrate(&f.binding, true).unwrap()["pruned"], 0);
    let g = Fixture::new("auto", "pilot");
    let old = g.root().join("Project.md");
    fs::copy(views::project_page(&g.root()), &old).unwrap();
    fs::write(g.binding.root.join("Projects.md"), "User-owned hub").unwrap();
    assert_eq!(g.store.migrate(&g.binding, true).unwrap()["skipped"], 1);
    assert!(old.exists());
}
#[test]
fn project_lock_does_not_hold_pending_database_during_stop() {
    let f = Fixture::new("auto", "pilot");
    let token = f.token("slow");
    let request = json!({"action":"remember","candidate":candidate("Slow heliotrope")});
    post(&f, "slow", stage_command(&token, &request), json!({}));
    let held = io::lock(&f.root(), &f.vault).unwrap();
    std::thread::scope(|scope| {
        let saving = scope.spawn(|| f.hook("Stop", "slow", json!({})));
        std::thread::sleep(std::time::Duration::from_millis(100));
        let start = std::time::Instant::now();
        let another = f.token("another");
        assert_eq!(another.len(), 97);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        drop(held);
        assert_eq!(saving.join().unwrap(), json!({}));
    });
    assert_eq!(
        f.store
            .search(&f.binding, "heliotrope", 5)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[cfg(unix)]
#[test]
fn revocation_marker_survives_failed_generation_replace() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("auto", "pilot");
    let token = f.token("one");
    let key = pending::digest(&["session", "one"]);
    let scope = hooks::scope_id(&f.binding, &f.repo);
    let dir = f.store.state.join("pending-generations");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let result = f.pending().invalidate(&key);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(!f.pending().is_current(&token, &scope).unwrap());
    assert!(f
        .store
        .state
        .join("pending-revocations")
        .join(&token)
        .is_file());
    assert_eq!(f.pending().consume(&key, &scope, false).unwrap(), None);
}
#[cfg(unix)]
#[test]
fn state_symlinks_non_utf8_paths_and_unsafe_git_path_fail_closed() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = Fixture::new("auto", "pilot");
    let outside = f.base.join("outside");
    fs::create_dir(&outside).unwrap();
    let state = f.base.join("linked-state");
    symlink(&outside, &state).unwrap();
    assert!(pending::Pending::new(&state, &f.repo)
        .issue(&"a".repeat(64), "scope", None)
        .is_err());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    let bad = f.base.join(OsString::from_vec(vec![0xff]));
    assert!(io::absolute(&bad).is_err());
}
#[test]
fn git_executable_ignores_workspace_and_relative_search_paths() {
    let f = Fixture::new("auto", "pilot");
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    fs::write(f.repo.join(name), "not git").unwrap();
    let mut paths = vec![PathBuf::new(), PathBuf::from("."), f.repo.clone()];
    let untrusted = std::env::join_paths(&paths).unwrap();
    assert!(scope::executable(&f.repo, &untrusted).is_err());
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let exe = scope::executable(&f.repo, &std::env::join_paths(paths).unwrap()).unwrap();
    assert!(exe.is_absolute() && !exe.starts_with(&f.repo));
}
#[test]
fn malformed_single_quote_metadata_fails_closed_without_panicking() {
    let f = Fixture::new("auto", "pilot");
    let v = f.save("Existing heliotrope", true);
    let p = store::record(&f.root(), id(&v));
    let raw = fs::read_to_string(&p)
        .unwrap()
        .replacen("---\n", "---\nsource: '\n", 1);
    fs::write(&p, raw).unwrap();
    assert!(f.store.get(&f.binding, id(&v)).is_err());
    assert!(f.store.search(&f.binding, "heliotrope", 5).is_err());
    let result = f.hook("UserPromptSubmit", "one", json!({"prompt":"heliotrope"}));
    assert!(result["hookSpecificOutput"]["additionalContext"].is_string());
}
#[cfg(windows)]
#[test]
fn windows_path_prefix_case_and_separators_preserve_scope() {
    let ordinary = Path::new(r"C:\Users\Example\Repo");
    let verbatim = Path::new(r"\\?\c:\users\example\repo");
    assert!(io::equal(ordinary, verbatim));
    assert!(io::within(
        Path::new(r"\\?\C:\USERS\EXAMPLE\REPO\git.exe"),
        ordinary
    ));
    assert!(!io::within(
        Path::new(r"C:\Users\Example\Repo-other\git.exe"),
        ordinary
    ));
    assert_eq!(io::normalized(ordinary), io::normalized(verbatim));
    assert!(io::equal(
        Path::new(r"\\?\UNC\server\share\repo"),
        Path::new(r"\\SERVER\SHARE\REPO")
    ));
    let f = Fixture::new("auto", "pilot");
    let fake = f.repo.join("git.exe");
    fs::write(&fake, "not Git").unwrap();
    let uppercase = PathBuf::from(f.repo.to_string_lossy().to_uppercase());
    assert!(scope::executable(&f.repo, &std::env::join_paths([uppercase]).unwrap()).is_err());
    let junction = f.base.join("outside-junction");
    let result = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&f.repo)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(scope::executable(&f.repo, &std::env::join_paths([junction]).unwrap()).is_err());
}
#[test]
fn prompt_only_exposes_ids_and_disabled_project_never_issues_token() {
    let f = Fixture::new("auto", "pilot");
    let v = f.save("Private sentinel heliotrope body", true);
    let result = f.hook("UserPromptSubmit", "one", json!({"prompt":"heliotrope"}));
    let text = result["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(text.contains(id(&v)));
    assert!(!text.contains("Private sentinel"));
    assert_eq!(
        store::files(&f.root().join("Records"), false)
            .unwrap()
            .len(),
        1
    );
    assert!(f.store.disable(&f.repo).unwrap());
    assert_eq!(f.hook("UserPromptSubmit", "disabled", json!({})), json!({}));
    assert_eq!(f.hook("Stop", "disabled", json!({})), json!({}));
}
#[test]
fn prompt_search_failure_warns_without_exposing_notes_or_blocking_the_turn() {
    let f = Fixture::new("auto", "pilot");
    let v = f.save("Private sentinel heliotrope body", true);
    let path = store::record(&f.root(), id(&v));
    let raw = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{raw}\n```unsafe-memory-payload\n")).unwrap();
    assert!(f.store.search(&f.binding, "heliotrope", 3).is_err());

    let result = f.hook(
        "UserPromptSubmit",
        "damaged",
        json!({"prompt":"heliotrope"}),
    );
    assert!(result["systemMessage"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(result.get("decision").is_none());
    let text = result["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let token = regex::Regex::new(r"stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})")
        .unwrap()
        .captures(text)
        .unwrap()[1]
        .to_owned();
    let output = result.to_string();
    for hidden in ["Private sentinel", "unsafe-memory-payload", id(&v)] {
        assert!(!output.contains(hidden));
    }
    assert_eq!(
        post(
            &f,
            "damaged",
            stage_command(&token, &json!({"action":"skip"})),
            json!({})
        ),
        json!({})
    );
    assert_eq!(f.hook("Stop", "damaged", json!({})), json!({}));

    fs::write(&path, raw).unwrap();
    for (turn, prompt) in [("recovered", "heliotrope"), ("empty", "nohits")] {
        let result = f.hook("UserPromptSubmit", turn, json!({"prompt":prompt}));
        assert!(result.get("systemMessage").is_none());
        assert!(result.get("decision").is_none());
    }
}
#[test]
fn migration_does_not_modify_another_project_and_cross_profile_locking_hides_forget() {
    let f = Fixture::new("inbox", "pilot");
    let v = f.save("Current heliotrope", false);
    let marker = f.legacy("Approved", id(&v), None);
    let other = f
        .store
        .setup(&f.vault, &f.repo, "other", Some("inbox"))
        .unwrap();
    let other_entry = f
        .store
        .save(&other, &candidate("Other heliotrope"), false, None, None)
        .unwrap();
    let p = store::record(&f.store.root(&other).unwrap(), id(&other_entry));
    let before = fs::read(&p).unwrap();
    f.store.migrate(&f.binding, false).unwrap();
    f.store.migrate(&f.binding, true).unwrap();
    assert!(!marker.exists());
    assert_eq!(fs::read(&p).unwrap(), before);
    let other_store = Store::new(Some(f.base.join("second-profile")), &f.store.package).unwrap();
    let b = other_store
        .setup(&f.vault, &f.repo, "pilot", Some("auto"))
        .unwrap();
    std::thread::scope(|scope| {
        let save = scope
            .spawn(|| other_store.save(&b, &candidate("Current heliotrope"), true, None, None));
        let remove = scope.spawn(|| f.store.forget(&f.binding, id(&v)));
        let _ = save.join().unwrap();
        remove.join().unwrap().unwrap();
    });
    assert!(other_store.get(&b, id(&v)).is_err());
    assert!(f.store.get(&f.binding, id(&v)).is_err());
    assert!(!store::record(&f.root(), id(&v)).exists());
}
