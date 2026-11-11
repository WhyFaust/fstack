use super::{
    action, decode, format, io,
    pending::{self, Pending, Stop},
    scope as project_scope,
    store::{Binding, Store},
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
            io::write(&marker, &io::nonce()?, false, None)?;
            io::read(&marker)
        }
        Some(false) => {
            let _mode_lock = mode_lock(store, session)?;
            Ok(io::delete(&marker)?.then(String::new))
        }
        None => io::read(&marker),
    }
}
fn activation(event: &str, package: &Path) -> Value {
    extra(event, format!("В этом чате включён faust-mode до команды /faust-mode-off. Используй инструкции fstack:faust-mode из файла {}. Если они уже загружены, повторно не читай.", package.join("skills/faust-mode/SKILL.md").display()))
}
fn posted(payload: &Value) -> Result<Option<(String, Value)>> {
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
    if payload["tool_response"]
        .as_str()
        .is_none_or(|s| s.trim() != "{\"staged\": true}")
    {
        return Ok(None);
    }
    let start = format!("{} stage --turn ", prefix()?);
    let Some(rest) = command.strip_prefix(&start) else {
        return Ok(None);
    };
    let Some((token, encoded)) = rest.split_once(" --payload ") else {
        return Ok(None);
    };
    if pending::token_parts(token).is_none() {
        return Ok(None);
    }
    Ok(decode(encoded).ok().map(|v| (token.into(), v)))
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
    let prompt_submit = event == "UserPromptSubmit";
    if !prompt_submit && (plan || (event == "PostToolUse" && !pending.active(&key)?)) {
        return Ok(json!({}));
    }
    let command = if prompt_submit {
        payload["prompt"].as_str().and_then(mode_command)
    } else {
        None
    };
    let enabled = chat_mode(store, cwd, session, command)?;
    if command == Some(false) {
        return Ok(if enabled.is_some() {
            extra(
                event,
                "Faust-mode выключен для этого чата. Выполняй запрос напрямую.".into(),
            )
        } else {
            json!({})
        });
    }
    let Some(epoch) = enabled else {
        return Ok(json!({}));
    };
    if plan {
        return Ok(activation(event, &store.package));
    }
    let staged = if event == "PostToolUse" {
        posted(payload)?
    } else {
        None
    };
    if event == "PostToolUse" && staged.is_none() {
        return Ok(json!({}));
    }
    let binding = if event == "Stop" {
        store.bound(cwd)?
    } else {
        store.resolve(cwd)?
    };
    let Some(b) = binding else {
        return Ok(if prompt_submit {
            activation(event, &store.package)
        } else {
            json!({})
        });
    };
    let scope = scope_id(&b, cwd);
    if event == "UserPromptSubmit" {
        let prefix = prefix()?;
        let mut ids = Vec::new();
        let mut search_failed = false;
        if let Some(prompt) = payload["prompt"].as_str().filter(|s| !s.trim().is_empty()) {
            let q = prompt.chars().take(500).collect::<String>();
            match store.search(&b, &q, 3) {
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
        let token = pending.issue(&key, &scope, generation, &epoch, session)?;
        let found = if search_failed {
            "поиск недоступен".into()
        } else if ids.is_empty() {
            "—".into()
        } else {
            ids.join(", ")
        };
        let mut output = extra(event,format!("Память fstack, проект {}, режим {}. ID поиска: {found}. Загрузи навык use-memory для правил записи. Перед завершением обычного хода сам оцени, есть ли обоснованное долговечное решение, поправка или устойчивое предпочтение. Опирайся на текущую работу, слова пользователя или проверяемый результат. В режиме auto подготовь такое знание через remember. Разрешение уже даёт режим auto. Сомнительное, но полезное знание можно направить в draft. В режиме inbox подготовленные знания поступают на ручную проверку. Для approve и forget нужно явное поручение пользователя. Если подходящей новой записи нет, подготовь {{\"action\":\"skip\"}} без текста и причин. Не создавай запись ради отчёта. В обычном режиме результат оценки передай структурированный JSON в {prefix} pack через stdin. Затем выполни отдельный точный вызов в оболочке из текущего cwd, подставив ASCII-строку из JSON-ответа pack без кавычек вместо BASE64URL: {prefix} stage --turn {token} --payload BASE64URL. Подготовь одну операцию за ход. Ответ staged подтверждает проверку команды; другую операцию хук отклонит. В режиме Plan пропусти stage. Не включай токен в ответ.",b.project,b.mode));
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        output["hookSpecificOutput"]["additionalContext"] = json!(format!(
            "{} {context}",
            activation(event, &store.package)["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
        ));
        if search_failed {
            output["systemMessage"] = json!("Не удалось подобрать контекст памяти fstack. Продолжай основную задачу. Выполни отдельный search, если для ответа не хватает контекста из памяти.");
        }
        return Ok(output);
    }
    if let Some((token, value)) = staged {
        if pending.stage(&key, &token, &scope, &value)? {
            return Ok(
                json!({"decision":"block","reason":"На этот ход уже принята другая операция памяти fstack. Она остаётся в очереди. Вторую операцию выполни в новом ходе."}),
            );
        }
        return Ok(json!({}));
    }
    let outcome = pending.claim(
        &key,
        &scope,
        session,
        turn,
        &epoch,
        payload["stop_hook_active"] == false,
    )?;
    let mut failed = false;
    for key in pending.due(&scope, session, &epoch, &key)? {
        let _turn_lock = pending.lock_turn(&key)?;
        let Some(claim) = pending.operation(&key, &scope, session, &epoch)? else {
            continue;
        };
        let result = (|| -> Result<&str> {
            let _mode_lock = mode_lock(store, session)?;
            if chat_mode(store, cwd, session, None)?.as_deref() != Some(&epoch) {
                return Ok("cancelled");
            }
            let _binding_lock =
                store.lock_binding(&project_scope::binding_path(&store.state, cwd)?)?;
            let Some(binding) = store.bound(cwd)?.filter(|b| scope_id(b, cwd) == scope) else {
                return Ok("cancelled");
            };
            apply(store, &binding, &claim.data, session, &claim.turn)?;
            Ok(if claim.data["action"] == "skip" {
                "skipped"
            } else {
                "applied"
            })
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
            json!({"decision":"block","reason":"Перед завершением оцени долговечную память текущего проекта по fstack:use-memory. Без новой просьбы пользователя подготовь обоснованное решение, поправку или предпочтение через pack и stage: remember в auto, draft для кандидата или режима inbox. Если подходящего знания нет, передай {\"action\":\"skip\"} тем же способом. Не выдумывай записи, не читай историю чатов и не сохраняй секреты. Используй последний токен из UserPromptSubmit; в Plan пропусти запись. Если подготовка недоступна, сообщи об этом и заверши основную задачу."}),
        );
    }
    if failed {
        return Ok(
            json!({"systemMessage":"Операция памяти fstack пока не применена. Очередь проверит следующий обычный Stop в этом чате. Техническое состояние доступно через memory diagnostics."}),
        );
    }
    if outcome == Some(Stop::Missing) {
        return Ok(
            json!({"systemMessage":"Оценка памяти fstack не подтверждена; новая запись не создана."}),
        );
    }
    Ok(json!({}))
}

fn apply(store: &Store, binding: &Binding, data: &Value, session: &str, turn: &str) -> Result<()> {
    let request = action(data)?;
    match request["action"].as_str().unwrap() {
        "skip" => {}
        "approve" => {
            store.approve(binding, request["id"].as_str().unwrap())?;
        }
        "forget" => {
            store.forget(binding, request["id"].as_str().unwrap())?;
        }
        a => {
            store.save(
                binding,
                &request["candidate"],
                a == "remember",
                Some(session),
                Some(turn),
            )?;
        }
    }
    Ok(())
}
