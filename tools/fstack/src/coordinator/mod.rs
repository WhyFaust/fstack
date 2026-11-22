mod codex_task;
mod io;
mod protocol;
#[cfg(test)]
mod tests;

use crate::Result;
use protocol::read_json;
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

struct State {
    profile: PathBuf,
    directory: PathBuf,
}

impl State {
    fn new(profile: PathBuf) -> Result<Self> {
        let profile = io::absolute(&profile)?;
        Ok(Self {
            directory: profile.join("fstack/coordinator"),
            profile,
        })
    }

    fn marker(&self, session: &str) -> PathBuf {
        self.directory.join("sessions").join(io::digest(&[session]))
    }
}

fn matches(pattern: &str, value: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|pattern| pattern.is_match(value))
}

fn extra(event: &str, context: String) -> Value {
    json!({"hookSpecificOutput":{"hookEventName":event,"additionalContext":context}})
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManualChoice {
    Enabled,
    Disabled,
}

fn read_choice(text: Option<&str>) -> Result<Option<ManualChoice>> {
    match text {
        None => Ok(None),
        Some("enabled\n") => Ok(Some(ManualChoice::Enabled)),
        Some("disabled\n") => Ok(Some(ManualChoice::Disabled)),
        Some(_) => Err("Invalid coordinator choice".into()),
    }
}

fn previous_chat_mode(
    state: &State,
    cwd: &Path,
    session: &str,
    command: Option<bool>,
) -> Result<Option<ManualChoice>> {
    io::state(&state.directory, cwd, false)?;
    let marker = state.marker(session);
    let Some(enabled) = command else {
        return read_choice(io::read(&marker)?.as_deref());
    };
    io::state(&state.directory, cwd, true)?;
    io::directory(marker.parent().unwrap(), true, true)?;
    let key = io::digest(&[&io::normalized(&state.directory), session]);
    let _lock = io::lock_key(&format!("mode-{}", &key[..2]), Some(cwd))?;
    let previous = io::read(&marker)?;
    let choice = read_choice(previous.as_deref())?;
    let next = if enabled { "enabled\n" } else { "disabled\n" };
    if previous.as_deref() != Some(next) {
        io::write(&marker, next, true, previous.as_deref())?;
    }
    Ok(choice)
}

fn fallback(package: &Path, event: &str, payload: &Value) -> Value {
    match event {
        "UserPromptSubmit"
            if payload["prompt"].as_str().and_then(mode_command) == Some(false) =>
        {
            extra(event, "Faust-mode выключен только на этот запрос. Постоянный выбор не сохранён: состояние профиля недоступно. Следующий запрос использует последний сохранённый режим. Выполняй текущий запрос напрямую и применяй только явно вызванные навыки.".into())
        }
        "UserPromptSubmit" if payload["prompt"].as_str().and_then(mode_command) == Some(true) => {
            extra(
                event,
                activation(package, explicit_plan(payload), Activation::SingleTurn),
            )
        }
        "UserPromptSubmit" => extra(event, INACTIVE.into()),
        "SessionStart" => extra(event, SESSION_START.into()),
        "SubagentStart" => extra(event, WORKER.into()),
        _ => json!({}),
    }
}

fn explicit_plan(payload: &Value) -> bool {
    let collaboration = if payload["collaboration_mode"].is_object() {
        &payload["collaboration_mode"]["mode"]
    } else {
        &payload["collaboration_mode"]
    };
    [&payload["permission_mode"], collaboration, &payload["mode"]]
        .iter()
        .any(|v| **v == "plan")
}

fn hook(state: &State, package: &Path, event: &str, payload: &Value, cwd: &Path) -> Result<Value> {
    let child = payload.get("agent_id").is_some() || payload.get("agent_type").is_some();
    if payload["hook_event_name"] != event
        || child != (event == "SubagentStart")
        || (child
            && (!payload["agent_id"]
                .as_str()
                .is_some_and(|id| matches(r"\A[a-zA-Z0-9_-]{1,128}\z", id))
                || !payload["agent_type"].is_string()))
    {
        return Ok(json!({}));
    }
    let Some(cwd) = payload["cwd"]
        .as_str()
        .map(Path::new)
        .filter(|path| io::same_directory(path, cwd))
    else {
        return Ok(json!({}));
    };
    let Some(session) = payload["session_id"]
        .as_str()
        .filter(|s| matches(r"\A[a-zA-Z0-9_-]{1,128}\z", s))
    else {
        return Ok(json!({}));
    };
    if io::within(&state.directory, cwd) {
        return Ok(fallback(package, event, payload));
    }
    if event == "SubagentStart" {
        return Ok(extra(event, WORKER.into()));
    }
    let turn = if event == "UserPromptSubmit" {
        let Some(turn) = payload["turn_id"]
            .as_str()
            .filter(|s| matches(r"\A[a-zA-Z0-9_-]{1,128}\z", s))
        else {
            return Ok(json!({}));
        };
        Some(turn)
    } else {
        None
    };
    let recorded = codex_task::current(&state.profile, payload, cwd, session, turn);
    let plan = explicit_plan(payload)
        || recorded
            .as_ref()
            .is_some_and(|turn| turn.mode == codex_task::Mode::Plan);
    let command = (event == "UserPromptSubmit")
        .then(|| payload["prompt"].as_str().and_then(mode_command))
        .flatten();
    let previous = previous_chat_mode(state, cwd, session, command)?;
    let context = if event == "SessionStart" {
        if previous == Some(ManualChoice::Enabled) {
            activation(package, plan, Activation::Restore)
        } else if recorded.as_ref().is_some_and(|turn| turn.delegated) {
            WORKER.into()
        } else {
            SESSION_START.into()
        }
    } else {
        let activation_state = if let Some(enable) = command {
            enable.then_some(Activation::Enter)
        } else {
            match previous {
                Some(ManualChoice::Disabled) => None,
                Some(ManualChoice::Enabled) => Some(Activation::Continue),
                None if recorded.as_ref().is_some_and(|turn| turn.root_ultra) => {
                    Some(Activation::Ultra)
                }
                None => None,
            }
        };
        activation_state.map_or_else(|| INACTIVE.into(), |state| activation(package, plan, state))
    };
    Ok(extra(event, context))
}

pub fn run(arguments: &[OsString], package: &Path) -> Result<()> {
    let [event] = arguments else {
        return Err("Использование: fstack hook EVENT".into());
    };
    let Some(event @ ("SessionStart" | "SubagentStart" | "UserPromptSubmit")) = event.to_str()
    else {
        return Err("Неизвестное событие координатора".into());
    };
    let payload = read_json(std::io::stdin().lock(), 2_097_152);
    let output = match payload {
        Ok(payload) => {
            let result = (|| {
                let state = State::new(io::configured_directory("CODEX_HOME", ".codex")?)?;
                let cwd = std::env::current_dir().map_err(io::err)?;
                hook(&state, package, event, &payload, &cwd)
            })();
            result.unwrap_or_else(|_| fallback(package, event, &payload))
        }
        Err(_) => json!({}),
    };
    println!("{output}");
    Ok(())
}

fn mode_command(prompt: &str) -> Option<bool> {
    let line = prompt.lines().find(|line| !line.trim().is_empty())?;
    let prompt = line.trim_start_matches(' ');
    if line.len() - prompt.len() >= 4 || prompt.starts_with('\t') {
        return None;
    }
    match prompt.split_whitespace().next() {
        Some("/faust-mode" | "$fstack:faust-mode" | "$faust-mode") => Some(true),
        Some("/faust-mode-off") => Some(false),
        _ => {
            let target = prompt.strip_prefix("[$fstack:faust-mode](")?;
            let mut depth = 1;
            for (index, ch) in target.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => continue,
                }
                if depth == 0 {
                    let rest = &target[index + 1..];
                    return (index > 0 && rest.chars().next().is_none_or(char::is_whitespace))
                        .then_some(true);
                }
            }
            None
        }
    }
}
const INACTIVE: &str = "Faust-mode выключен. Выполняй запрос напрямую. Применяй только явно вызванный пользователем навык fstack. Цитаты, ссылки внутри навыка и разбор его исходников не разрешают другие вызовы.";
const SESSION_START: &str = "Режим текущего запроса сообщит UserPromptSubmit. До включения применяй только явно вызванный пользователем навык fstack.";
const WORKER: &str = "Если активный координатор передал инструкцию исполнителя, применяй навыки в границах поручения. Иначе требуется прямой вызов пользователя. Цитата не включает навык.";

enum Activation {
    Enter,
    Restore,
    Continue,
    SingleTurn,
    Ultra,
}

fn activation(package: &Path, plan: bool, state: Activation) -> String {
    let status = match state {
        Activation::Restore => "При старте чата восстановлен faust-mode до /faust-mode-off.",
        Activation::SingleTurn => "В этом чате включён faust-mode на этот запрос. Состояние режима недоступно для этой рабочей папки.",
        Activation::Ultra => "В этом чате включён faust-mode на этот запрос по глубине ultra.",
        _ => "В этом чате включён faust-mode до команды /faust-mode-off.",
    };
    let mut text = format!(
        "{status} Следуй fstack:faust-mode из {}. \
         Выбирай по {} и каталогу {}. При смене этапа повтори выбор. \
         Файл выбранного метода — <имя>/SKILL.md в {}.",
        package.join("skills/faust-mode/SKILL.md").display(),
        package.join("skills/route/SKILL.md").display(),
        package.join("docs/skills.md").display(),
        package.join("skills").display()
    );
    if plan {
        text.push_str(" Клиент в нативном Plan Mode. Подготовь план в формате клиента.");
    }
    if !matches!(state, Activation::Continue) {
        match crate::package::skill_index(package) {
            Ok(index) => {
                text.push('\n');
                text.push_str(&index);
            }
            Err(_) => text.push_str(
                " Хук не смог прочитать индекс. Проверь файл каталога по указанному пути.",
            ),
        }
    }
    text
}
