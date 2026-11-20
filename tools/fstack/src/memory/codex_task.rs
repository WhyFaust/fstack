use super::{format, io, read_json};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Component, Path},
};

const HEADER_LIMIT: u64 = 256 * 1024;
const TAIL_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Default,
    Plan,
}

impl Mode {
    fn read(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "default" => Some(Self::Default),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }
}

pub(super) struct Turn {
    pub mode: Mode,
    pub delegated: bool,
    pub root_ultra: bool,
}

struct Transcript {
    header: Value,
    tail: String,
}

fn turn_id(id: &Value) -> bool {
    id.as_str()
        .is_some_and(|id| format::matches(r"\A[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\z", id))
}

fn session_file(state: &Path, transcript: &Path, session: &str) -> Option<()> {
    let fstack = state.parent()?;
    let sessions = fstack.parent()?.join("sessions");
    if fstack.file_name()? != "fstack"
        || state.file_name()? != "memory"
        || !transcript.is_absolute()
        || transcript
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || !io::same_directory(transcript.ancestors().nth(4)?, &sessions)
    {
        return None;
    }
    let file = transcript.file_name()?.to_str()?;
    let time = file
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?
        .strip_suffix(session)?;
    if !format::matches(
        r"\A[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}-\z",
        time,
    ) {
        return None;
    }
    let days = [(&time[0..4], 3), (&time[5..7], 2), (&time[8..10], 1)];
    days.into_iter()
        .all(|(date, up)| {
            transcript
                .ancestors()
                .nth(up)
                .and_then(Path::file_name)
                .is_some_and(|part| part == date)
        })
        .then_some(())
}

fn transcript(state: &Path, path: &Value, session: &str) -> Option<Transcript> {
    let path = Path::new(path.as_str()?);
    session_file(state, path, session)?;
    if !io::file(path).ok()? {
        return None;
    }
    #[cfg(windows)]
    if io::windows_info(path, false).ok()?.2 != 1 {
        return None;
    }
    let file = io::options().read(true).open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 || metadata.uid() != unsafe { libc::getuid() } {
            return None;
        }
    }
    let mut file = BufReader::new(file);
    let mut first = Vec::new();
    (&mut file)
        .take(HEADER_LIMIT)
        .read_until(b'\n', &mut first)
        .ok()?;
    if first.last() != Some(&b'\n') {
        return None;
    }
    let mut meta = read_json(first.as_slice(), first.len()).ok()?;
    if meta["type"] != "session_meta" || meta["payload"]["id"] != session {
        return None;
    }

    let head = first.len() as u64;
    let start = metadata.len().saturating_sub(TAIL_LIMIT).max(head);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(TAIL_LIMIT).read_to_end(&mut tail).ok()?;
    let first_line = if start == head {
        0
    } else {
        tail.iter().position(|&c| c == b'\n')? + 1
    };
    let end = tail.iter().rposition(|&c| c == b'\n')?;
    if first_line > end {
        return None;
    }
    tail.truncate(end);
    tail.drain(..first_line);
    Some(Transcript {
        header: meta["payload"].take(),
        tail: String::from_utf8(tail).ok()?,
    })
}

fn app_task(data: &Value, turn: &Value) -> bool {
    let attribution = &data["turn_attribution"];
    attribution["turn_id"] == *turn
        && matches!(
            attribution["turn_trigger"].as_str(),
            Some("app_tool_create_thread" | "app_tool_send_message")
        )
        && turn_id(&attribution["parent_turn_id"])
        && turn_id(&attribution["root_turn_id"])
        && attribution["parent_turn_id"] != *turn
        && data["root_turn_id"] == attribution["root_turn_id"]
}

struct ActiveTurn {
    id: String,
    mode: Mode,
    context_seen: bool,
    delegated: bool,
    ultra: bool,
}

fn latest_turn(
    transcript: &Transcript,
    cwd: Option<&Path>,
    expected: Option<&str>,
    allow_completed: bool,
) -> Option<Turn> {
    let mut turn: Option<ActiveTurn> = None;
    let mut communication = false;
    for line in transcript.tail.lines().filter(|line| {
        [
            "\"task_started\"",
            "\"turn_started\"",
            "\"task_complete\"",
            "\"turn_complete\"",
            "\"turn_aborted\"",
            "\"turn_context\"",
            "\"inter_agent_communication_metadata\"",
            "\"agent_message\"",
        ]
        .iter()
        .any(|kind| line.contains(kind))
    }) {
        let row = read_json(line.as_bytes(), line.len()).ok()?;
        let data = &row["payload"];
        let kind = row["type"].as_str();
        let event = data["type"].as_str();
        if kind == Some("event_msg") && matches!(event, Some("task_started" | "turn_started")) {
            turn = data["turn_id"].as_str().and_then(|id| {
                let mode = Mode::read(&data["collaboration_mode_kind"])?;
                expected
                    .is_none_or(|expected| expected == id)
                    .then(|| ActiveTurn {
                        id: id.to_owned(),
                        mode,
                        context_seen: false,
                        delegated: app_task(data, &data["turn_id"]),
                        ultra: false,
                    })
            });
            communication = false;
            continue;
        }
        let Some(active) = turn.as_mut() else {
            continue;
        };
        match (kind, event) {
            (Some("event_msg"), Some("task_complete" | "turn_complete" | "turn_aborted"))
                if data["turn_id"] == active.id =>
            {
                if !allow_completed || event == Some("turn_aborted") {
                    turn = None;
                }
            }
            (Some("turn_context"), _) if data["turn_id"] == active.id => {
                if let Some(cwd) = cwd {
                    let recorded = Path::new(data["cwd"].as_str()?);
                    if !io::same_directory(recorded, cwd) {
                        return None;
                    }
                }
                let mode = Mode::read(&data["collaboration_mode"]["mode"])?;
                if mode == Mode::Plan {
                    active.mode = mode;
                }
                active.ultra = data["effort"] == "ultra";
                active.context_seen = true;
            }
            (Some("inter_agent_communication_metadata"), _) => {
                communication = data["trigger_turn"].is_boolean();
            }
            (Some("response_item"), Some("agent_message")) => {
                active.delegated |= communication
                    && data["recipient"] == "/root"
                    && data["author"]
                        .as_str()
                        .is_some_and(|author| author.starts_with("/root/"));
                communication = false;
            }
            _ => {}
        }
    }
    let turn = turn?;
    (turn.context_seen || turn.mode == Mode::Plan).then_some(Turn {
        mode: turn.mode,
        delegated: turn.delegated,
        root_ultra: turn.ultra && !turn.delegated && transcript.header["source"].is_string(),
    })
}

fn hook_transcript(state: &Path, payload: &Value, session: &str) -> Option<Transcript> {
    let path = if payload["hook_event_name"] == "SubagentStop" {
        &payload["agent_transcript_path"]
    } else {
        &payload["transcript_path"]
    };
    transcript(state, path, session)
}

pub(super) fn current(
    state: &Path,
    payload: &Value,
    cwd: &Path,
    session: &str,
    turn: Option<&str>,
) -> Option<Turn> {
    let local = hook_transcript(state, payload, session)?;
    let mut turn = latest_turn(&local, Some(cwd), turn, false)?;
    if payload["agent_id"] == session {
        let parent = &local.header["source"]["subagent"]["thread_spawn"]["parent_thread_id"];
        if !turn_id(parent) || parent == session {
            return None;
        }
        turn.delegated = true;
        if payload["hook_event_name"] == "SubagentStop" {
            let parent = transcript(state, &payload["transcript_path"], parent.as_str()?)?;
            if latest_turn(&parent, None, None, true)?.mode == Mode::Plan {
                turn.mode = Mode::Plan;
            }
        }
    } else {
        turn.delegated &=
            local.header["originator"] == "Codex Desktop" && local.header["source"] == "vscode";
    }
    Some(turn)
}

pub(super) fn cancelled(state: &Path, payload: &Value, session: &str, turn: &str) -> bool {
    let Some(transcript) = hook_transcript(state, payload, session) else {
        return false;
    };
    transcript
        .tail
        .lines()
        .filter(|line| {
            line.contains(turn) && (line.contains("\"turn_aborted\"") || line.contains("\"plan\""))
        })
        .filter_map(|line| read_json(line.as_bytes(), line.len()).ok())
        .any(|row| {
            let data = &row["payload"];
            data["turn_id"] == turn
                && match row["type"].as_str() {
                    Some("event_msg") => {
                        data["type"] == "turn_aborted"
                            || (matches!(
                                data["type"].as_str(),
                                Some("task_started" | "turn_started")
                            ) && data["collaboration_mode_kind"] == "plan")
                    }
                    Some("turn_context") => data["collaboration_mode"]["mode"] == "plan",
                    _ => false,
                }
        })
}
