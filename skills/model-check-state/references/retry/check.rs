use std::collections::{HashMap, VecDeque};
use std::{env, fs, io, path::Path, process::ExitCode};

const MAX_ATTEMPTS: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Phase {
    Ready,
    InFlight,
    ReplyPending,
    Done,
    Exhausted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct State {
    phase: Phase,
    attempts: u8,
    commits: u8,
}

impl State {
    fn successors(self, deduplicate: bool) -> Vec<(&'static str, Self)> {
        match self.phase {
            Phase::Ready if self.attempts < MAX_ATTEMPTS => vec![(
                "send",
                Self {
                    phase: Phase::InFlight,
                    attempts: self.attempts + 1,
                    ..self
                },
            )],
            Phase::InFlight => vec![(
                "commit",
                Self {
                    phase: Phase::ReplyPending,
                    commits: self.commits + u8::from(!deduplicate || self.commits == 0),
                    ..self
                },
            )],
            Phase::ReplyPending => vec![
                (
                    "lose_reply",
                    Self {
                        phase: if self.attempts < MAX_ATTEMPTS {
                            Phase::Ready
                        } else {
                            Phase::Exhausted
                        },
                        ..self
                    },
                ),
                (
                    "deliver_reply",
                    Self {
                        phase: Phase::Done,
                        ..self
                    },
                ),
            ],
            _ => Vec::new(),
        }
    }
}

fn check(deduplicate: bool, trace_path: &Path) -> io::Result<ExitCode> {
    let initial = State {
        phase: Phase::Ready,
        attempts: 0,
        commits: 0,
    };
    let mut queue = VecDeque::from([initial]);
    let mut predecessors = HashMap::from([(initial, None)]);

    while let Some(state) = queue.pop_front() {
        if state.commits > 1 {
            let mut trace = Vec::new();
            let mut cursor = state;
            while let Some((previous, event)) = predecessors[&cursor] {
                trace.push(event);
                cursor = previous;
            }
            trace.reverse();
            fs::write(trace_path, trace.join("\n") + "\n")?;
            eprintln!("AtMostOneCommit violated: {state:?}");
            return Ok(ExitCode::from(1));
        }

        for (event, next) in state.successors(deduplicate) {
            if let std::collections::hash_map::Entry::Vacant(entry) = predecessors.entry(next) {
                entry.insert(Some((state, event)));
                queue.push_back(next);
            }
        }
    }

    fs::write(trace_path, "")?;
    eprintln!(
        "AtMostOneCommit holds: states={} queue={}",
        predecessors.len(),
        queue.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [mode, trace] if mode == "broken" => check(false, Path::new(trace)),
        [mode, trace] if mode == "fixed" => check(true, Path::new(trace)),
        _ => {
            eprintln!("Usage: check broken|fixed TRACE");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("Write trace: {error}");
            ExitCode::from(2)
        }
    }
}
