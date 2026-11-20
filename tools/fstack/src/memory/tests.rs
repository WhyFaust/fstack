use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::OnceLock,
};
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
        let mut v = json!({"hook_event_name":event,"session_id":"session","turn_id":turn,"cwd":self.repo,"collaboration_mode":{"mode":"default"},"stop_hook_active":false});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        hooks::hook(&self.store, event, &v, &self.repo).unwrap()
    }
    fn token(&self, turn: &str) -> String {
        let v = self.hook(
            "UserPromptSubmit",
            turn,
            json!({"prompt":"/faust-mode heliotrope"}),
        );
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
#[test]
fn a_codex_task_can_only_report_new_project_decisions_with_sources() {
    let decide = |fields: Value| json!({"action":"remember","candidate":fields});
    let fields = json!({"kind":"decision","title":"Convention","body":"Verified result","source":"Build output"});
    let payload = |action: Value| report_action(&["--payload", &encode(&action).unwrap()]);
    assert!(payload(decide(fields.clone())).is_ok());
    let mut shared = decide(fields.clone());
    shared["scope"] = json!("shared");
    shared["candidate"]["kind"] = json!("preference");
    let id = "a".repeat(64);
    let mut superseding = fields.clone();
    superseding["supersedes"] = json!(id);
    let mut correction = fields.clone();
    correction["kind"] = json!("correction");
    for operation in [
        shared,
        decide(superseding),
        decide(correction),
        decide(candidate("Result with no source")),
        json!({"action":"approve","id":id}),
        json!({"action":"forget","id":id}),
    ] {
        assert!(payload(operation).is_err());
    }
}
#[test]
fn ordinary_prompts_get_memory_without_implicitly_activating_the_coordinator() {
    let f = Fixture::new("auto", "pilot");
    for prompt in [
        "Исправь ошибку",
        "Объясни /faust-mode",
        "`/faust-mode`",
        "> /faust-mode task",
        "[$fstack:faust-mode](/projects/tools(qa)/SKILL.md",
        "[$fstack:faust-mode](/projects/tools(qa)/SKILL.md)example",
        "Проверь инструкции в skills/delegate/SKILL.md с примерами $fstack:faust-mode и $fstack:debug",
        "$fstack:debug task",
    ] {
        let output = f.hook("UserPromptSubmit", "plain", json!({"prompt":prompt}));
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("stage --turn"));
        assert!(context.contains("Faust-mode выключен"));
        assert!(context.contains("только явно вызванный пользователем навык fstack"));
        assert!(!context.contains("SKILL.md"));
        assert!(!context.contains("docs/skills.md"));
        assert!(!context.contains("model-check —"));
    }
    assert!(!f.store.state.join("faust-mode-sessions").exists());
    assert!(!f.store.state.join("memory-sessions").exists());
}

#[test]
fn ultra_follows_the_verified_root_turn_and_respects_the_manual_chat_choice() {
    let f = Fixture::new("auto", "pilot");
    let store = Store::new(Some(f.base.join("fstack/memory")), &f.store.package).unwrap();
    let sessions = f.base.join("sessions/2026/11/20");
    fs::create_dir_all(&sessions).unwrap();
    let session = "ultra-test";
    let transcript = sessions.join(format!("rollout-2026-11-20T12-00-00-{session}.jsonl"));
    let snapshot = |source: Value, context: Value| {
        let meta = json!({"type":"session_meta","payload":{"id":session,"source":source}});
        let start = json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"current","collaboration_mode_kind":"default"}});
        let row = json!({"type":"turn_context","payload":context});
        fs::write(&transcript, format!("{meta}\n{start}\n{row}\n")).unwrap();
    };
    let context = |effort: &str| json!({"turn_id":"current","cwd":f.repo,"effort":effort,"collaboration_mode":{"mode":"default"}});
    let submit = |turn: &str, prompt: &str, fields: Value| {
        let mut input = json!({"hook_event_name":"UserPromptSubmit","session_id":session,
            "cwd":f.repo,"turn_id":turn,"prompt":prompt,"transcript_path":transcript});
        input
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        hooks::hook(&store, "UserPromptSubmit", &input, &f.repo).unwrap()["hookSpecificOutput"]
            ["additionalContext"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let ordinary = || submit("current", "Проверь модуль", json!({}));
    let root = json!("vscode");
    snapshot(root.clone(), context("ultra"));
    assert!(ordinary().contains("по глубине ultra"));
    assert!(ordinary().contains("model-check —"));
    assert!(!store.state.join("faust-mode-sessions").exists());
    assert!(submit("new-turn", "Продолжи", json!({})).contains("выключен"));
    assert!(
        submit("current", "Продолжи", json!({"permission_mode":"plan"}))
            .contains("нативном Plan Mode")
    );
    let mut other_cwd = context("ultra");
    other_cwd["cwd"] = json!(f.base);
    snapshot(root.clone(), other_cwd);
    assert!(ordinary().contains("выключен"));
    snapshot(json!({"subagent":{"thread_spawn":{}}}), context("ultra"));
    assert!(ordinary().contains("выключен"));
    snapshot(root.clone(), context("low"));
    assert!(submit("current", "Объясни ultra", json!({"effort":"ultra"})).contains("выключен"));
    snapshot(root, context("ultra"));
    assert!(submit("current", "/faust-mode-off", json!({})).contains("выключен"));
    assert!(ordinary().contains("выключен"));
    assert!(submit("current", "/faust-mode", json!({})).contains("до команды"));
    snapshot(json!("cli"), context("high"));
    assert!(ordinary().contains("до команды"));
}

#[test]
fn markdown_command_examples_preserve_the_current_mode() {
    let f = Fixture::new("auto", "pilot");
    for (session, active) in [("plain", false), ("coordinator", true)] {
        let submit = |prompt: &str| {
            let output = f.hook(
                "UserPromptSubmit",
                "turn",
                json!({"session_id":session,"prompt":prompt}),
            );
            output["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        if active {
            assert!(submit("\n   /faust-mode задача").contains("включён faust-mode"));
        }
        for command in ["/faust-mode", "/faust-mode-off"] {
            for indent in ["    ", "\t", "  \t"] {
                let example = format!("\n{indent}{command}\n\nОбъясни этот пример.");
                let context = submit(&example);
                assert_eq!(
                    context.contains("включён faust-mode"),
                    active,
                    "{example:?}"
                );
                assert!(!context.contains("model-check —"));
            }
        }
        assert_eq!(submit("Продолжи").contains("включён faust-mode"), active);
    }
}

#[test]
fn only_an_enabled_chat_receives_the_installed_index_on_activation_and_restore() {
    let f = Fixture::new("auto", "pilot");
    f.store.disable(&f.repo).unwrap();
    let skills = crate::package::skill_names(&f.store.package).unwrap();
    let contents = |event: &str, fields: Value| {
        f.hook(event, "turn", fields)["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let assert_index = |context: &str| {
        let names: BTreeSet<_> = context
            .lines()
            .filter_map(|line| line.split_once(" — "))
            .filter(|(name, signal)| !name.contains(' ') && !signal.is_empty())
            .map(|(name, _)| name.to_owned())
            .collect();
        assert_eq!(names, skills);
        for relative in [
            "skills/faust-mode/SKILL.md",
            "skills/route/SKILL.md",
            "docs/skills.md",
        ] {
            assert!(context.contains(&f.store.package.join(relative).display().to_string()));
        }
        assert!(!context.contains("stage --turn"));
    };
    let initial = contents("SessionStart", json!({}));
    assert!(initial.contains("Режим текущего запроса сообщит UserPromptSubmit"));
    assert!(!initial.contains("выключен"));
    assert!(!initial.contains("model-check —"));
    assert_index(&contents(
        "UserPromptSubmit",
        json!({"prompt":"/faust-mode"}),
    ));
    let next = contents("UserPromptSubmit", json!({"prompt":"Продолжи"}));
    assert!(next.contains("При смене этапа"));
    assert!(!next.contains("model-check —"));
    for source in ["resume", "compact"] {
        assert_index(&contents("SessionStart", json!({"source":source})));
    }
    let other = contents("SessionStart", json!({"session_id":"other"}));
    assert_eq!(other, initial);
    assert!(!other.contains("SKILL.md"));
    let worker = contents(
        "SubagentStart",
        json!({"agent_id":"worker","session_id":"worker","agent_type":"worker"}),
    );
    assert!(worker.contains("активный координатор передал"));
    assert!(!worker.contains("SKILL.md"));
    let binding = scope::binding_path(&f.store.state, &f.repo).unwrap();
    fs::write(binding, "invalid binding").unwrap();
    let broken_memory = f.hook("SessionStart", "turn", json!({"source":"resume"}));
    assert!(broken_memory["systemMessage"].is_string());
    assert_index(
        broken_memory["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap(),
    );
    contents("UserPromptSubmit", json!({"prompt":"/faust-mode-off"}));
    let restored = contents("SessionStart", json!({"source":"resume"}));
    assert_eq!(restored, initial);
    assert!(!restored.contains("model-check —"));
}

#[test]
fn explicit_mode_controls_only_the_coordinator_and_keeps_accepted_memory() {
    let f = Fixture::new("auto", "pilot");
    for prompt in [
        "/faust-mode task",
        " $fstack:faust-mode task",
        "$faust-mode\ntask",
        "[$fstack:faust-mode](C:/Codex Home/skills/faust-mode/SKILL.md) task",
        "[$fstack:faust-mode](/projects/tools(qa)/skills/faust-mode/SKILL.md) task",
        "Продолжи",
    ] {
        let output = f.hook("UserPromptSubmit", "active", json!({"prompt":prompt}));
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("включён faust-mode"));
        assert!(context.contains("stage --turn"));
    }
    let other = f.hook(
        "UserPromptSubmit",
        "next",
        json!({"prompt":"Продолжи","session_id":"another"}),
    );
    let other = other["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(other.contains("stage --turn"));
    assert!(!other.contains("включён faust-mode"));

    stage_action(
        &f,
        "queued",
        &json!({"action":"remember","candidate":candidate("Survive coordinator toggle")}),
    );
    let off = f.hook(
        "UserPromptSubmit",
        "off",
        json!({"prompt":"/faust-mode-off"}),
    );
    let context = off["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("выключен"));
    assert!(context.contains("stage --turn"));
    f.hook("Stop", "queued", json!({}));
    assert_eq!(
        f.store
            .search(&f.binding, "Survive", 5)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let next = f.hook("UserPromptSubmit", "new", json!({"prompt":"Продолжи"}));
    let context = next["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("stage --turn"));
    assert!(!context.contains("включён faust-mode"));
}

#[test]
fn explicit_mode_activates_without_binding_and_in_plan_without_memory() {
    let f = Fixture::new("auto", "pilot");
    let plan = f.hook(
        "UserPromptSubmit",
        "plan",
        json!({"prompt":"/faust-mode plan", "permission_mode":"plan"}),
    );
    assert!(plan["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("нативном Plan Mode"));
    assert!(!f.store.state.join("pending.sqlite3").exists());
    let store = Store::new(Some(f.base.join("unbound-state")), &f.store.package).unwrap();
    let payload = json!({"hook_event_name":"UserPromptSubmit", "session_id":"new", "turn_id":"new", "cwd":f.repo,"prompt":"/faust-mode task"});
    let output = hooks::hook(&store, "UserPromptSubmit", &payload, &f.repo).unwrap();
    assert!(output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("fstack:faust-mode"));
    assert!(output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains(
            &store
                .package
                .join("skills/faust-mode/SKILL.md")
                .display()
                .to_string()
        ));
    assert!(!store.state.join("pending.sqlite3").exists());
    let followup = json!({"hook_event_name":"UserPromptSubmit", "session_id":"new", "turn_id":"next", "cwd":f.repo,"prompt":"Продолжи"});
    assert!(
        hooks::hook(&store, "UserPromptSubmit", &followup, &f.repo).unwrap()["hookSpecificOutput"]
            ["additionalContext"]
            .as_str()
            .is_some()
    );
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
fn existing_library_is_preserved() {
    let f = Fixture::new("auto", "pilot");
    let path = f.binding.root.join("Library.base");
    let custom = "views: []\n# Personal table settings\n";
    fs::write(&path, custom).unwrap();
    views::ensure_library(&f.binding.root, &f.store.package).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), custom);
}

#[test]
fn obsidian_properties_survive_read_and_approval() {
    let f = Fixture::new("inbox", "pilot");
    for properties in [
        "tags: [manual, project/rules]\naliases: [\"Порог чтения\"]\ncssclasses: compact\n",
        "tags:\n  - manual\n  - project/rules\naliases:\n- \"Порог чтения\"\ncssclasses:\n  - compact\n",
    ] {
        let entry = f.save(properties.lines().next().unwrap(), false);
        let path = store::record(&f.root(), id(&entry));
        let raw = fs::read_to_string(&path).unwrap();
        let decorated = raw.replacen("---\n", &format!("---\n{properties}"), 1);
        fs::write(&path, &decorated).unwrap();
        assert_eq!(f.store.inbox(&f.binding).unwrap().as_array().unwrap().len(), 1);
        let approved = f.store.approve(&f.binding, id(&entry)).unwrap();
        assert_eq!(approved["body"], entry["body"]);
        assert!(fs::read_to_string(&path).unwrap().contains(properties));
        assert_eq!(f.store.get(&f.binding, id(&entry)).unwrap(), approved);
        assert!(!f.store.search(&f.binding, "tags", 5).unwrap().as_array().unwrap().is_empty());
    }
}

#[test]
fn obsidian_properties_cannot_hide_reserved_metadata_or_duplicate_keys() {
    let fields = BTreeMap::from([
        ("id".into(), "a".repeat(64)),
        ("kind".into(), "decision".into()),
        ("title".into(), "Правило".into()),
    ]);
    let raw = format::markdown(format::MEMORY, &fields, "Проверенное правило.");
    for invalid in [
        "tags: [one]\ntags: [two]\n",
        "tags:\n  id: invalid\n",
        "aliases:\n  - fstack_status: approved\n",
        "cssclasses: {id: invalid}\n",
        "aliases: []\nid: invalid\n",
    ] {
        let value = raw.replacen("---\n", &format!("---\n{invalid}"), 1);
        assert!(format::parse(&value, &"a".repeat(64), format::MEMORY, true).is_err());
    }
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
fn fresh_session_recalls_approved_correction_across_russian_word_forms() {
    let f = Fixture::new("auto", "pilot");
    let save = |kind: &str, title: &str, body: &str, predecessor: Option<&str>, approved| {
        let mut c = json!({"kind": kind, "title": title, "body": body});
        if let Some(predecessor) = predecessor {
            c["supersedes"] = json!(predecessor);
        }
        f.store.save(&f.binding, &c, approved, None, None).unwrap()
    };
    let old = save(
        "decision",
        "Публикация релиза",
        "Публиковать в рабочее время.",
        None,
        true,
    );
    let current = save(
        "correction",
        "Публикация релиза",
        "Выпускать после согласования.",
        Some(id(&old)),
        true,
    );
    let draft = save(
        "decision",
        "Публикация релиза",
        "Выпускать ночью.",
        None,
        false,
    );
    let other = save(
        "decision",
        "Подсчёт запусков",
        "Проверять счётчик build_id.",
        None,
        true,
    );
    let search = |query: &str| f.store.search(&f.binding, query, 3).unwrap();
    let paraphrase = "Как согласовываются публикации релизов?";
    assert_eq!(search(paraphrase), json!([current]));
    assert_eq!(search("ПОДСЧЕТЕ"), json!([other]));
    assert_eq!(search("builder"), json!([]));
    assert_eq!(search("Настроить конференцию"), json!([]));
    let hook = f.hook(
        "UserPromptSubmit",
        "next",
        json!({
            "session_id":"new-session", "prompt": format!("/faust-mode {paraphrase}")
        }),
    );
    let context = hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains(id(&current)));
    for excluded in [&old, &draft, &other] {
        assert!(!context.contains(id(excluded)));
    }
    assert!(!context.contains(current["body"].as_str().unwrap()));
}

#[test]
fn exact_terms_rank_above_word_forms_and_newer_records_break_equal_scores() {
    let f = Fixture::new("auto", "pilot");
    let save = |title: &str, body| {
        f.store
            .save(
                &f.binding,
                &json!({"kind":"decision","title":title,"body":body}),
                true,
                None,
                None,
            )
            .unwrap()
    };
    let mut exact = [
        save("Публикации", "Первый пункт"),
        save("Публикации", "Второй пункт"),
    ];
    let inflected = save("Публикация", "Третий пункт");
    exact.sort_by(|a, b| id(a).cmp(id(b)));
    for (entry, when) in [
        (&exact[0], "2024-01-01"),
        (&exact[1], "2025-01-01"),
        (&inflected, "2026-01-01"),
    ] {
        let path = store::record(&f.root(), id(entry));
        let text = fs::read_to_string(&path).unwrap().replace(
            entry["created"].as_str().unwrap(),
            &format!("{when}T00:00:00+00:00"),
        );
        fs::write(path, text).unwrap();
    }
    let rows = f.store.search(&f.binding, "публикации", 3).unwrap();
    assert_eq!(
        rows.as_array().unwrap().iter().map(id).collect::<Vec<_>>(),
        [id(&exact[1]), id(&exact[0]), id(&inflected)]
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
fn search_and_recall_skip_invalid_record_bodies_without_reviving_predecessors() {
    let f = Fixture::new("auto", "pilot");
    let first = f.save("Original convention for heliotrope", true);
    let mut correction = candidate("Correction for heliotrope");
    correction["supersedes"] = first["id"].clone();
    let second = f
        .store
        .save(&f.binding, &correction, true, None, None)
        .unwrap();
    let other = f.save("Independent convention for heliotrope", true);
    let broken_draft = f.save("Draft for the first decision", false);
    let good_draft = f.save("Draft for the second decision", false);
    let path = store::record(&f.root(), id(&second));
    let raw = fs::read(&path).unwrap();
    let draft_path = store::record(&f.root(), id(&broken_draft));
    let draft_raw = fs::read(&draft_path).unwrap();
    for invalid_body in [vec![b'x'; 4001], vec![b'x'; 32768], vec![0xff]] {
        let mut edited = raw.clone();
        edited.extend(&invalid_body);
        fs::write(&path, edited).unwrap();
        let mut edited_draft = draft_raw.clone();
        edited_draft.extend(invalid_body);
        fs::write(&draft_path, edited_draft).unwrap();
        assert_eq!(
            f.store.search(&f.binding, "heliotrope", 5).unwrap(),
            json!([other])
        );
        assert_eq!(
            f.store.recall(&f.binding, "heliotrope", 5).unwrap(),
            json!([other])
        );
        assert_eq!(f.store.get(&f.binding, id(&other)).unwrap(), other);
        assert!(f.store.get(&f.binding, id(&first)).is_err());
        assert!(f.store.get(&f.binding, id(&second)).is_err());
        assert_eq!(f.store.inbox(&f.binding).unwrap(), json!([good_draft]));
    }
}
#[test]
fn hook_recall_defers_large_areas_and_cli_can_still_search_them() {
    let f = Fixture::new("auto", "pilot");
    let saved = f.save("Keep heliotrope", true);
    let records = f.root().join("Records");
    for n in 0..63 {
        let id = format!("{n:064x}");
        fs::write(
            records.join(format!("{id}-forgotten.json")),
            json!({"id":id}).to_string(),
        )
        .unwrap();
    }
    assert_eq!(
        f.store
            .recall_for_hook(&f.binding, "heliotrope", 3)
            .unwrap(),
        Some(json!([saved]))
    );
    f.legacy("Forgotten", &"f".repeat(64), None);
    assert_eq!(
        f.store
            .recall_for_hook(&f.binding, "heliotrope", 3)
            .unwrap(),
        None
    );
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 3).unwrap(),
        json!([saved])
    );
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
    assert!(f.store.get(&f.binding, id(&a)).is_err());
    assert!(f.store.get(&f.binding, id(&b)).is_ok());
    assert!(old.exists() && forgot.exists());
}
#[test]
fn project_page_selection_preserves_user_notes() {
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
    assert_eq!(fs::read_to_string(&old).unwrap(), raw);
    assert!(!named.exists());
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
    fs::write(f.binding.root.join("Projects.md"), "my hub").unwrap();
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
fn auto_memory_finishes_empty_turn_on_first_stop() {
    let f = Fixture::new("auto", "pilot");
    let issued = f.hook(
        "UserPromptSubmit",
        "question",
        json!({"prompt":"Объясни результат"}),
    );
    let context = issued["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("stage --turn"));
    assert!(!context.contains("--skip"));

    assert_eq!(f.hook("Stop", "question", json!({})), json!({}));
    assert_eq!(
        f.hook("Stop", "question", json!({"stop_hook_active":true})),
        json!({})
    );

    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert_eq!(
        f.pending().diagnostics(&scope).unwrap(),
        json!({"phase":"skipped","staged":0,"pending":0})
    );
}
#[test]
fn hook_requires_exact_invocation_and_applies_only_one_operation() {
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
    for overrides in [
        json!({"tool_name":"exec_command"}),
        json!({"agent_id":"child"}),
        json!({"tool_response":{"staged":true}}),
        json!({"tool_input":{"command":command,"timeout":10}}),
        json!({"tool_use_id":"bad id"}),
    ] {
        assert_eq!(post(&f, "one", command.clone(), overrides), json!({}));
    }
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 0);
    assert_eq!(post(&f, "one", command.clone(), json!({})), json!({}));
    let different = stage_command(&token, &json!({"action":"skip"}));
    assert_eq!(post(&f, "one", different, json!({}))["decision"], "block");
    assert_eq!(f.hook("Stop", "one", json!({})), json!({}));
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
}
#[test]
fn hook_guards_binding_changes_and_expired_turns() {
    let f = Fixture::new("auto", "pilot");
    for extra in [
        json!({"permission_mode":"plan"}),
        json!({"collaboration_mode":{"mode":"plan"}}),
        json!({"mode":"plan"}),
    ] {
        let result = f.hook("UserPromptSubmit", "guard", extra);
        let context = result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("Faust-mode выключен"));
        assert!(!context.contains("--turn"));
    }
    for extra in [
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
    assert_eq!(f.hook("Stop", "steered", json!({})), json!({}));
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
    let request = json!({"action":"remember","candidate":candidate("Review heliotrope")});
    f.token("unstaged");
    assert_eq!(
        f.hook(
            "Stop",
            "unstaged",
            json!({"last_assistant_message":request.to_string()})
        ),
        json!({})
    );
    assert_eq!(f.store.inbox(&f.binding).unwrap(), json!([]));

    let token = f.token("draft");
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert!(f.pending().is_current(&token, &scope).unwrap());
    post(&f, "draft", stage_command(&token, &request), json!({}));
    assert_eq!(f.hook("Stop", "draft", json!({})), json!({}));
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
    assert!(!f
        .pending()
        .claim(
            &key,
            &scope,
            "session",
            "one",
            "",
            pending::Delivery::Current {
                first_stop: true,
                confirmed: false
            }
        )
        .unwrap());
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
        .issue(&"a".repeat(64), "scope", None, "", "session", "one")
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
    let prior_path = scope::binding_path(&f.store.state, &f.repo).unwrap();
    let prior = f
        .store
        .setup(&f.vault, &f.repo, "pilot", Some("inbox"))
        .unwrap();
    let saved = f
        .store
        .save(
            &prior,
            &candidate("A remembered convention"),
            true,
            None,
            None,
        )
        .unwrap();
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
    fs::create_dir(nested.join(".git")).unwrap();
    for cwd in [&nested, &f.repo] {
        let restored = f.store.auto_resolve(cwd).unwrap().unwrap();
        assert_eq!(restored.project, "pilot");
        assert_eq!(restored.mode, "inbox");
        assert_eq!(restored.generation, prior.generation);
        assert_eq!(f.store.inbox(&restored).unwrap(), json!([saved]));
        assert_eq!(
            scope::binding_path(&f.store.state, cwd).unwrap(),
            prior_path
        );
    }
    assert_eq!(
        scope::binding_location(&f.store.state, &f.repo)
            .unwrap()
            .identity,
        scope::binding_location(&f.store.state, &nested)
            .unwrap()
            .identity
    );
    for worktree in [&f.repo, &linked] {
        let state_in_worktree = worktree.join("profile/fstack/memory");
        assert!(io::profile_state(&state_in_worktree, true).is_err());
        assert!(!state_in_worktree.exists());
    }
    let forged = f.base.join("forged");
    fs::create_dir(&forged).unwrap();
    fs::copy(linked.join(".git"), forged.join(".git")).unwrap();
    assert!(f.store.resolve(&forged).is_err());
    assert!(f.store.disable(&linked).unwrap());
    assert!(f.store.auto_resolve(&f.repo).unwrap().is_none());
    assert!(f.store.auto_resolve(&linked).unwrap().is_none());
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
fn record_limit_is_checked_on_insert_and_existing_history_stays_readable() {
    let f = Fixture::new("auto", "pilot");
    let root = f.root();
    let records = root.join("Records");
    let saved = f.save("Keep readable heliotrope", true);
    let draft = f.save("Draft for capacity test", false);
    let contents = fs::read_to_string(store::record(&root, id(&draft))).unwrap();
    let synced_draft = |n| {
        let synced_id = format!("{n:064x}");
        fs::write(
            store::record(&root, &synced_id),
            contents.replace(id(&draft), &synced_id),
        )
        .unwrap();
    };
    for n in 0..4093 {
        synced_draft(n);
    }
    fs::write(records.join("Personal note.md"), "Other vault data").unwrap();
    let last = f.save("Last writable record", true);
    let new = candidate("Next writable record");
    let insert = || f.store.save(&f.binding, &new, true, None, None);
    assert!(insert().is_err());
    assert_eq!(f.save("Last writable record", true)["id"], last["id"]);
    assert_eq!(store::files(&records, false).unwrap().len(), 4096);

    synced_draft(4093);
    assert_eq!(store::files(&records, false).unwrap().len(), 4097);
    assert_eq!(f.store.get(&f.binding, id(&saved)).unwrap(), saved);
    assert_eq!(f.store.search(&f.binding, "readable", 1).unwrap()[0], saved);
    assert!(insert().is_err());
    f.store.forget(&f.binding, id(&draft)).unwrap();
    assert!(insert().is_err());
    f.store.forget(&f.binding, id(&last)).unwrap();
    assert_eq!(insert().unwrap()["status"], "memory");
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
fn legacy_tombstones_respect_new_confirmation_without_rewriting() {
    let f = Fixture::new("auto", "pilot");
    let first = f.save("First heliotrope", true);
    let second = f.save("Second heliotrope", true);
    let tomb = store::record(&f.root(), id(&second))
        .with_file_name(format!("{}-forgotten.json", id(&second)));
    fs::write(&tomb, json!({"id":id(&second)}).to_string()).unwrap();
    f.legacy("Forgotten", id(&second), Some(id(&first)));
    assert!(f.store.get(&f.binding, id(&first)).is_err());
    let v: Value = serde_json::from_str(&fs::read_to_string(&tomb).unwrap()).unwrap();
    assert!(v.get("supersedes").is_none());
    fs::write(
        &tomb,
        json!({"id":id(&second),"supersedes":null}).to_string(),
    )
    .unwrap();
    let v: Value = serde_json::from_str(&fs::read_to_string(&tomb).unwrap()).unwrap();
    assert!(v["supersedes"].is_null());
    assert!(f.store.get(&f.binding, id(&first)).is_ok());
}
#[cfg(windows)]
#[test]
fn windows_pending_journal_distinguishes_commit_from_hard_links() {
    let f = Fixture::new("auto", "pilot");
    f.token("seed");
    let db = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
    let journal = f.store.state.join("pending.sqlite3-journal");
    db.execute_batch("BEGIN IMMEDIATE; INSERT INTO diagnostics VALUES ('probe','test',0)")
        .unwrap();
    let handle = fs::File::open(&journal).unwrap();
    assert_eq!(io::windows_info_by_handle(&handle).unwrap().unwrap().2, 1);
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(io::windows_info_by_handle(&handle).unwrap(), None);
    drop(handle);

    db.execute_batch("BEGIN IMMEDIATE; DELETE FROM diagnostics WHERE scope='probe'")
        .unwrap();
    fs::hard_link(&journal, f.base.join("second-link")).unwrap();
    assert_eq!(
        f.pending().diagnostics("probe").unwrap_err(),
        "Unsafe pending memory journal"
    );
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
    assert!(!f
        .pending()
        .claim(
            &key,
            &scope,
            "session",
            "one",
            "",
            pending::Delivery::Current {
                first_stop: true,
                confirmed: false
            }
        )
        .unwrap());
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
        .issue(&"a".repeat(64), "scope", None, "", "session", "one")
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
    let result = f.hook(
        "UserPromptSubmit",
        "one",
        json!({"prompt":"/faust-mode heliotrope"}),
    );
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
    let result = f.hook(
        "UserPromptSubmit",
        "one",
        json!({"prompt":"/faust-mode heliotrope"}),
    );
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
    let disabled = f.hook("UserPromptSubmit", "disabled", json!({"prompt":"Продолжи"}));
    assert!(disabled["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .is_some_and(|context| !context.contains("stage --turn")));
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
        json!({"prompt":"/faust-mode heliotrope"}),
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
fn cross_profile_locking_hides_forgotten_records() {
    let f = Fixture::new("inbox", "pilot");
    let v = f.save("Current heliotrope", false);
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

fn stage_action(f: &Fixture, turn: &str, request: &Value) {
    let token = f.token(turn);
    assert_eq!(
        post(f, turn, stage_command(&token, request), json!({})),
        json!({})
    );
}

fn read_memory_epoch(f: &Fixture) -> String {
    f.pending()
        .session_epoch("session", None, false)
        .unwrap()
        .unwrap()
}

#[test]
fn claimed_stop_recovers_after_project_unavailable_in_a_later_turn() {
    let f = Fixture::new("auto", "pilot");
    let scope = hooks::scope_id(&f.binding, &f.repo);
    let request = json!({"action":"remember","candidate":candidate("Recovery heliotrope")});
    stage_action(&f, "original", &request);
    assert_eq!(
        f.pending().diagnostics(&scope).unwrap(),
        json!({"phase":"stage","staged":1,"pending":0})
    );
    let absent = f.root().with_file_name("unavailable");
    fs::rename(f.root(), &absent).unwrap();
    assert!(f
        .hook("Stop", "original", json!({}))
        .get("systemMessage")
        .is_some());
    assert_eq!(
        f.pending().diagnostics(&scope).unwrap(),
        json!({"phase":"failure","staged":0,"pending":1})
    );
    fs::rename(absent, f.root()).unwrap();
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );

    for (session, turn, mode) in [("other", "foreign", "default"), ("session", "plan", "plan")] {
        f.hook(
            "UserPromptSubmit",
            turn,
            json!({"session_id":session,"mode":mode,"prompt":"/faust-mode"}),
        );
        f.hook("Stop", turn, json!({"session_id":session,"mode":mode}));
        assert_eq!(
            f.store.search(&f.binding, "heliotrope", 5).unwrap(),
            json!([])
        );
    }

    stage_action(&f, "later", &json!({"action":"skip"}));
    assert_eq!(f.hook("Stop", "later", json!({})), json!({}));
    let result = f.store.search(&f.binding, "heliotrope", 5).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["turn_id"], "original");
    assert_eq!(result[0]["session_id"], "session");
    assert_eq!(
        f.pending().diagnostics(&scope).unwrap(),
        json!({"phase":"applied","staged":0,"pending":0})
    );
    assert_eq!(f.hook("Stop", "original", json!({})), json!({}));
}

#[test]
fn uncertain_stop_commit_is_idempotent_across_concurrent_retries() {
    let f = Fixture::new("auto", "pilot");
    let scope = hooks::scope_id(&f.binding, &f.repo);
    let original = "original";
    let key = pending::digest(&["session", original]);
    let epoch = read_memory_epoch_after_stage(&f, original);
    assert!(f
        .pending()
        .claim(
            &key,
            &scope,
            "session",
            original,
            &epoch,
            pending::Delivery::Current {
                first_stop: true,
                confirmed: true
            }
        )
        .unwrap());
    let committed = f
        .store
        .save(
            &f.binding,
            &candidate("Repeat heliotrope"),
            true,
            Some("session"),
            Some(original),
        )
        .unwrap();
    let path = store::record(&f.root(), id(&committed));
    let bytes = fs::read(&path).unwrap();
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["pending"], 1);

    std::thread::scope(|threads| {
        let a = threads.spawn(|| f.hook("Stop", original, json!({})));
        let b = threads.spawn(|| f.hook("Stop", "different", json!({})));
        assert_eq!(a.join().unwrap(), json!({}));
        assert_eq!(b.join().unwrap(), json!({}));
    });
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["pending"], 0);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([committed])
    );
}

fn read_memory_epoch_after_stage(f: &Fixture, turn: &str) -> String {
    stage_action(
        f,
        turn,
        &json!({"action":"remember","candidate":candidate("Repeat heliotrope")}),
    );
    read_memory_epoch(f)
}

#[test]
fn an_accepted_action_cannot_cross_revocation_or_binding_generation() {
    for (revoke, claimed) in ["steer", "plan", "binding", "expired"]
        .into_iter()
        .flat_map(|revoke| [(revoke, true), (revoke, false)])
    {
        let f = Fixture::new("auto", "pilot");
        let scope = hooks::scope_id(&f.binding, &f.repo);
        let epoch = read_memory_epoch_after_stage(&f, "original");
        let key = pending::digest(&["session", "original"]);
        if claimed {
            assert!(f
                .pending()
                .claim(
                    &key,
                    &scope,
                    "session",
                    "original",
                    &epoch,
                    pending::Delivery::Current {
                        first_stop: true,
                        confirmed: true
                    }
                )
                .unwrap());
        }
        match revoke {
            "steer" => {
                f.token("original");
            }
            "plan" => {
                f.hook("Stop", "original", json!({"mode":"plan"}));
            }
            "binding" => {
                f.store.disable(&f.repo).unwrap();
                f.store
                    .setup(&f.vault, &f.repo, "pilot", Some("auto"))
                    .unwrap();
            }
            "expired" => {
                let conn =
                    rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
                let table = if claimed { "claimed" } else { "pending" };
                conn.execute(&format!("UPDATE {table} SET expires=0"), [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        stage_action(&f, "later", &json!({"action":"skip"}));
        f.hook("Stop", "later", json!({}));
        assert_eq!(
            f.store.search(&f.binding, "heliotrope", 5).unwrap(),
            json!([]),
            "{revoke}"
        );
    }
}

#[test]
fn interrupted_approval_or_write_cannot_restore_forgotten_content() {
    for action in ["remember", "approve"] {
        let f = Fixture::new("auto", "pilot");
        let candidate = candidate("Forget heliotrope");
        let saved = f
            .store
            .save(&f.binding, &candidate, action == "remember", None, None)
            .unwrap();
        let operation = if action == "approve" {
            json!({"action":"approve","id":saved["id"]})
        } else {
            json!({"action":"remember","candidate":candidate})
        };
        let scope = hooks::scope_id(&f.binding, &f.repo);
        stage_action(&f, "original", &operation);
        let epoch = read_memory_epoch(&f);
        let key = pending::digest(&["session", "original"]);
        assert!(f
            .pending()
            .claim(
                &key,
                &scope,
                "session",
                "original",
                &epoch,
                pending::Delivery::Current {
                    first_stop: true,
                    confirmed: true
                }
            )
            .unwrap());
        if action == "approve" {
            f.store.approve(&f.binding, id(&saved)).unwrap();
        }
        f.store.forget(&f.binding, id(&saved)).unwrap();
        assert_eq!(f.hook("Stop", "original", json!({})), json!({}));
        assert_eq!(
            f.pending().diagnostics(&scope).unwrap(),
            json!({
                "phase":"discarded_forgotten","staged":0,"pending":0
            })
        );
        let conn = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM claimed", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
        assert_eq!(
            f.store.search(&f.binding, "heliotrope", 5).unwrap(),
            json!([])
        );
        assert!(f.store.history(&f.binding, id(&saved), 5).is_err());
        assert!(!store::record(&f.root(), id(&saved)).exists());
    }
}

#[test]
fn history_links_scoped_approved_versions_and_diffs_without_forgotten_content() {
    let f = Fixture::new("auto", "pilot");
    let a = f
        .store
        .save(
            &f.binding,
            &json!({
                "kind":"decision","title":"Heliotrope convention","body":"Default\nUbuntu\nRelease",
                "source":"workflow.yml @ v1",
            }),
            true,
            None,
            None,
        )
        .unwrap();
    let b = f.store.save(&f.binding, &json!({
        "kind":"correction","title":"Heliotrope convention","body":"Default\nWindows\nRelease",
        "source":"workflow.yml @ v2","supersedes":id(&a),
    }), true, Some("session"), Some("update")).unwrap();
    let draft = f.store.save(&f.binding, &json!({
        "kind":"correction","title":"Heliotrope convention","body":"A proposal","supersedes":id(&b),
    }), false, None, None).unwrap();
    assert_eq!(
        f.store.search(&f.binding, "Heliotrope", 5).unwrap(),
        json!([b])
    );
    assert!(f.store.get(&f.binding, id(&a)).is_err());
    assert!(f.store.history(&f.binding, id(&draft), 5).is_err());
    assert!(f.store.history(&f.binding, id(&b), 11).is_err());
    let from_new = f.store.history(&f.binding, id(&b), 5).unwrap();
    let from_old = f.store.history(&f.binding, id(&a), 5).unwrap();
    assert_eq!(from_new["entries"], json!([b, a]));
    assert_eq!(from_old["entries"], json!([a, b]));
    assert_eq!(from_new["changes"], from_old["changes"]);
    assert_eq!(
        from_new["changes"][0],
        json!({
            "from":id(&a),"to":id(&b),"fields":{
                "kind":{"from":"decision","to":"correction"},
                "source":{"from":"workflow.yml @ v1","to":"workflow.yml @ v2"},
                "body":{"start_line":2,"removed":["Ubuntu"],"added":["Windows"]},
            }
        })
    );
    assert_eq!(
        f.store.history(&f.binding, id(&b), 1).unwrap(),
        json!({"entries":[b],"changes":[],"truncated":true})
    );
    let foreign = f
        .store
        .setup(&f.vault, &f.repo, "other", Some("auto"))
        .unwrap();
    assert!(f.store.history(&foreign, id(&a), 5).is_err());

    let stale = fs::read(store::record(&f.root(), id(&a))).unwrap();
    f.store.forget(&f.binding, id(&a)).unwrap();
    fs::write(store::record(&f.root(), id(&a)), stale).unwrap();
    assert!(f.store.history(&f.binding, id(&a), 5).is_err());
    let after_forget = f.store.history(&f.binding, id(&b), 5).unwrap();
    assert_eq!(after_forget["entries"].as_array().unwrap().len(), 1);
    assert_eq!(after_forget["changes"], json!([]));
    assert!(!after_forget.to_string().contains("Ubuntu"));
    assert!(!after_forget.to_string().contains(id(&a)));
}

#[test]
fn active_legacy_queue_keeps_its_epoch_after_the_coordinator_is_turned_off() {
    let f = Fixture::new("auto", "pilot");
    stage_action(
        &f,
        "old",
        &json!({"action":"remember","candidate":candidate("Legacy heliotrope")}),
    );
    let key = pending::digest(&["session"]);
    let legacy = io::nonce().unwrap();
    fs::write(
        f.store.state.join("faust-mode-sessions").join(&key),
        &legacy,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
    conn.execute(
        "UPDATE issued_modes SET epoch=? WHERE session='session'",
        [&legacy],
    )
    .unwrap();
    drop(conn);

    let off = f.hook(
        "UserPromptSubmit",
        "off",
        json!({"prompt":"/faust-mode-off"}),
    );
    let context = off["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("выключен"));
    assert!(context.contains("stage --turn"));
    assert_eq!(read_memory_epoch(&f), legacy);
    f.hook("Stop", "old", json!({}));
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap()[0]["turn_id"],
        "old"
    );
}

#[test]
fn old_pending_rows_and_empty_mode_markers_survive_upgrade() {
    let f = Fixture::new("auto", "pilot");
    let path = scope::binding_path(&f.store.state, &f.repo).unwrap();
    let legacy_binding =
        json!({"root":f.binding.root,"project":f.binding.project,"mode":f.binding.mode});
    fs::write(path, legacy_binding.to_string()).unwrap();
    f.token("bootstrap");
    let marker = f
        .store
        .state
        .join("faust-mode-sessions")
        .join(pending::digest(&["session"]));
    fs::write(marker, "").unwrap();
    let request = json!({"action":"remember","candidate":candidate("Legacy heliotrope")});
    stage_action(&f, "old", &request);
    let conn = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
    conn.execute_batch("DROP TABLE issued_modes; DROP TABLE claimed; DROP TABLE diagnostics")
        .unwrap();
    drop(conn);
    fs::remove_file(
        f.store
            .state
            .join("memory-sessions")
            .join(pending::digest(&["session"])),
    )
    .unwrap();

    assert_eq!(f.hook("Stop", "new", json!({})), json!({}));
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 5).unwrap(),
        json!([])
    );
    assert_eq!(f.hook("Stop", "old", json!({})), json!({}));
    let records = f.store.search(&f.binding, "heliotrope", 5).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["turn_id"], "old");
}

#[test]
fn later_stop_recovers_trusted_stage_when_original_stop_never_committed_claim() {
    for failed_stop in [true, false] {
        let f = Fixture::new("auto", "pilot");
        let scope = hooks::scope_id(&f.binding, &f.repo);
        let request =
            json!({"action":"remember","candidate":candidate("Recover preclaim heliotrope")});
        f.token("capture-only");
        stage_action(&f, "original", &request);
        assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 1);
        if failed_stop {
            let conn = rusqlite::Connection::open(f.store.state.join("pending.sqlite3")).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            let event = json!({"hook_event_name":"Stop","session_id":"session","turn_id":"original","cwd":f.repo,"stop_hook_active":false});
            assert!(hooks::hook(&f.store, "Stop", &event, &f.repo).is_err());
            conn.execute_batch("ROLLBACK").unwrap();
        }

        let foreign = f.base.join("worktree");
        fs::create_dir(&foreign).unwrap();
        f.store
            .setup(&f.vault, &foreign, "pilot", Some("auto"))
            .unwrap();
        for (session, cwd, mode) in [
            ("other", &f.repo, "default"),
            ("session", &foreign, "default"),
            ("session", &f.repo, "plan"),
        ] {
            let event = json!({"hook_event_name":"UserPromptSubmit","session_id":session,"turn_id":"foreign","cwd":cwd,"prompt":"/faust-mode","mode":mode});
            hooks::hook(&f.store, "UserPromptSubmit", &event, cwd).unwrap();
            let event = json!({"hook_event_name":"Stop","session_id":session,"turn_id":"foreign","cwd":cwd,"mode":mode,"stop_hook_active":false});
            hooks::hook(&f.store, "Stop", &event, cwd).unwrap();
            assert_eq!(
                f.store.search(&f.binding, "heliotrope", 5).unwrap(),
                json!([])
            );
        }
        let absent = f.hook("Stop", "new", json!({"agent_id":"child"}));
        assert_eq!(absent, json!({}));
        assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 1);
        assert_eq!(f.hook("Stop", "new", json!({})), json!({}));
        let entries = f.store.search(&f.binding, "heliotrope", 5).unwrap();
        assert_eq!(entries.as_array().unwrap().len(), 1);
        assert_eq!(entries[0]["turn_id"], "original");
        assert_eq!(entries[0]["session_id"], "session");
        assert_eq!(
            f.pending().diagnostics(&scope).unwrap(),
            json!({"phase":"applied","staged":0,"pending":0})
        );
        assert_eq!(f.hook("Stop", "capture-only", json!({})), json!({}));
        assert_eq!(f.hook("Stop", "original", json!({})), json!({}));
        assert_eq!(
            f.store.search(&f.binding, "heliotrope", 5).unwrap(),
            entries
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_publish_preserves_atomic_new_and_replace_for_long_unicode_paths() {
    use std::os::windows::ffi::OsStrExt;
    let f = Fixture::new("auto", "pilot");
    let component = "проверка".repeat(8);
    let mut parent = f.vault.join(&component);
    let filename = "заметка.md";
    while parent.join(filename).as_os_str().encode_wide().count() < 260 {
        parent.push(&component);
    }
    fs::create_dir_all(&parent).unwrap();
    let target = parent.join(filename);
    assert!(target.as_os_str().encode_wide().count() >= 260);
    assert!(io::write(&target, "one", false, None).unwrap());
    assert!(!io::write(&target, "lost", false, None).unwrap());
    assert_eq!(io::read(&target).unwrap().as_deref(), Some("one"));
    assert!(io::write(&target, "two", true, Some("one")).unwrap());
    assert_eq!(io::read(&target).unwrap().as_deref(), Some("two"));

    for (path, expected) in [
        (
            r"\\server\share\каталог\new.md",
            r"\\?\UNC\server\share\каталог\new.md",
        ),
        (
            r"//server/share/каталог/new.md",
            r"\\?\UNC\server\share\каталог\new.md",
        ),
        (
            r"\\server/share/каталог/new.md",
            r"\\?\UNC\server\share\каталог\new.md",
        ),
        (r"каталог/new.md", r"каталог/new.md"),
        (r"\\?\C:\каталог\new.md", r"\\?\C:\каталог\new.md"),
        (r"C:/каталог/new.md", r"\\?\C:\каталог\new.md"),
    ] {
        let units = io::windows_publish_path(Path::new(path));
        assert_eq!(
            units,
            expected.encode_utf16().chain(Some(0)).collect::<Vec<_>>()
        );
    }
}

#[test]
fn corrupt_tombstone_retains_accepted_write_until_repaired() {
    for action in ["remember", "approve"] {
        let f = Fixture::new("auto", "pilot");
        let candidate = candidate("Repair heliotrope");
        let target = format::stable_id(&format::candidate(&candidate).unwrap());
        let request = if action == "approve" {
            f.store
                .save(&f.binding, &candidate, false, None, None)
                .unwrap();
            json!({"action":"approve","id":target})
        } else {
            json!({"action":"remember","candidate":candidate})
        };
        let scope = hooks::scope_id(&f.binding, &f.repo);
        stage_action(&f, "original", &request);
        let marker = f
            .root()
            .join("Records")
            .join(format!("{target}-forgotten.json"));
        fs::write(&marker, "").unwrap();
        assert!(f
            .hook("Stop", "original", json!({}))
            .get("systemMessage")
            .is_some());
        assert_eq!(
            f.pending().diagnostics(&scope).unwrap(),
            json!({
                "phase":"failure","staged":0,"pending":1
            })
        );
        assert!(f.store.search(&f.binding, "heliotrope", 5).is_err());

        fs::remove_file(marker).unwrap();
        assert_eq!(f.hook("Stop", "next", json!({})), json!({}));
        let entries = f.store.search(&f.binding, "heliotrope", 5).unwrap();
        assert_eq!(entries.as_array().unwrap().len(), 1);
        assert_eq!(entries[0]["id"], target);
        assert_eq!(
            f.pending().diagnostics(&scope).unwrap(),
            json!({
                "phase":"applied","staged":0,"pending":0
            })
        );
    }
}

#[test]
fn unknown_mode_stop_cannot_replay_an_older_action_without_current_turn_confirmation() {
    let f = Fixture::new("auto", "pilot");
    stage_action(
        &f,
        "previous",
        &json!({"action":"remember","candidate":candidate("Deferred heliotrope")}),
    );
    let unknown = json!({"permission_mode":"default", "collaboration_mode":null});
    f.hook(
        "UserPromptSubmit",
        "plan",
        json!({"permission_mode":"default","collaboration_mode":null,"prompt":"Предложи план"}),
    );
    assert_eq!(f.hook("Stop", "plan", unknown.clone()), json!({}));
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert_eq!(
        f.pending().diagnostics(&scope).unwrap()["phase"],
        "unconfirmed"
    );
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 3).unwrap(),
        json!([])
    );
    stage_action(&f, "ordinary", &json!({"action":"skip"}));
    assert_eq!(f.hook("Stop", "ordinary", unknown), json!({}));
    assert_eq!(
        f.store.search(&f.binding, "heliotrope", 3).unwrap()[0]["turn_id"],
        "previous"
    );
}

#[test]
fn invalid_profile_layout_blocks_only_when_an_existing_generation_must_be_revoked() {
    let f = Fixture::new("auto", "pilot");
    let versioned = f.base.join("versioned");
    let marker = versioned.join(".git");
    fs::create_dir_all(&marker).unwrap();
    let state_in_marker = marker.join("fstack/memory");
    assert!(io::profile_state(&state_in_marker, true).is_err());
    assert!(!state_in_marker.exists());
    fs::write(marker.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let state = versioned.join("absent-profile/fstack/memory");
    let store = Store::new(Some(state.clone()), &f.store.package).unwrap();
    let event = json!({"hook_event_name":"UserPromptSubmit","session_id":"session","turn_id":"test",
        "cwd":f.repo,"permission_mode":"default","prompt":"Продолжи работу"});
    for (hook, prompt, status, index) in [
        ("SessionStart", "", "Режим текущего запроса", false),
        ("UserPromptSubmit", "/faust-mode", "на этот запрос", true),
        ("UserPromptSubmit", "Продолжи", "Faust-mode выключен", false),
        (
            "UserPromptSubmit",
            "/faust-mode-off",
            "Faust-mode выключен",
            false,
        ),
    ] {
        let mut payload = event.clone();
        payload["hook_event_name"] = json!(hook);
        payload["prompt"] = json!(prompt);
        let result = hooks::hook(&store, hook, &payload, &f.repo).unwrap();
        let context = result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains(status));
        assert_eq!(context.contains("model-check —"), index);
        assert!(!context.contains("--turn"));
        assert!(result["systemMessage"].is_string());
        assert!(result.get("decision").is_none());
    }
    assert!(!state.exists());
    let generations = state.join("pending-generations");
    fs::create_dir_all(&generations).unwrap();
    fs::write(
        generations.join(pending::digest(&["session", "test"])),
        io::nonce().unwrap(),
    )
    .unwrap();
    let result = hooks::hook(&store, "UserPromptSubmit", &event, &f.repo).unwrap();
    assert_eq!(result["decision"], "block");
}

#[test]
fn workspace_containing_the_profile_never_blocks_ordinary_prompts_or_creates_state() {
    let f = Fixture::new("auto", "pilot");
    let cwd = f.base.join("home");
    fs::create_dir(&cwd).unwrap();
    let state = cwd.join("profile/fstack/memory");
    let store = Store::new(Some(state.clone()), &f.store.package).unwrap();
    for path in [cwd.as_path(), cwd.ancestors().last().unwrap()] {
        for (event, prompt) in [
            ("UserPromptSubmit", "Исправь тесты"),
            ("Stop", ""),
            ("PostToolUse", ""),
        ] {
            let payload = json!({"hook_event_name":event,"session_id":"ordinary","turn_id":"test","cwd":path,"permission_mode":"default","prompt":prompt,"stop_hook_active":false});
            let result = hooks::hook(&store, event, &payload, path).unwrap();
            if event == "UserPromptSubmit" {
                assert!(result["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap()
                    .contains("Faust-mode выключен"));
                assert!(!result.to_string().contains("--turn"));
            } else {
                assert_eq!(result, json!({}));
            }
        }
        let activation = json!({"hook_event_name":"UserPromptSubmit","session_id":"s","turn_id":"t","cwd":path,"permission_mode":"default","prompt":"/faust-mode Сделай задачу"});
        let response = hooks::hook(&store, "UserPromptSubmit", &activation, path).unwrap();
        assert!(response["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("faust-mode"));
        assert!(!state.exists());
    }
}

#[test]
fn short_skip_requires_a_validated_standalone_command_and_leaves_no_note() {
    let f = Fixture::new("auto", "pilot");
    let token = f.token("quiet");
    let packed = stage_command(&token, &json!({"action":"skip"}));
    let (prefix, _) = packed.split_once(" --payload ").unwrap();
    let skip = format!("{prefix} --skip");
    for invalid in [
        format!("{skip}; true"),
        format!("{skip} "),
        format!("{skip} reason"),
    ] {
        post(&f, "quiet", invalid, json!({}));
    }
    post(&f, "quiet", skip.clone(), json!({"agent_type":"worker"}));
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 0);
    post(&f, "quiet", skip, json!({}));
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 1);
    assert_eq!(f.hook("Stop", "quiet", json!({})), json!({}));
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["phase"], "skipped");
    assert_eq!(
        std::fs::read_dir(f.root().join("Records")).unwrap().count(),
        0
    );
}

#[test]
fn correction_requires_a_single_literal_command_in_an_authorized_turn() {
    let f = Fixture::new("inbox", "reviewed");
    let token = f.token("fix");
    let packed = stage_command(&token, &json!({"action": "skip"}));
    let (prefix, _) = packed.split_once(" --payload ").unwrap();
    let cmd = format!("{prefix} --correction 'Runner' 'Use uv run pytest'");
    for invalid in [
        format!("{cmd}; echo done"),
        format!("{cmd} --supersedes not-an-id"),
        format!("{prefix} --correction '$(ignored)' 'body' && true"),
        format!("{prefix} --correction 'Runner' 'Do' 'another thing'"),
        format!("{prefix} --correction 'Runner' 'Don’t change an argument’ ; echo bad #'"),
        format!("{prefix} --correction 'Runner' 'api_key=not-for-memory'"),
        format!("{prefix} --correction 'Runner' 'multiple\nlines'"),
        format!("echo {cmd}"),
    ] {
        post(&f, "fix", invalid, json!({}));
    }
    for overrides in [
        json!({"agent_id":"child"}),
        json!({"tool_name":"unrelated"}),
        json!({"tool_response":"not validated"}),
        json!({"session_id":"unrelated"}),
    ] {
        post(&f, "fix", cmd.clone(), overrides);
    }
    let scope = hooks::scope_id(&f.binding, &f.repo);
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 0);
    post(&f, "fix", cmd, json!({}));
    assert_eq!(f.hook("Stop", "fix", json!({})), json!({}));
    assert_eq!(f.store.search(&f.binding, "pytest", 5).unwrap(), json!([]));
    let inbox = f.store.inbox(&f.binding).unwrap();
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    assert_eq!(inbox[0]["title"], "Runner");
    assert_eq!(inbox[0]["kind"], "correction");
}

#[test]
fn double_quoted_corrections_retry_expansions_without_staging_or_changing_other_turns() {
    let f = Fixture::new("auto", "pilot");
    let token = f.token("review");
    let packed = stage_command(&token, &json!({"action":"skip"}));
    let (prefix, _) = packed.split_once(" --payload ").unwrap();
    let scope = hooks::scope_id(&f.binding, &f.repo);
    for body in [
        "Use $PWD",
        "Use `pwd`",
        r"Use C:\Temp",
        "Use !history",
        "Use %TEMP%",
    ] {
        let cmd = format!(r#"{prefix} --correction "Проверки" "{body}""#);
        let response = post(&f, "review", cmd.clone(), json!({}));
        assert!(response["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("stage --payload"));
        assert_eq!(
            post(&f, "review", format!("{cmd}; echo done"), json!({})),
            json!({})
        );
        assert_eq!(
            post(&f, "review", cmd.clone(), json!({"agent_id":"worker"})),
            json!({})
        );
        f.token("other");
        assert_eq!(post(&f, "other", cmd, json!({})), json!({}));
    }
    assert_eq!(f.pending().diagnostics(&scope).unwrap()["staged"], 0);
    let cmd = format!(
        r#"{prefix} --correction "Заголовок" "В разделе «Проверки» указывай команду и результат.""#
    );
    assert_eq!(post(&f, "review", cmd, json!({})), json!({}));
    assert_eq!(f.hook("Stop", "review", json!({})), json!({}));
    assert_eq!(
        f.store.search(&f.binding, "Заголовок", 3).unwrap()[0]["body"],
        "В разделе «Проверки» указывай команду и результат."
    );
}
#[test]
fn a_new_chat_considers_a_project_rule_without_topic_overlap() {
    let f = Fixture::new("auto", "sample");
    let rule = json!({"kind":"correction","title":"Язык ответов","body":"В этом проекте всегда отвечай по-русски."});
    let saved = f.store.save(&f.binding, &rule, true, None, None).unwrap();
    let query = "Explain binary search";
    assert_eq!(f.store.search(&f.binding, query, 3).unwrap(), json!([]));
    assert_eq!(
        f.store.recall(&f.binding, query, 3).unwrap()[0]["id"],
        saved["id"]
    );
    f.store
        .forget(&f.binding, saved["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(f.store.recall(&f.binding, query, 3).unwrap(), json!([]));
}
