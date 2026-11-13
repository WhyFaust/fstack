use crate::{json, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Pipeline {
    Unknown,
    Absent,
    Created,
    Preparing,
    WaitingForResource,
    Scheduled,
    Pending,
    Running,
    Manual,
    Success,
    Failed,
    Canceled,
    Skipped,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    expected_head: String,
    head: String,
    expected_target: String,
    target: String,
    draft: bool,
    pipeline_required: bool,
    pipeline: Pipeline,
    required_manual_job_id: Option<u64>,
    may_start: bool,
}

fn next(snapshot: Snapshot) -> Result<Value> {
    for sha in [
        &snapshot.expected_head,
        &snapshot.head,
        &snapshot.expected_target,
        &snapshot.target,
    ] {
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Ожидался полный Git SHA".into());
        }
    }
    if snapshot.required_manual_job_id == Some(0) {
        return Err("Некорректный ID обязательной ручной job".into());
    }
    if !snapshot.head.eq_ignore_ascii_case(&snapshot.expected_head)
        || !snapshot
            .target
            .eq_ignore_ascii_case(&snapshot.expected_target)
    {
        return Ok(json!({"action":"refresh"}));
    }
    if snapshot.draft {
        return Ok(json!({"action":"draft"}));
    }
    if !snapshot.pipeline_required {
        return Ok(json!({"action":"not_required"}));
    }
    let action = match snapshot.pipeline {
        Pipeline::Unknown => "refresh",
        Pipeline::Absent if snapshot.may_start => "create_pipeline",
        Pipeline::Absent => "needs_permission",
        Pipeline::Created
        | Pipeline::Preparing
        | Pipeline::WaitingForResource
        | Pipeline::Scheduled
        | Pipeline::Pending
        | Pipeline::Running
        | Pipeline::Manual
        | Pipeline::Success => match snapshot.required_manual_job_id {
            Some(id) if snapshot.may_start => return Ok(json!({"action":"play_job","id":id})),
            Some(_) => "needs_permission",
            None if matches!(snapshot.pipeline, Pipeline::Manual) => "inspect",
            None if matches!(snapshot.pipeline, Pipeline::Success) => "check_requirements",
            None => "wait",
        },
        Pipeline::Failed | Pipeline::Canceled | Pipeline::Skipped => "inspect",
    };
    Ok(json!({"action":action}))
}

pub fn run(input: impl Read) -> Result<()> {
    let snapshot = serde_json::from_reader(input).map_err(|error| error.to_string())?;
    println!("{}", next(snapshot)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(pipeline: &str, changes: Value) -> Value {
        let head = "a".repeat(40);
        let mut state = json!({
            "expected_head": head, "head": head, "expected_target":"c".repeat(40), "target":"c".repeat(40), "draft": false,
            "pipeline_required": true, "pipeline": pipeline, "may_start": true,
        });
        state
            .as_object_mut()
            .unwrap()
            .extend(changes.as_object().unwrap().clone());
        next(serde_json::from_value(state).unwrap()).unwrap()
    }

    #[test]
    fn ready_mr_only_starts_confirmed_missing_and_authorized_work() {
        assert_eq!(decision("absent", json!({}))["action"], "create_pipeline");
        for (state, changes, expected) in [
            ("absent", json!({"draft":true}), "draft"),
            ("absent", json!({"head":"b".repeat(40)}), "refresh"),
            ("absent", json!({"target":"d".repeat(40)}), "refresh"),
            ("absent", json!({"pipeline_required":false}), "not_required"),
            ("absent", json!({"may_start":false}), "needs_permission"),
            ("unknown", json!({}), "refresh"),
            ("pending", json!({}), "wait"),
            ("running", json!({}), "wait"),
            ("scheduled", json!({}), "wait"),
            ("waiting_for_resource", json!({}), "wait"),
            ("running", json!({"required_manual_job_id":3}), "play_job"),
            ("manual", json!({}), "inspect"),
            (
                "manual",
                json!({"required_manual_job_id":3,"may_start":false}),
                "needs_permission",
            ),
            ("failed", json!({}), "inspect"),
            ("skipped", json!({}), "inspect"),
            ("canceled", json!({}), "inspect"),
            ("success", json!({}), "check_requirements"),
            ("success", json!({"required_manual_job_id":3}), "play_job"),
            (
                "success",
                json!({"required_manual_job_id":3,"may_start":false}),
                "needs_permission",
            ),
        ] {
            assert_eq!(decision(state, changes)["action"], expected, "{state}");
        }
        assert_eq!(
            decision("manual", json!({"required_manual_job_id":3})),
            json!({"action":"play_job","id":3})
        );
    }
}
