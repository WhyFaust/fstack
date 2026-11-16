use super::{format, io, read_json};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Component, Path},
};

const HEADER_LIMIT: u64 = 256 * 1024;
const TAIL_LIMIT: u64 = 8 * 1024 * 1024;

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

pub(super) fn ordinary(
    state: &Path,
    payload: &Value,
    cwd: &Path,
    session: &str,
    turn: &str,
) -> Option<()> {
    let path = Path::new(payload["transcript_path"].as_str()?);
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
    let meta = read_json(first.as_slice(), first.len()).ok()?;
    let header = &meta["payload"];
    if meta["type"] != "session_meta"
        || header["id"] != session
        || header["originator"] != "Codex Desktop"
        || header["source"] != "vscode"
    {
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
    let lines = std::str::from_utf8(tail.get(first_line..end)?).ok()?;
    let mut active = false;
    let mut default_mode_seen = false;
    for line in lines.lines().filter(|line| {
        [
            "\"task_started\"",
            "\"task_complete\"",
            "\"turn_aborted\"",
            "\"turn_context\"",
        ]
        .iter()
        .any(|kind| line.contains(kind))
    }) {
        let row = read_json(line.as_bytes(), line.len()).ok()?;
        let data = &row["payload"];
        match (row["type"].as_str(), data["type"].as_str()) {
            (Some("event_msg"), Some("task_started")) => {
                let attribution = &data["turn_attribution"];
                active = data["turn_id"] == turn
                    && attribution["turn_id"] == turn
                    && data["collaboration_mode_kind"] == "default"
                    && matches!(
                        attribution["turn_trigger"].as_str(),
                        Some("app_tool_create_thread" | "app_tool_send_message")
                    )
                    && turn_id(&attribution["parent_turn_id"])
                    && turn_id(&attribution["root_turn_id"])
                    && attribution["parent_turn_id"] != turn
                    && data["root_turn_id"] == attribution["root_turn_id"];
                default_mode_seen = false;
            }
            (Some("event_msg"), Some("task_complete" | "turn_aborted"))
                if data["turn_id"] == turn =>
            {
                active = false;
            }
            (Some("turn_context"), _) if active && data["turn_id"] == turn => {
                let recorded_cwd = Path::new(data["cwd"].as_str()?);
                if data["collaboration_mode"]["mode"] != "default"
                    || !io::same_directory(recorded_cwd, cwd)
                {
                    return None;
                }
                default_mode_seen = true;
            }
            _ => {}
        }
    }
    (active && default_mode_seen).then_some(())
}
