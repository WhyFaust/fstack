use super::{
    action, decode, format, io,
    pending::{self, Pending},
    store::{Binding, Store},
    Result,
};
use serde_json::{json, Value};
use std::path::Path;

pub fn scope_id(b: &Binding, cwd: &Path) -> String {
    pending::digest(&[
        &io::normalized(cwd),
        &io::normalized(&b.root),
        &b.project,
        &b.mode,
    ])
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
        if plan {
            return Ok(json!({}));
        }
        let b = store.resolve(cwd)?;
        let status = b
            .map(|b| format!("{} ({})", b.project, b.mode))
            .unwrap_or("проект не подключён".into());
        return Ok(extra(event, format!("Память fstack: {status}.")));
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
    if plan {
        return Ok(json!({}));
    }
    let staged = if event == "PostToolUse" {
        posted(payload)?
    } else {
        None
    };
    if event == "PostToolUse" && staged.is_none() {
        return Ok(json!({}));
    }
    let Some(b) = store.resolve(cwd)? else {
        return Ok(json!({}));
    };
    let scope = scope_id(&b, cwd);
    if event == "UserPromptSubmit" {
        let prefix = prefix()?;
        let mut ids = Vec::new();
        if let Some(prompt) = payload["prompt"].as_str().filter(|s| !s.trim().is_empty()) {
            let q = prompt.chars().take(500).collect::<String>();
            if let Ok(v) = store.search(&b, &q, 3) {
                for entry in v.as_array().into_iter().flatten() {
                    if let Some(id) = entry["id"].as_str().filter(|s| format::id(s).is_ok()) {
                        ids.push(id.to_owned());
                    }
                }
            }
        }
        let token = pending.issue(&key, &scope, generation)?;
        let found = if ids.is_empty() {
            "—".into()
        } else {
            ids.join(", ")
        };
        return Ok(extra(event,format!("Память fstack, проект {}, режим {}. ID поиска: {found}. Загрузи навык use-memory для правил записи. Перед завершением обычного хода сам оцени, есть ли обоснованное долговечное решение, поправка или устойчивое предпочтение. Опирайся на текущую работу, слова пользователя или проверяемый результат. В режиме auto подготовь такое знание через remember. Разрешение уже даёт режим auto. Сомнительное, но полезное знание можно направить в draft. В режиме inbox подготовленные знания поступают на ручную проверку. Для approve и forget нужно явное поручение пользователя. Если подходящей новой записи нет, подготовь {{\"action\":\"skip\"}} без текста и причин. Не создавай запись ради отчёта. В обычном режиме результат оценки передай структурированный JSON в {prefix} pack через stdin. Затем выполни отдельный точный вызов в оболочке из текущего cwd, подставив ASCII-строку из JSON-ответа pack без кавычек вместо BASE64URL: {prefix} stage --turn {token} --payload BASE64URL. Подготовь одну операцию за ход. Ответ staged подтверждает проверку команды; другую операцию хук отклонит. В режиме Plan пропусти stage. Не включай токен в ответ.",b.project,b.mode)));
    }
    if let Some((token, value)) = staged {
        if pending.stage(&key, &token, &scope, &value)? {
            return Ok(
                json!({"decision":"block","reason":"На этот ход уже принята другая операция памяти fstack. Она остаётся в очереди. Вторую операцию выполни в новом ходе."}),
            );
        }
        return Ok(json!({}));
    }
    let Some(data) = pending.consume(
        &key,
        &scope,
        !payload["stop_hook_active"].as_bool().unwrap_or(true),
    )?
    else {
        return Ok(json!({}));
    };
    if let Some(review) = data["review_required"].as_bool() {
        if !review || payload["stop_hook_active"] == true {
            return Ok(
                json!({"systemMessage":"Оценка памяти fstack не подтверждена; новая запись не создана."}),
            );
        }
        return Ok(
            json!({"decision":"block","reason":"Перед завершением оцени долговечную память текущего проекта по fstack:use-memory. Без новой просьбы пользователя подготовь обоснованное решение, поправку или предпочтение через pack и stage: remember в auto, draft для кандидата или режима inbox. Если подходящего знания нет, передай {\"action\":\"skip\"} тем же способом. Не выдумывай записи, не читай историю чатов и не сохраняй секреты. Используй последний токен из UserPromptSubmit; в Plan пропусти запись. Если подготовка недоступна, сообщи об этом и заверши основную задачу."}),
        );
    }
    let result = (|| -> Result<()> {
        let request = action(&data)?;
        match request["action"].as_str().unwrap() {
            "skip" => {}
            "approve" => {
                store.approve(&b, request["id"].as_str().unwrap())?;
            }
            "forget" => {
                store.forget(&b, request["id"].as_str().unwrap())?;
            }
            a => {
                store.save(
                    &b,
                    &request["candidate"],
                    a == "remember",
                    Some(session),
                    Some(turn),
                )?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        Ok(
            json!({"systemMessage":"Не удалось применить операцию памяти fstack. Повтори поручение в новом ходе."}),
        )
    } else {
        Ok(json!({}))
    }
}
