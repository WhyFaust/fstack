mod codex_task;
mod hooks;
mod pending;
#[cfg(test)]
mod shared_tests;
#[cfg(test)]
mod tests;
use fmem::{
    memory::{format, io, scope, shared, store},
    operations,
    protocol::{action, decode, decode_json, encode, read_json},
    Result,
};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
fn prepare_request(value: &Value) -> Result<Value> {
    if value.get("operations").is_some() {
        operations::validate_request(value)
    } else {
        action(value)
    }
}

fn report_action(arguments: &[&str]) -> Result<Value> {
    match arguments {
        ["--skip"] => Ok(json!({"action":"skip"})),
        ["--payload", encoded] => {
            let request = decode(encoded)?;
            let candidate = &request["candidate"];
            if !matches!(request["action"].as_str(), Some("remember" | "draft"))
                || request.get("scope").is_some()
                || candidate["kind"] != "decision"
                || candidate.get("supersedes").is_some()
                || candidate["source"].as_str().is_none_or(str::is_empty)
            {
                return Err(
                    "A Codex task can only report a new project decision with a source".into(),
                );
            }
            Ok(request)
        }
        _ => Err("Invalid memory report arguments".into()),
    }
}
fn correction(title: &str, body: &str, predecessor: Option<&str>) -> Result<Value> {
    for field in [title, body] {
        if field.contains([
            '\'', '"', '\n', '\r', '‘', '’', '‚', '‛', '“', '”', '„', '‟',
        ]) {
            return Err("Use memory pack for a correction with quotes or line breaks".into());
        }
    }
    let mut candidate = json!({
        "kind": "correction", "title": title, "body": body,
        "source": "Поправка пользователя в текущем ходе"
    });
    if let Some(id) = predecessor {
        candidate["supersedes"] = json!(id);
    }
    action(&json!({"action": "remember", "candidate": candidate}))
}
fn status(b: Option<&store::Binding>) -> Value {
    match b {
        None => json!({"enabled":false}),
        Some(b) => json!({"enabled":true,"root":b.root,"project":b.project,"mode":b.mode}),
    }
}
pub fn run(arguments: &[OsString], package: &Path) -> Result<()> {
    let args = arguments
        .iter()
        .map(|s| {
            s.to_str()
                .map(str::to_owned)
                .ok_or("Memory arguments require UTF-8".into())
        })
        .collect::<Result<Vec<_>>>()?;
    let mut args = args.as_slice();
    let state = if args.first().is_some_and(|s| s == "--state-dir") {
        if args.len() < 3 {
            return Err("--state-dir requires a path and command".into());
        }
        let s = PathBuf::from(&args[1]);
        args = &args[2..];
        s
    } else {
        io::configured_directory("CODEX_HOME", ".codex")?.join("fstack/memory")
    };
    let command = args.first().ok_or("Missing memory command")?;
    let store = store::Store::new(state)?;
    let cwd = std::env::current_dir().map_err(io::err)?;
    let result = if command == "hook" {
        let event = args
            .get(1)
            .filter(|s| {
                matches!(
                    s.as_str(),
                    "SessionStart"
                        | "SubagentStart"
                        | "UserPromptSubmit"
                        | "PostToolUse"
                        | "Stop"
                        | "SubagentStop"
                        | "Interrupt"
                )
            })
            .ok_or("Invalid memory hook event")?;
        if args.len() != 2 {
            return Err("Invalid memory hook arguments".into());
        }
        match read_json(std::io::stdin().lock(), 2_097_152)
            .and_then(|v| hooks::hook(&store, package, event, &v, &cwd))
        {
            Ok(v) => v,
            Err(_) => json!({"systemMessage":"Хук памяти fstack не смог обработать событие."}),
        }
    } else {
        execute(&store, args, &cwd)?
    };
    println!("{result}");
    Ok(())
}
fn execute(store: &store::Store, args: &[String], cwd: &Path) -> Result<Value> {
    let command = args[0].as_str();
    let rest = &args[1..];
    if command == "init" {
        let mut opts = std::collections::BTreeMap::new();
        if !rest.len().is_multiple_of(2) {
            return Err("Invalid memory init arguments".into());
        }
        for pair in rest.chunks(2) {
            if !matches!(
                pair[0].as_str(),
                "--vault" | "--repo" | "--project" | "--mode"
            ) || opts.insert(pair[0].as_str(), pair[1].as_str()).is_some()
            {
                return Err("Invalid memory init arguments".into());
            }
        }
        let repo = Path::new(*opts.get("--repo").ok_or("Missing --repo")?);
        io::state(&store.state, &io::absolute(repo)?, true)?;
        let b = store.setup(
            Path::new(*opts.get("--vault").ok_or("Missing --vault")?),
            repo,
            opts.get("--project").ok_or("Missing --project")?,
            opts.get("--mode").copied(),
        )?;
        return Ok(status(Some(&b)));
    }
    if command == "pack" && rest.is_empty() {
        return Ok(json!(encode(&prepare_request(&read_json(
            std::io::stdin().lock(),
            20000
        )?)?)?));
    }
    if command == "vault" {
        return match rest {
            [] => store.vault_status(),
            [flag, path] if flag == "--set" => store.pin_vault(Path::new(path)),
            _ => Err("Use memory vault or memory vault --set <absolute path>".into()),
        };
    }
    if command == "ensure" && rest.len() == 2 && rest[0] == "--turn" {
        let scope = store
            .setup_scope(cwd)?
            .ok_or("Memory setup is unavailable")?;
        if !pending::Pending::new(&store.state, cwd).is_current(&rest[1], &scope)? {
            return Err("Invalid or expired memory setup token".into());
        }
        return Ok(json!({"ready":true}));
    }
    if command == "ensure" && rest.is_empty() {
        return Ok(status(store.auto_resolve(cwd)?.as_ref()));
    }
    if command == "disable" && rest.is_empty() {
        return Ok(json!({"disabled":store.disable(cwd)?}));
    }
    if command == "diagnostics" && rest.is_empty() {
        let binding = store
            .bound(cwd)?
            .ok_or("Memory is not enabled for this project")?;
        return pending::Pending::new(&store.state, cwd)
            .diagnostics(&hooks::scope_id(&binding, cwd));
    }
    let b = if matches!(command, "stage" | "report" | "receipt") {
        store.bound(cwd)?
    } else {
        store.resolve(cwd)?
    };
    if command == "status" && rest.is_empty() {
        return Ok(status(b.as_ref()));
    }
    let b = b.ok_or("Memory is not enabled for this project")?;
    if command == "shared" {
        return shared::execute(store, &b, rest);
    }
    match (command, rest) {
        ("search", [query]) => store.search(&b, query, 5),
        ("search", [query, flag, n]) if flag == "--limit" => {
            store.search(&b, query, n.parse().map_err(io::err)?)
        }
        ("show", [id]) => store.get(&b, id),
        ("history", [id]) => store.history(&b, id, 5),
        ("history", [id, flag, n]) if flag == "--limit" => {
            store.history(&b, id, n.parse().map_err(io::err)?)
        }
        ("inbox", []) => store.inbox(&b),
        ("receipt", [id]) => Ok(operations::receipt(store, &b, id)?.unwrap_or(Value::Null)),
        ("approve", [id]) => store.approve(&b, id),
        ("forget", [id]) => store.forget(&b, id),
        ("remember", []) => store.save(
            &b,
            &read_json(std::io::stdin().lock(), 32768)?,
            true,
            None,
            None,
        ),
        ("report", arguments) => {
            report_action(&arguments.iter().map(String::as_str).collect::<Vec<_>>())?;
            Ok(json!({"reported":true}))
        }
        ("stage", [turn_flag, token, rest @ ..]) if turn_flag == "--turn" => {
            let (key, _) = pending::token_parts(token).ok_or("Invalid memory turn token")?;
            let pending = pending::Pending::new(&store.state, cwd);
            let scope = hooks::scope_id(&b, cwd);
            let request = match rest {
                [flag] if flag == "--skip" => json!({"action":"skip"}),
                [flag, encoded] if flag == "--payload" => prepare_request(&decode_json(encoded)?)?,
                [flag, title, body] if flag == "--correction" => {
                    correction(title, body, None)?;
                    return validate_correction(&pending, token, &scope);
                }
                [flag, title, body, supersedes, id]
                    if flag == "--correction" && supersedes == "--supersedes" =>
                {
                    correction(title, body, Some(id))?;
                    return validate_correction(&pending, token, &scope);
                }
                _ => return Err("Invalid memory stage arguments".into()),
            };
            match pending.stage(key, token, &scope, &request)? {
                pending::Stage::Accepted => Ok(json!({"staged":true,"phase":"stage"})),
                pending::Stage::Conflict => {
                    Err("A different memory action is already staged for this turn".into())
                }
                pending::Stage::Unavailable => Err("Invalid or expired memory turn token".into()),
            }
        }
        _ => Err("Invalid memory command arguments".into()),
    }
}

fn validate_correction(pending: &pending::Pending, token: &str, scope: &str) -> Result<Value> {
    if !pending.is_current(token, scope)? {
        return Err("Invalid or expired memory turn token".into());
    }
    Ok(json!({"validated":true}))
}
