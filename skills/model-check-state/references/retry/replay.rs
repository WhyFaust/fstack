use std::{env, fs, process::ExitCode};

#[derive(Clone, Copy, Debug)]
enum Phase {
    Ready,
    InFlight,
    ReplyPending,
}

fn replay(trace: &str, deduplicate: bool) -> Result<(), String> {
    let request_id = "order-17";
    let mut ledger = Vec::new();
    let mut phase = Phase::Ready;
    let mut attempts = 0;

    for event in trace.lines() {
        phase = match (event, phase) {
            ("send", Phase::Ready) if attempts < 2 => {
                attempts += 1;
                Phase::InFlight
            }
            ("commit", Phase::InFlight) => {
                if !deduplicate || !ledger.contains(&request_id) {
                    ledger.push(request_id);
                }
                Phase::ReplyPending
            }
            ("lose_reply", Phase::ReplyPending) if attempts == 1 => Phase::Ready,
            _ => return Err(format!("Unexpected event {event:?} in {phase:?}")),
        };
        let commits = ledger.iter().filter(|&&id| id == request_id).count();
        println!("{event} id={request_id} commits={commits}");
    }

    let commits = ledger.iter().filter(|&&id| id == request_id).count();
    let expected = if deduplicate { 1 } else { 2 };
    if !matches!(phase, Phase::ReplyPending) || attempts != 2 || commits != expected {
        return Err(format!(
            "Expected two attempts and {expected} commits, got attempts={attempts} commits={commits}"
        ));
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let [mode, path] = args.as_slice() else {
        return Err("Usage: replay broken|fixed TRACE".into());
    };
    let deduplicate = match mode.to_str() {
        Some("broken") => false,
        Some("fixed") => true,
        _ => return Err("Usage: replay broken|fixed TRACE".into()),
    };
    let trace = fs::read_to_string(path).map_err(|error| format!("Read trace: {error}"))?;
    replay(&trace, deduplicate)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
