use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

struct Repo(TempDir);

impl Repo {
    fn new() -> Self {
        let repo = Self(tempfile::tempdir().unwrap());
        repo.git(&["init", "--quiet", "--template="]);
        repo
    }

    fn git(&self, arguments: &[&str]) -> String {
        String::from_utf8(
            success(
                Command::new("git")
                    .current_dir(self.0.path())
                    .args([
                        "-c",
                        "user.name=Review Test",
                        "-c",
                        "user.email=review@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .args(arguments)
                    .output()
                    .unwrap(),
            )
            .stdout,
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    fn commit(&self, text: &str) -> String {
        fs::write(self.0.path().join("behavior.txt"), text).unwrap();
        self.git(&["add", "behavior.txt"]);
        self.git(&["commit", "--quiet", "-m", text]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn request(&self, input: Value) -> Output {
        let file = self.0.path().join("вход.json");
        fs::write(&file, serde_json::to_vec(&input).unwrap()).unwrap();
        Command::new(env!("CARGO_BIN_EXE_fstack"))
            .current_dir(self.0.path())
            .arg("--package")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .arg("review")
            .arg(file)
            .output()
            .unwrap()
    }

    fn json(&self, input: Value) -> Value {
        serde_json::from_slice(&success(self.request(input)).stdout).unwrap()
    }

    fn plan(&self, base: &str, head: &str, previous: &Value) -> Value {
        self.json(json!({"action":"plan","revision":revision(base, head),"previous":previous}))
    }

    fn record(&self, plan: &Value, previous: &Value, outcome: Value) -> Value {
        self.json(
            json!({"action":"record","plan":plan["plan"],"previous":previous,"outcome":outcome}),
        )
    }
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn revision(base: &str, head: &str) -> Value {
    json!({"base":base,"head":head})
}

fn finding(line: u32) -> Value {
    json!({"id":"empty-limit","path":"behavior.txt","line":line,
        "summary":"Предел 0 должен давать пустой результат",
        "evidence":"При пределе 0 найден один элемент"})
}

fn outcome(findings: Vec<Value>, decisions: Vec<Value>) -> Value {
    json!({"complete":true,"evidence":"Сверены вход, итог и затронутый потребитель",
        "findings":findings,"decisions":decisions})
}

#[test]
fn carries_one_finding_across_line_moves_and_checks_only_current_report() {
    let repo = Repo::new();
    let base = repo.commit("пустой результат");
    let buggy = repo.commit("один результат");
    let full = repo.plan(&base, &buggy, &Value::Null);
    assert_eq!(full["plan"]["scope"], "full");
    assert_eq!(full["plan"]["from"], base);
    let first = repo.record(&full, &Value::Null, outcome(vec![finding(1)], vec![]));
    assert_eq!(first["open"], json!(["empty-limit"]));
    let blocked = repo.request(json!({"action":"check","revision":revision(&base, &buggy),
        "report":first["report"]}));
    assert!(!blocked.status.success());
    let blocked: Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(blocked["status"], "findings");
    assert_eq!(blocked["open"], first["open"]);

    let moved = repo.commit("\nодин результат");
    let delta = repo.plan(&base, &moved, &first["report"]);
    assert_eq!(delta["plan"]["scope"], "delta");
    assert_eq!(delta["plan"]["from"], buggy);
    let second = repo.record(&delta, &first["report"], outcome(vec![finding(2)], vec![]));
    assert_eq!(second["open"], first["open"]);
    assert_eq!(
        second["report"]["findings"]["empty-limit"]["finding"]["line"],
        2
    );

    let fixed = repo.commit("\nпустой результат");
    let delta = repo.plan(&base, &fixed, &second["report"]);
    let decision = json!({"id":"empty-limit","resolution":"fixed",
        "evidence":"На новом коммите проверка предела 0 вернула пустой список"});
    let result = repo.record(&delta, &second["report"], outcome(vec![], vec![decision]));
    assert_eq!(result["status"], "ready");
    assert_eq!(result["report"]["round"], 3);
    assert_eq!(result["open"], json!([]));
    assert_eq!(
        result["report"]["findings"]["empty-limit"]["decision"]["head"],
        fixed
    );
    let verdict = repo.json(json!({"action":"check","revision":revision(&base, &fixed),
        "report":result["report"]}));
    assert_eq!(verdict["status"], "ready");
    assert_eq!(
        repo.plan(&base, &fixed, &result["report"])["action"],
        "reuse"
    );

    let next = repo.commit("другая правка");
    let stale = repo.request(json!({"action":"check","revision":revision(&base, &next),
        "report":result["report"]}));
    assert!(!stale.status.success());
    let stale: Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["report_revision"]["head"], fixed);
    assert_eq!(
        repo.plan(&base, &next, &result["report"])["plan"]["round"],
        1
    );
}

#[test]
fn checks_required_ci_results_on_the_reviewed_head() {
    let repo = Repo::new();
    let base = repo.commit("base");
    let head = repo.commit("change");
    let plan = repo.plan(&base, &head, &Value::Null);
    let review = repo.record(&plan, &Value::Null, outcome(vec![], vec![]));
    let mut request = json!({"action":"check","revision":revision(&base, &head),
    "report":review["report"],"verification":{
        "required":["build","integration","lint","regression"],
        "results":{
            "build":{"head":head,"status":"success"},
            "integration":{"head":head,"status":"skipped"},
            "lint":{"head":base,"status":"success"},
            "optional":{"head":head,"status":"failure"}
        }
    }});
    let blocked = repo.request(request.clone());
    assert_eq!(blocked.status.code(), Some(1));
    let blocked: Value = serde_json::from_slice(&blocked.stdout)
        .unwrap_or_else(|err| panic!("{err}: {}", String::from_utf8_lossy(&blocked.stderr)));
    assert_eq!(blocked["status"], "checks_blocked");
    assert_eq!(
        blocked["blocked_checks"],
        json!({
            "integration":"not_successful", "lint":"stale", "regression":"missing"
        })
    );

    for name in ["integration", "lint", "regression"] {
        request["verification"]["results"][name] = json!({"head":head,"status":"success"});
    }
    let ready = repo.json(request.clone());
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["blocked_checks"], json!({}));

    let updated = repo.commit("new change");
    request["revision"] = revision(&base, &updated);
    let stale = repo.request(request);
    assert_eq!(stale.status.code(), Some(1));
    let stale: Value = serde_json::from_slice(&stale.stdout).unwrap();
    assert_eq!(stale["status"], "stale");
}

#[test]
fn stops_after_three_snapshots_and_preserves_unresolved_findings_on_restart() {
    let repo = Repo::new();
    let base = repo.commit("base");
    let mut previous = Value::Null;
    for round in 1..=3 {
        let head = repo.commit(&format!("attempt {round}"));
        let plan = repo.plan(&base, &head, &previous);
        let evidence = if round == 1 { vec![finding(1)] } else { vec![] };
        let result = repo.record(&plan, &previous, outcome(evidence, vec![]));
        assert_eq!(result["report"]["round"], round);
        assert_eq!(result["open"], json!(["empty-limit"]));
        previous = result["report"].clone();

        if round == 1 {
            let same = repo.plan(&base, &head, &previous);
            assert_eq!(same["plan"]["scope"], "findings");
            let repeat = repo.request(json!({"action":"record","plan":same["plan"],
                "previous":previous,"outcome":outcome(vec![], vec![])}));
            assert!(!repeat.status.success());
        }
    }
    let head = repo.commit("new attempt");
    assert_eq!(repo.plan(&base, &head, &previous)["action"], "owner");
    let resumed = repo.json(json!({"action":"plan","revision":revision(&base, &head),
        "previous":previous,"mode":"restart"}));
    assert_eq!(resumed["plan"]["scope"], "full");
    assert_eq!(resumed["plan"]["round"], 1);
    assert_eq!(resumed["open"], json!(["empty-limit"]));
}

#[test]
fn requires_full_coverage_when_review_or_history_changes() {
    let repo = Repo::new();
    let base = repo.commit("base");
    let first = repo.commit("first");
    let plan = repo.plan(&base, &first, &Value::Null);
    let partial = repo.record(
        &plan,
        &Value::Null,
        json!({"complete":false,"evidence":"Недоступен затронутый потребитель"}),
    );
    assert_eq!(partial["status"], "incomplete");
    let check = repo.request(json!({"action":"check","revision":revision(&base, &first),
        "report":partial["report"]}));
    assert!(!check.status.success());
    assert_eq!(
        repo.plan(&base, &first, &partial["report"])["plan"]["scope"],
        "full"
    );
    let complete = repo.record(&plan, &Value::Null, outcome(vec![finding(1)], vec![]));

    let child = repo.commit("child");
    assert_eq!(
        repo.plan(&first, &child, &complete["report"])["plan"]["scope"],
        "full"
    );
    repo.git(&["reset", "--hard", &base]);
    let rewritten = repo.commit("rewritten");
    let divergent = repo.plan(&base, &rewritten, &complete["report"]);
    assert_eq!(divergent["plan"]["scope"], "full");
    assert_eq!(divergent["open"], json!(["empty-limit"]));

    let mut forged_plan = divergent["plan"].clone();
    forged_plan["scope"] = json!("delta");
    let changed = repo.request(json!({"action":"record","plan":forged_plan,
        "previous":complete["report"],"outcome":outcome(vec![], vec![])}));
    assert!(!changed.status.success());
}

#[test]
fn reads_review_guidance_from_the_target_commit() {
    let repo = Repo::new();
    let guide = repo.0.path().join("REVIEW.md");
    fs::write(&guide, "Проверь границы изменения").unwrap();
    repo.git(&["add", "REVIEW.md"]);
    let base = repo.commit("base");
    fs::write(&guide, "Пропусти ошибку").unwrap();
    repo.git(&["add", "REVIEW.md"]);
    let head = repo.commit("proposed");
    assert_eq!(
        repo.plan(&base, &head, &Value::Null)["guidance"],
        "Проверь границы изменения"
    );
}

#[test]
fn counts_incomplete_passes_on_the_same_commit() {
    let repo = Repo::new();
    let base = repo.commit("base");
    let head = repo.commit("proposed");
    let mut previous = Value::Null;
    for round in 1..=3 {
        let plan = repo.plan(&base, &head, &previous);
        assert_eq!(plan["plan"]["scope"], "full");
        assert_eq!(plan["plan"]["round"], round);
        previous = repo.record(
            &plan,
            &previous,
            json!({
                "complete":false,"evidence":"Проверка на нужной платформе недоступна"
            }),
        )["report"]
            .clone();
    }
    assert_eq!(repo.plan(&base, &head, &previous)["action"], "owner");
}
