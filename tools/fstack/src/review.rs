use crate::{json, package::require, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read};
use std::process::{Command, Output};

const ROUND_LIMIT: u8 = 3;
const SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    base: String,
    head: String,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Auto,
    Full,
    Restart,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Full,
    Delta,
    Findings,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    revision: Revision,
    merge_base: String,
    from: String,
    scope: Scope,
    round: u8,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    id: String,
    path: String,
    line: u32,
    summary: String,
    evidence: String,
}

impl Finding {
    fn validate(&self) -> Result<()> {
        require(
            !self.id.is_empty()
                && self.id.len() <= 64
                && self
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "ID замечания должен состоять из строчных букв ASCII, цифр и дефисов",
        )?;
        require(
            self.line > 0
                && !self.path.trim().is_empty()
                && !self.summary.trim().is_empty()
                && !self.evidence.trim().is_empty(),
            format!("{}: нужны место, нарушение и доказательство", self.id),
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
    Fixed,
    Dismissed,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: String,
    resolution: Resolution,
    evidence: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    finding: Finding,
    head: String,
    decision: Option<RecordedDecision>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecordedDecision {
    head: String,
    resolution: Resolution,
    evidence: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u8,
    revision: Revision,
    merge_base: String,
    round: u8,
    complete: bool,
    evidence: String,
    findings: BTreeMap<String, Entry>,
}

impl Report {
    fn validate(&self) -> Result<()> {
        require(
            self.schema_version == SCHEMA_VERSION,
            "Неизвестная схема отчёта",
        )?;
        require(
            (1..=ROUND_LIMIT).contains(&self.round) && !self.evidence.trim().is_empty(),
            "Некорректный раунд или пустые доказательства отчёта",
        )?;
        for sha in [&self.revision.base, &self.revision.head, &self.merge_base] {
            validate_sha(sha)?;
        }
        for (id, entry) in &self.findings {
            entry.finding.validate()?;
            require(
                id == &entry.finding.id,
                "ID замечания расходится с ключом отчёта",
            )?;
            validate_sha(&entry.head)?;
            if let Some(decision) = &entry.decision {
                validate_sha(&decision.head)?;
                require(
                    !decision.evidence.trim().is_empty(),
                    format!("{id}: нужно основание закрытия"),
                )?;
            }
        }
        Ok(())
    }

    fn open(&self) -> Vec<&str> {
        self.findings
            .iter()
            .filter_map(|(id, entry)| entry.decision.is_none().then_some(id.as_str()))
            .collect()
    }

    fn ready(&self) -> bool {
        self.complete && self.findings.values().all(|entry| entry.decision.is_some())
    }

    fn status(&self) -> &'static str {
        if self.ready() {
            "ready"
        } else if self.round == ROUND_LIMIT {
            "owner"
        } else if self.complete {
            "findings"
        } else {
            "incomplete"
        }
    }

    fn stopped(&self) -> bool {
        self.round == ROUND_LIMIT && !self.ready()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    complete: bool,
    evidence: String,
    #[serde(default)]
    findings: Vec<Finding>,
    #[serde(default)]
    decisions: Vec<Decision>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Plan {
        revision: Revision,
        previous: Option<Report>,
        #[serde(default)]
        mode: Mode,
    },
    Record {
        plan: Plan,
        previous: Option<Report>,
        #[serde(default)]
        mode: Mode,
        outcome: Outcome,
    },
    Check {
        revision: Revision,
        report: Report,
    },
}

enum Next {
    Review(Plan),
    Reuse,
    Owner,
}

fn validate_sha(sha: &str) -> Result<()> {
    require(
        matches!(sha.len(), 40 | 64)
            && sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "Ожидался полный Git SHA в нижнем регистре",
    )
}

fn git(args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("--no-replace-objects")
        .args(args)
        .output()
        .map_err(|error| format!("git: {error}"))
}

fn git_text(args: &[&str]) -> Result<String> {
    let output = git(args)?;
    require(
        output.status.success(),
        format!(
            "git {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn merge_base(revision: &Revision) -> Result<String> {
    for sha in [&revision.base, &revision.head] {
        validate_sha(sha)?;
        let commit = git_text(&["rev-parse", "--verify", &format!("{sha}^{{commit}}")])?;
        require(&commit == sha, "Ревью требует SHA коммитов")?;
    }
    let base = git_text(&["merge-base", "--all", &revision.base, &revision.head])?;
    require(
        base.lines().nth(1).is_none(),
        "Неоднозначная база сравнения",
    )?;
    validate_sha(&base)?;
    Ok(base)
}

fn guidance(base: &str) -> Result<Option<String>> {
    let entry = git_text(&["ls-tree", base, "--", "REVIEW.md"])?;
    if entry.starts_with("100644 blob ") || entry.starts_with("100755 blob ") {
        return git_text(&["show", &format!("{base}:REVIEW.md")]).map(Some);
    }
    Ok(None)
}

fn next(revision: Revision, previous: Option<&Report>, mode: Mode) -> Result<Next> {
    let merge_base = merge_base(&revision)?;
    if let Some(report) = previous {
        report.validate()?;
    }
    let stopped = previous.is_some_and(Report::stopped);
    if mode == Mode::Restart {
        require(stopped, "Новый цикл возможен после передачи владельцу")?;
    } else if stopped {
        return Ok(Next::Owner);
    }
    let same = previous
        .is_some_and(|report| report.revision == revision && report.merge_base == merge_base);
    if mode == Mode::Auto && same && previous.is_some_and(Report::ready) {
        return Ok(Next::Reuse);
    }
    let continuation = previous.filter(|report| {
        mode == Mode::Auto
            && report.complete
            && report.revision.base == revision.base
            && report.merge_base == merge_base
    });
    let (scope, from) = match continuation {
        Some(report) if same => (Scope::Findings, report.revision.head.clone()),
        Some(report)
            if git(&[
                "merge-base",
                "--is-ancestor",
                &report.revision.head,
                &revision.head,
            ])?
            .status
            .success() =>
        {
            (Scope::Delta, report.revision.head.clone())
        }
        _ => (Scope::Full, merge_base.clone()),
    };
    let round = match previous {
        Some(report) if scope == Scope::Findings => report.round,
        Some(report) if mode != Mode::Restart && !report.ready() => report.round + 1,
        _ => 1,
    };
    Ok(Next::Review(Plan {
        revision,
        merge_base,
        from,
        scope,
        round,
    }))
}

fn record(plan: Plan, previous: Option<Report>, outcome: Outcome) -> Result<Value> {
    require(
        !outcome.evidence.trim().is_empty(),
        "Укажи проверенные входы и результат",
    )?;
    require(
        plan.scope != Scope::Findings
            || (!outcome.decisions.is_empty() && outcome.findings.is_empty()),
        "На этом шаге запиши решение по открытому замечанию. Для поиска новых дефектов нужен полный проход",
    )?;
    let mut entries = previous.map_or_else(BTreeMap::new, |report| report.findings);
    let mut updates = BTreeMap::new();
    for finding in outcome.findings {
        finding.validate()?;
        let id = finding.id.clone();
        let entry = Entry {
            finding,
            head: plan.revision.head.clone(),
            decision: None,
        };
        require(
            updates.insert(id.clone(), entry).is_none(),
            format!("{id}: повтор ID замечания"),
        )?;
    }
    for decision in outcome.decisions {
        let id = &decision.id;
        require(
            !updates.contains_key(id),
            format!("{id}: одновременно открыто и закрыто"),
        )?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| format!("{id}: замечание отсутствует"))?;
        require(
            entry.decision.is_none(),
            format!("{id}: замечание уже закрыто"),
        )?;
        require(
            !decision.evidence.trim().is_empty(),
            format!("{id}: нужно основание закрытия"),
        )?;
        entry.decision = Some(RecordedDecision {
            head: plan.revision.head.clone(),
            resolution: decision.resolution,
            evidence: decision.evidence,
        });
    }
    entries.extend(updates);
    let report = Report {
        schema_version: SCHEMA_VERSION,
        revision: plan.revision,
        merge_base: plan.merge_base,
        round: plan.round,
        complete: outcome.complete,
        evidence: outcome.evidence,
        findings: entries,
    };
    Ok(json!({"status": report.status(), "open": report.open(), "report": report}))
}

fn execute(request: Request) -> Result<Value> {
    match request {
        Request::Plan {
            revision,
            previous,
            mode,
        } => {
            let open = previous.as_ref().map_or_else(Vec::new, Report::open);
            match next(revision, previous.as_ref(), mode)? {
                Next::Review(plan) => {
                    let guidance = guidance(&plan.revision.base)?;
                    Ok(
                        json!({"action": "review", "plan": plan, "open": open, "guidance": guidance}),
                    )
                }
                Next::Owner => Ok(json!({"action": "owner", "open": open})),
                Next::Reuse => Ok(json!({"action": "reuse", "open": open})),
            }
        }
        Request::Record {
            plan,
            previous,
            mode,
            outcome,
        } => match next(plan.revision.clone(), previous.as_ref(), mode)? {
            Next::Review(expected) if expected == plan => record(plan, previous, outcome),
            _ => Err("Снимок или план изменился. Сначала запроси следующий шаг".into()),
        },
        Request::Check { revision, report } => {
            report.validate()?;
            let base = merge_base(&revision)?;
            let status = if report.revision == revision && report.merge_base == base {
                report.status()
            } else {
                "stale"
            };
            Ok(
                json!({"status": status, "revision": revision, "report_revision": report.revision,
                "round": report.round, "open": report.open()}),
            )
        }
    }
}

fn run_request(reader: impl Read) -> Result<()> {
    let input = serde_json::from_reader(reader).map_err(|error| error.to_string())?;
    let check = matches!(input, Request::Check { .. });
    let result = execute(input)?;
    println!("{result}");
    require(
        !check || result["status"] == "ready",
        format!("Ревью не завершено, статус {}", result["status"]),
    )
}

pub fn run(args: &[OsString]) -> Result<()> {
    match args {
        [] => run_request(std::io::stdin().lock()),
        [path] => run_request(BufReader::new(
            File::open(path).map_err(|error| error.to_string())?,
        )),
        _ => Err("Использование: fstack review [JSON], без файла читает stdin".into()),
    }
}
