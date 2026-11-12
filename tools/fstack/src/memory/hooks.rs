use super::{
    action, correction, decode, format, io,
    pending::{self, Delivery, Pending, Stop},
    scope as project_scope,
    store::{Binding, Store, WriteOutcome},
    Result,
};
use serde_json::{json, Value};
use std::path::Path;

pub fn scope_id(b: &Binding, cwd: &Path) -> String {
    let cwd = io::normalized(cwd);
    let root = io::normalized(&b.root);
    let mut parts = vec![cwd.as_str(), root.as_str(), &b.project, &b.mode];
    if let Some(generation) = &b.generation {
        parts.push(generation);
    }
    pending::digest(&parts)
}
fn prefix() -> Result<String> {
    let p = std::env::current_exe()
        .map_err(io::err)?
        .canonicalize()
        .map_err(io::err)?;
    let p = p.to_str().ok_or("Invalid memory executable path")?;
    if p.chars()
        .any(|c| matches!(c, '"' | '`' | '$' | '\r' | '\n') || (!cfg!(windows) && c == '\\'))
    {
        return Err("Memory command path cannot be used in a shell command".into());
    }
    Ok(format!(
        "{}\"{p}\" memory",
        if cfg!(windows) { "& " } else { "" }
    ))
}
fn extra(event: &str, context: String) -> Value {
    json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":context}})
}
fn mode_command(prompt: &str) -> Option<bool> {
    let prompt = prompt.trim_start();
    match prompt.split_whitespace().next() {
        Some("/faust-mode" | "$fstack:faust-mode" | "$faust-mode") => Some(true),
        Some("/faust-mode-off") => Some(false),
        _ if prompt
            .strip_prefix("[$fstack:faust-mode](")
            .and_then(|link| link.split_once(')'))
            .is_some_and(|(target, rest)| {
                !target.is_empty() && rest.chars().next().is_none_or(char::is_whitespace)
            }) =>
        {
            Some(true)
        }
        _ => None,
    }
}
fn mode_lock(store: &Store, session: &str) -> Result<std::fs::File> {
    let key = pending::digest(&["memory-mode", &io::normalized(&store.state), session]);
    io::lock_key(&format!("mode-{}", &key[..2]), None)
}
fn chat_mode(
    store: &Store,
    cwd: &Path,
    session: &str,
    command: Option<bool>,
) -> Result<Option<String>> {
    let enable = command == Some(true);
    let dir = store.state.join("faust-mode-sessions");
    if !io::state(&store.state, cwd, enable)? || !io::directory(&dir, enable, true)? {
        return Ok(None);
    }
    let marker = dir.join(pending::digest(&[session]));
    match command {
        Some(true) => {
            if let Some(epoch) = io::read(&marker)? {
                return Ok(Some(epoch));
            }
            let _mode_lock = mode_lock(store, session)?;
            io::write(
                &marker,
                &format!("coordinator-{}", io::nonce()?),
                false,
                None,
            )?;
            io::read(&marker)
        }
        Some(false) => {
            let _mode_lock = mode_lock(store, session)?;
            Ok(io::delete(&marker)?.then(String::new))
        }
        None => io::read(&marker),
    }
}
fn activation(package: &Path) -> String {
    format!("В этом чате включён faust-mode до команды /faust-mode-off. Используй инструкции fstack:faust-mode из файла {}. Если они уже загружены, повторно не читай.", package.join("skills/faust-mode/SKILL.md").display())
}
enum Posted {
    Ensure(String),
    Stage(String, Value),
}
fn posted(payload: &Value) -> Result<Option<Posted>> {
    if payload["tool_name"] != "Bash"
        || payload.get("agent_id").is_some()
        || payload.get("agent_type").is_some()
    {
        return Ok(None);
    }
    if !payload["tool_use_id"]
        .as_str()
        .is_some_and(|s| format::matches(r"\A[a-zA-Z0-9_-]{1,128}\z", s))
    {
        return Ok(None);
    }
    let Some(input) = payload["tool_input"].as_object().filter(|m| m.len() == 1) else {
        return Ok(None);
    };
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return Ok(None);
    };
    let prefix = prefix()?;
    if payload["tool_response"]
        .as_str()
        .is_some_and(|s| s.trim() == "{\"ready\":true}")
    {
        return Ok(command
            .strip_prefix(&format!("{prefix} ensure --turn "))
            .filter(|token| pending::token_parts(token).is_some())
            .map(|token| Posted::Ensure(token.into())));
    }
    if payload["tool_response"]
        .as_str()
        .is_none_or(|s| s.trim() != "{\"staged\": true}")
    {
        return Ok(None);
    }
    let start = format!("{prefix} stage --turn ");
    let Some(rest) = command.strip_prefix(&start) else {
        return Ok(None);
    };
    let Some((token, arguments)) = rest.split_once(' ') else {
        return Ok(None);
    };
    if pending::token_parts(token).is_none() {
        return Ok(None);
    }
    let value = if arguments == "--skip" {
        Some(json!({"action":"skip"}))
    } else if let Some(encoded) = arguments.strip_prefix("--payload ") {
        decode(encoded).ok()
    } else {
        literal_correction(arguments)
    };
    Ok(value.map(|v| Posted::Stage(token.into(), v)))
}
fn literal_correction(arguments: &str) -> Option<Value> {
    let fields = arguments.strip_prefix("--correction '")?;
    let (title, fields) = fields.split_once("' '")?;
    let (body, suffix) = fields.split_once('\'')?;
    let predecessor = if suffix.is_empty() {
        None
    } else {
        Some(suffix.strip_prefix(" --supersedes ")?)
    };
    correction(title, body, predecessor).ok()
}
pub fn hook(store: &Store, event: &str, payload: &Value, cwd: &Path) -> Result<Value> {
    if payload["hook_event_name"] != event
        || (event == "Stop" && !payload["stop_hook_active"].is_boolean())
        || payload.get("agent_id").is_some()
        || payload.get("agent_type").is_some()
    {
        return Ok(json!({}));
    }
    let collab = if payload["collaboration_mode"].is_object() {
        &payload["collaboration_mode"]["mode"]
    } else {
        &payload["collaboration_mode"]
    };
    let plan = [&payload["permission_mode"], collab, &payload["mode"]]
        .iter()
        .any(|v| **v == "plan");
    if payload["cwd"]
        .as_str()
        .is_none_or(|s| !io::equal(Path::new(s), cwd))
    {
        return Ok(json!({}));
    }
    if event == "SessionStart" {
        return Ok(json!({}));
    }
    let Some(session) = payload["session_id"]
        .as_str()
        .filter(|s| format::matches(r"\A[a-zA-Z0-9_-]{1,128}\z", s))
    else {
        return Ok(json!({}));
    };
    let Some(turn) = payload["turn_id"]
        .as_str()
        .filter(|s| format::matches(r"\A[a-zA-Z0-9_-]{1,128}\z", s))
    else {
        return Ok(json!({}));
    };
    if io::within(&store.state, cwd) {
        return Ok(
            if event == "UserPromptSubmit"
                && payload["prompt"].as_str().and_then(mode_command) == Some(true)
            {
                extra(event,format!("Для этого запроса используй fstack:faust-mode из {}. Рабочая папка содержит профиль Codex, поэтому состояние режима здесь не сохраняется.", store.package.join("skills/faust-mode/SKILL.md").display()))
            } else {
                json!({})
            },
        );
    }
    let ordinary = [collab, &payload["mode"]]
        .iter()
        .any(|v| matches!(v.as_str(), Some("default" | "normal")));
    let key = pending::digest(&[session, turn]);
    let pending = Pending::new(&store.state, cwd);
    let generation = if event == "UserPromptSubmit" || (event == "Stop" && plan) {
        match pending.invalidate(&key) {
            Ok(n) => n,
            Err(_) if event == "UserPromptSubmit" => {
                return Ok(
                    json!({"decision":"block","reason":"Не удалось отменить предыдущую операцию памяти fstack. Повтори ввод."}),
                )
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    if io::state(&store.state, cwd, false).is_err() {
        return Ok(
            json!({"systemMessage":"Автоматическая память недоступна. Проверь права и расположение локального профиля памяти вне версионируемых каталогов. Продолжай основной запрос."}),
        );
    }
    let prompt_submit = event == "UserPromptSubmit";
    if !prompt_submit && (plan || (event == "PostToolUse" && !pending.active(&key)?)) {
        return Ok(json!({}));
    }
    let command = if prompt_submit {
        payload["prompt"].as_str().and_then(mode_command)
    } else {
        None
    };
    let previous_mode = chat_mode(store, cwd, session, None)?;
    let coordinator = if prompt_submit {
        if command == Some(false) {
            chat_mode(store, cwd, session, command)?;
            Some("Faust-mode выключен для этого чата. Выполняй запрос напрямую.".to_owned())
        } else {
            let enabled = if command.is_some() {
                chat_mode(store, cwd, session, command)?.is_some()
            } else {
                previous_mode.is_some()
            };
            enabled.then(|| activation(&store.package))
        }
    } else {
        None
    };
    let coordinator_only = || {
        coordinator
            .as_ref()
            .map_or_else(|| json!({}), |text| extra(event, text.clone()))
    };
    if plan {
        return Ok(coordinator_only());
    }
    let staged = if event == "PostToolUse" {
        posted(payload)?
    } else {
        None
    };
    if event == "PostToolUse" && staged.is_none() {
        return Ok(json!({}));
    }
    let mut generation = generation;
    let ensure = matches!(staged, Some(Posted::Ensure(_)));
    let _setup_lock = if let Some(Posted::Ensure(token)) = &staged {
        let guard = pending.lock_turn(&key)?;
        let Some((issued_key, nonce)) = pending::token_parts(token) else {
            return Ok(json!({}));
        };
        let Some(target) = store.setup_scope(cwd)? else {
            return Ok(json!({}));
        };
        if issued_key != key || !pending.is_current(token, &target)? {
            return Ok(json!({}));
        }
        generation = Some(nonce.to_owned());
        Some(guard)
    } else {
        None
    };
    let failure = || {
        let mut output = coordinator_only();
        output["systemMessage"] = json!("Не удалось автоматически подключить память проекта. Команда memory vault показывает рабочий vault; при нескольких vault выбери его через memory vault --set.");
        output
    };
    let binding = if ensure || (prompt_submit && ordinary) {
        match store.auto_resolve(cwd) {
            Ok(binding) => binding,
            Err(_) => return Ok(failure()),
        }
    } else {
        store.bound(cwd)?
    };
    let Some(b) = binding else {
        if prompt_submit && !ordinary {
            let target = match store.setup_scope(cwd) {
                Ok(target) => target,
                Err(_) => return Ok(failure()),
            };
            if let Some(target) = target {
                let token = pending.issue(&key, &target, generation, "setup", session, turn)?;
                let prefix = prefix()?;
                let setup = format!("В обычном режиме подключи память проекта отдельной командой из текущего cwd: {prefix} ensure --turn {token}. Хук вернёт инструкции для этого же хода. В Plan пропусти эту команду.");
                return Ok(extra(
                    event,
                    coordinator.map_or_else(|| setup.clone(), |text| format!("{text} {setup}")),
                ));
            }
        }
        return Ok(coordinator_only());
    };
    let issuing = prompt_submit || ensure;
    let Some(epoch) = pending.session_epoch(session, previous_mode, issuing)? else {
        return Ok(json!({}));
    };
    let scope = scope_id(&b, cwd);
    if issuing {
        let prefix = prefix()?;
        let mut ids = Vec::new();
        let mut search_failed = false;
        if let Some(prompt) = payload["prompt"].as_str().filter(|s| !s.trim().is_empty()) {
            let q = prompt.chars().take(500).collect::<String>();
            match store.recall(&b, &q, 3) {
                Ok(v) => {
                    for entry in v.as_array().into_iter().flatten() {
                        if let Some(id) = entry["id"].as_str().filter(|s| format::id(s).is_ok()) {
                            ids.push(id.to_owned());
                        }
                    }
                }
                Err(_) => search_failed = true,
            }
        }
        let token = pending.issue(&key, &scope, generation, &epoch, session, turn)?;
        let recall = if ids.is_empty() {
            String::new()
        } else {
            format!(
                " Прочитай совпадения через {prefix} show ID и примени подходящие правила. ID: {}.",
                ids.join(", ")
            )
        };
        let instructions = format!(
            concat!(
                "Память fstack, проект {project}, режим {mode}.{recall} ",
                "В Plan пропусти запись. Устойчивую поправку пользователя к правилу проекта примени сейчас. ",
                "Проверь дубликаты и подготовь новую поправку отдельной командой из текущего cwd: ",
                "{prefix} stage --turn {token} --correction 'Тема' 'Полное правило и область'. ",
                "При замене добавь --supersedes ID. Если нового знания нет, замени --correction и оба поля на --skip. ",
                "Для решения, предпочтения, черновика или сложного текста прочитай {skill}. ",
                "За ход принимается одна операция. Её сохраняет Stop. Не показывай токен."
            ),
            project = b.project,
            mode = b.mode,
            recall = recall,
            prefix = prefix,
            token = token,
            skill = store.package.join("skills/use-memory/SKILL.md").display(),
        );
        let instructions = match coordinator {
            Some(text) => format!("{text} {instructions}"),
            None => instructions,
        };
        let mut output = extra(event, instructions);
        let mut shared_instructions = String::from(" Только если пользователь явно просит применять предпочтение во всех проектах, подготовь через pack и stage одну операцию remember с полем scope=shared и kind=preference в candidate. Для изменения общей записи используй тот же scope и supersedes. Остальные знания сохраняй в проекте.");
        match super::shared::recall_context(
            store,
            &b,
            payload["prompt"].as_str().unwrap_or_default(),
            &prefix,
        ) {
            Ok(Some(shared)) => {
                shared_instructions.push(' ');
                shared_instructions.push_str(&shared);
            }
            Ok(None) => {}
            Err(_) => search_failed = true,
        }
        let context = &mut output["hookSpecificOutput"]["additionalContext"];
        *context = json!(format!(
            "{}{shared_instructions}",
            context.as_str().unwrap_or_default()
        ));
        if search_failed {
            output["systemMessage"] = json!("Не удалось подобрать контекст памяти fstack. Продолжай основную задачу. Выполни отдельный search, если для ответа не хватает контекста из памяти.");
        }
        return Ok(output);
    }
    if let Some(Posted::Stage(token, value)) = staged {
        if pending.stage(&key, &token, &scope, &value)? {
            return Ok(
                json!({"decision":"block","reason":"На этот ход уже принята другая операция памяти fstack. Она остаётся в очереди. Вторую операцию выполни в новом ходе."}),
            );
        }
        return Ok(json!({}));
    }
    let confirmed = ordinary || pending.confirmed(&key, &scope, &epoch)?;
    let outcome = pending.claim(
        &key,
        &scope,
        session,
        turn,
        &epoch,
        Delivery::Current {
            review: confirmed && payload["stop_hook_active"] == false,
        },
    )?;
    if !confirmed && outcome != Some(Stop::Claimed) {
        return Ok(json!({}));
    }
    let mut failed = false;
    for (due_key, original_turn) in pending.due(&scope, session, &epoch, &key)? {
        if due_key != key {
            pending.claim(
                &due_key,
                &scope,
                session,
                &original_turn,
                &epoch,
                Delivery::Replay,
            )?;
        }
        let _turn_lock = pending.lock_turn(&due_key)?;
        let Some(claim) = pending.operation(&due_key, &scope, session, &epoch)? else {
            continue;
        };
        let result = (|| -> Result<&str> {
            let _binding_lock =
                store.lock_binding(&project_scope::binding_path(&store.state, cwd)?)?;
            let Some(binding) = store.bound(cwd)?.filter(|b| scope_id(b, cwd) == scope) else {
                return Ok("cancelled");
            };
            apply(store, &binding, &claim.data, session, &claim.turn)
        })();
        if let Ok(phase) = result {
            pending.finish(&scope, &claim, phase, true)?;
        } else {
            failed = true;
            pending.finish(&scope, &claim, "failure", false)?;
        }
    }
    if outcome == Some(Stop::Review) {
        return Ok(
            json!({"decision":"block","reason":"Оценка памяти за ход ещё не принята. Вне Plan используй последнюю команду stage из UserPromptSubmit. Для устойчивой поправки проекта передай --correction, если нового знания нет — --skip. Для остальных операций прочитай fstack:use-memory. Если подготовка недоступна, сообщи об этом и заверши задачу."}),
        );
    }
    if failed {
        return Ok(
            json!({"systemMessage":"Операция памяти fstack пока не применена. Очередь проверит следующий обычный Stop в этом чате. Техническое состояние доступно через memory diagnostics."}),
        );
    }
    if outcome == Some(Stop::Missing) {
        return Ok(json!({"systemMessage":"Оценка памяти fstack для этого хода не подтверждена."}));
    }
    Ok(json!({}))
}

fn apply(
    store: &Store,
    binding: &Binding,
    data: &Value,
    session: &str,
    turn: &str,
) -> Result<&'static str> {
    let request = action(data)?;
    let shared;
    let binding = if request["scope"] == "shared" {
        shared = super::shared::for_action(store, binding, &request)?;
        &shared
    } else {
        binding
    };
    let outcome = match request["action"].as_str().unwrap() {
        "skip" => return Ok("skipped"),
        "approve" => store.approve_outcome(binding, request["id"].as_str().unwrap())?,
        "forget" => {
            store.forget(binding, request["id"].as_str().unwrap())?;
            return Ok("applied");
        }
        a => store.save_outcome(
            binding,
            &request["candidate"],
            a == "remember",
            Some(session),
            Some(turn),
        )?,
    };
    Ok(match outcome {
        WriteOutcome::Written(_) => "applied",
        WriteOutcome::Forgotten => "discarded_forgotten",
    })
}
