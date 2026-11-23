use super::*;
use std::{collections::BTreeSet, fs};

fn package() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

struct Fixture {
    _temporary: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    state: State,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let repo = base.join("repo");
        fs::create_dir(&repo).unwrap();
        let state = State::new(base.join("profile")).unwrap();
        Self {
            _temporary: temporary,
            base,
            repo,
            state,
        }
    }

    fn hook(&self, event: &str, fields: Value) -> Value {
        let mut payload = json!({"hook_event_name":event,"session_id":"session","turn_id":"turn","cwd":self.repo});
        payload
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        hook(&self.state, package(), event, &payload, &self.repo).unwrap()
    }

    fn context(&self, event: &str, fields: Value) -> String {
        self.hook(event, fields)["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn submit(&self, prompt: &str) -> String {
        self.context("UserPromptSubmit", json!({"prompt":prompt}))
    }
}

#[test]
fn ordinary_prompts_and_quoted_commands_do_not_activate_the_coordinator() {
    let f = Fixture::new();
    for prompt in [
        "Исправь ошибку",
        "Объясни /faust-mode",
        "`/faust-mode`",
        "> /faust-mode task",
        "[$fstack:faust-mode](/projects/tools(qa)/SKILL.md",
        "[$fstack:faust-mode](/projects/tools(qa)/SKILL.md)example",
        "Проверь инструкции с примерами $fstack:faust-mode и $fstack:debug",
        "$fstack:debug task",
    ] {
        let context = f.submit(prompt);
        assert!(context.contains("Faust-mode выключен"));
        assert!(!context.contains("SKILL.md"));
        assert!(!context.contains("model-check —"));
    }
    assert!(!f.state.directory.exists());
}

#[test]
fn aliases_are_explicit_and_chat_state_is_isolated() {
    let f = Fixture::new();
    for prompt in [
        "/faust-mode task",
        " $fstack:faust-mode task",
        "$faust-mode\ntask",
        "[$fstack:faust-mode](C:/Codex Home/skills/faust-mode/SKILL.md) task",
        "[$fstack:faust-mode](/projects/tools(qa)/skills/faust-mode/SKILL.md) task",
        "Продолжи",
    ] {
        assert!(f.submit(prompt).contains("включён faust-mode"));
    }
    assert!(f
        .context(
            "UserPromptSubmit",
            json!({"session_id":"another","prompt":"Продолжи"})
        )
        .contains("выключен"));
    assert!(f.submit("/faust-mode-off").contains("выключен"));
    assert!(f.submit("Продолжи").contains("выключен"));
}

#[test]
fn markdown_command_examples_preserve_the_current_mode() {
    let f = Fixture::new();
    for (session, active) in [("plain", false), ("coordinator", true)] {
        let submit = |prompt: &str| {
            f.context(
                "UserPromptSubmit",
                json!({"session_id":session,"prompt":prompt}),
            )
        };
        if active {
            assert!(submit("\n   /faust-mode задача").contains("включён faust-mode"));
        }
        for command in ["/faust-mode", "/faust-mode-off"] {
            for indent in ["    ", "\t", "  \t"] {
                let context = submit(&format!("\n{indent}{command}\n\nОбъясни пример."));
                assert_eq!(context.contains("включён faust-mode"), active);
                assert!(!context.contains("model-check —"));
            }
        }
        assert_eq!(submit("Продолжи").contains("включён faust-mode"), active);
    }
}

#[test]
fn index_is_sent_on_activation_and_restore_but_not_every_turn() {
    let f = Fixture::new();
    let skills = crate::package::skill_names(package()).unwrap();
    let assert_index = |context: &str| {
        let names: BTreeSet<_> = context
            .lines()
            .filter_map(|line| line.split_once(" — "))
            .filter(|(name, signal)| !name.contains(' ') && !signal.is_empty())
            .map(|(name, _)| name.to_owned())
            .collect();
        assert_eq!(names, skills);
        assert!(context.contains(
            &package()
                .join("skills/faust-mode/SKILL.md")
                .display()
                .to_string()
        ));
    };
    let initial = f.context("SessionStart", json!({}));
    assert!(initial.contains("Режим текущего запроса сообщит UserPromptSubmit"));
    assert!(!initial.contains("SKILL.md"));
    assert_index(&f.submit("/faust-mode"));
    assert!(!f.submit("Продолжи").contains("model-check —"));
    for source in ["resume", "compact"] {
        assert_index(&f.context("SessionStart", json!({"source":source})));
    }
    assert_eq!(
        f.context("SessionStart", json!({"session_id":"other"})),
        initial
    );
    let worker = f.context(
        "SubagentStart",
        json!({"agent_id":"worker","session_id":"worker","agent_type":"worker"}),
    );
    assert!(worker.contains("активный координатор передал"));
    assert!(!worker.contains("SKILL.md"));
    f.submit("/faust-mode-off");
    assert_eq!(
        f.context("SessionStart", json!({"source":"resume"})),
        initial
    );
}

#[test]
fn native_child_uses_parent_session_without_changing_its_coordinator_choice() {
    let f = Fixture::new();
    f.submit("/faust-mode");
    let marker = f.state.marker("session");
    let before = fs::read(&marker).unwrap();
    let child = json!({"session_id":"session","agent_id":"child","agent_type":"default"});
    assert_eq!(f.context("SubagentStart", child.clone()), WORKER);
    let mut prompt = child;
    prompt["prompt"] = json!("/faust-mode-off");
    assert_eq!(f.hook("UserPromptSubmit", prompt), json!({}));
    assert_eq!(fs::read(&marker).unwrap(), before);
    assert!(!f.state.marker("child").exists());
    for id in [json!("../child"), json!(null), json!(false)] {
        assert_eq!(
            f.hook(
                "SubagentStart",
                json!({"agent_id":id,"agent_type":"default"})
            ),
            json!({})
        );
    }
}

#[test]
fn ultra_uses_only_the_verified_root_turn_and_manual_choice_wins() {
    let f = Fixture::new();
    let sessions = f.state.profile.join("sessions/2026/11/20");
    fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout-2026-11-20T12-00-00-session.jsonl");
    let snapshot = |source: Value, cwd: &Path, effort: &str| {
        let meta = json!({"type":"session_meta","payload":{"id":"session","source":source}});
        let start = json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn","collaboration_mode_kind":"default"}});
        let context = json!({"type":"turn_context","payload":{"turn_id":"turn","cwd":cwd,"effort":effort,"collaboration_mode":{"mode":"default"}}});
        fs::write(&transcript, format!("{meta}\n{start}\n{context}\n")).unwrap();
    };
    let submit = |prompt: &str, fields: Value| {
        let mut extra = json!({"prompt":prompt,"transcript_path":transcript});
        extra
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        f.context("UserPromptSubmit", extra)
    };
    snapshot(json!("vscode"), &f.repo, "ultra");
    assert!(submit("Проверь модуль", json!({})).contains("по глубине ultra"));
    assert!(!f.state.directory.exists());
    let check_journal = |prompt: &str, path: &Path| {
        f.hook(
            "UserPromptSubmit",
            json!({"prompt":prompt,"transcript_path":path}),
        )
    };
    assert!(check_journal("Проверь модуль", &transcript)["systemMessage"].is_null());
    let missing = check_journal("Проверь модуль", &sessions.join("missing.jsonl"));
    assert!(missing["systemMessage"]
        .as_str()
        .unwrap()
        .contains("журнал"));
    assert!(!missing.to_string().contains(sessions.to_str().unwrap()));
    let invalid = fs::read_to_string(&transcript)
        .unwrap()
        .replace("\"mode\":\"default\"", "\"kind\":\"default\"");
    fs::write(&transcript, invalid).unwrap();
    let output = check_journal("Проверь модуль", &transcript);
    assert!(output["systemMessage"]
        .as_str()
        .unwrap()
        .contains("текущего хода"));
    assert_eq!(output["hookSpecificOutput"]["additionalContext"], INACTIVE);
    assert!(check_journal("/faust-mode-off", &transcript)["systemMessage"].is_null());
    assert!(check_journal("Продолжи", &transcript)["systemMessage"].is_null());
    assert!(check_journal("/faust-mode", &transcript)["systemMessage"].is_null());
    let marker = f.state.marker("session");
    fs::remove_file(marker).unwrap();
    snapshot(json!("vscode"), &f.repo, "ultra");
    assert!(submit("Продолжи", json!({"turn_id":"other"})).contains("выключен"));
    assert!(submit("Продолжи", json!({"permission_mode":"plan"})).contains("нативном Plan Mode"));
    snapshot(json!("vscode"), &f.base, "ultra");
    assert!(submit("Проверь", json!({})).contains("выключен"));
    snapshot(json!({"subagent":{"thread_spawn":{}}}), &f.repo, "ultra");
    assert!(submit("Проверь", json!({})).contains("выключен"));
    snapshot(json!("cli"), &f.repo, "low");
    assert!(submit("Проверь", json!({"effort":"ultra"})).contains("выключен"));
    snapshot(json!("cli"), &f.repo, "ultra");
    assert!(submit("/faust-mode-off", json!({})).contains("выключен"));
    assert!(submit("Продолжи", json!({})).contains("выключен"));
    assert!(submit("/faust-mode", json!({})).contains("до команды"));
    assert!(submit("/faust-mode-off", json!({})).contains("выключен"));
    assert!(submit("Продолжи", json!({})).contains("выключен"));
}

#[test]
fn failed_state_update_preserves_the_last_completed_choice() {
    let f = Fixture::new();
    assert!(f.submit("/faust-mode").contains("включён faust-mode"));
    let marker = f.state.marker("session");
    let previous = fs::read(&marker).unwrap();
    let key = io::digest(&[&io::normalized(&f.state.directory), "session"]);
    let lock = io::lock_key(&format!("mode-{}", &key[..2]), Some(&f.repo)).unwrap();
    let input = json!({"hook_event_name":"UserPromptSubmit","session_id":"session","turn_id":"turn","cwd":f.repo,"prompt":"/faust-mode-off"});
    assert!(hook(&f.state, package(), "UserPromptSubmit", &input, &f.repo).is_err());
    assert_eq!(fs::read(&marker).unwrap(), previous);
    assert!(f.submit("Продолжи").contains("включён faust-mode"));
    drop(lock);
    assert!(f.submit("/faust-mode-off").contains("выключен"));
    assert_eq!(fs::read_to_string(&marker).unwrap(), "disabled\n");
    assert!(f.submit("Продолжи").contains("выключен"));
}

#[test]
fn plan_and_workspace_containing_profile_keep_coordination_without_other_state() {
    let f = Fixture::new();
    let context = f.context(
        "UserPromptSubmit",
        json!({"prompt":"/faust-mode plan","permission_mode":"plan"}),
    );
    assert!(context.contains("нативном Plan Mode"));
    let inside = State::new(f.repo.join("profile")).unwrap();
    let input = json!({"hook_event_name":"UserPromptSubmit","session_id":"inside","turn_id":"turn","cwd":f.repo,"prompt":"/faust-mode task"});
    let output = hook(&inside, package(), "UserPromptSubmit", &input, &f.repo).unwrap();
    assert!(output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("на этот запрос"));
    assert!(!inside.directory.exists());
}

#[test]
fn malformed_or_mismatched_hook_inputs_do_not_modify_state() {
    let f = Fixture::new();
    for (input, limit) in [
        (
            br#"{"session_id":"first","session_id":"second"}"#.as_slice(),
            100,
        ),
        (b"[]".as_slice(), 100),
        (b"{} {}".as_slice(), 100),
        (b"{}".as_slice(), 1),
    ] {
        assert!(read_json(input, limit).is_err());
        assert!(protocol::read_hook_json(input, limit).is_err());
    }
    assert_eq!(
        protocol::read_hook_json(b"\xef\xbb\xbf{}\r\n".as_slice(), 7).unwrap(),
        json!({})
    );
    for fields in [
        json!({"cwd":f.base}),
        json!({"session_id":"../other"}),
        json!({"turn_id":null}),
        json!({"hook_event_name":"Stop"}),
        json!({"agent_id":"worker","agent_type":"worker"}),
    ] {
        let mut fields = fields;
        fields["prompt"] = json!("/faust-mode");
        assert_eq!(f.hook("UserPromptSubmit", fields), json!({}));
    }
    assert!(!f.state.directory.exists());
}
