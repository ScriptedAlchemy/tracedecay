//! Deterministic layer of the behavioral memory-hygiene eval suite.
//!
//! Each scenario in `eval/scenarios/*.json` seeds a throwaway fixture project,
//! replays a scripted tool-call sequence through the real `tracedecay` binary
//! (the same write/curation paths an agent hits over MCP), then asserts on
//! end-state with plain SQL against the fixture's `.tracedecay/tracedecay.db`.
//! No LLM is involved; the cost-gated real-model layer lives in
//! `eval/run_real_model.py`.
//!
//! Scenario taxonomy adapted from the mnemon harness eval suite
//! (<https://github.com/mnemon-dev/mnemon>, Apache-2.0).
//!
//! Stable scenarios fail when a violation is accepted and leaves a bad end-state.
//! The harness still understands `pending-sibling` for future branch-split work,
//! but shipped hygiene contracts should be marked stable.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

#[derive(Deserialize)]
struct Scenario {
    schema_version: u32,
    id: String,
    #[allow(dead_code)]
    title: String,
    contract: ContractStatus,
    setup: Setup,
    deterministic: Deterministic,
    assertions: Vec<Assertion>,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum ContractStatus {
    Stable,
    PendingSibling,
}

#[derive(Deserialize)]
struct Setup {
    #[serde(default)]
    facts: Vec<SeedFact>,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct SeedFact {
    content: String,
    category: String,
    source: String,
    trust: f64,
    retrieval_count: i64,
}

#[derive(Deserialize)]
struct Deterministic {
    #[serde(default)]
    well_behaved: Vec<Step>,
    violation: Option<Violation>,
}

#[derive(Deserialize)]
struct Violation {
    expectation: Expectation,
    steps: Vec<Step>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum Expectation {
    /// The assertion set must flag the bad end-state (no machinery defense
    /// exists or should exist for this scenario).
    Detect,
    /// Either the write path defends (all assertions pass) or the instrument
    /// detects (some assertion fails after the write went through).
    DefendOrDetect,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Step {
    Tool { tool: String, args: Value },
    Curate { apply: bool },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Assertion {
    Sql {
        name: String,
        sql: String,
        op: CompareOp,
        value: i64,
        #[serde(default)]
        phase: AssertionPhase,
        #[serde(default)]
        #[allow(dead_code)]
        deterministic_only: bool,
    },
    CurateDeletesSource {
        name: String,
        source: String,
        expected: bool,
        #[serde(default)]
        phase: AssertionPhase,
    },
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum CompareOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Deserialize, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum AssertionPhase {
    #[default]
    Both,
    WellBehavedOnly,
    ViolationOnly,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    WellBehaved,
    Violation,
}

fn should_skip_assertion(phase: Phase, assertion_phase: AssertionPhase) -> bool {
    matches!(
        (phase, assertion_phase),
        (Phase::Violation, AssertionPhase::WellBehavedOnly)
            | (Phase::WellBehaved, AssertionPhase::ViolationOnly)
    )
}

struct AssertionOutcome {
    name: String,
    passed: bool,
    detail: String,
}

fn scenario_path(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("eval/scenarios")
        .join(format!("{id}.json"))
}

fn load_scenario(id: &str) -> Scenario {
    let path = scenario_path(id);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let scenario: Scenario = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
    assert_eq!(scenario.schema_version, 1, "unsupported scenario schema");
    assert_eq!(scenario.id, id, "scenario id must match its file name");
    scenario
}

struct Fixture {
    home: TempDir,
    project: TempDir,
}

impl Fixture {
    fn db_path(&self) -> PathBuf {
        self.project.path().join(".tracedecay/tracedecay.db")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
        command
            .current_dir(self.project.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env(
                "TRACEDECAY_GLOBAL_DB",
                self.home.path().join(".tracedecay/global.db"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn tracedecay: {e}"));
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("failed to poll child: {e}"))
        {
            let mut stdout = Vec::new();
            if let Some(mut out) = child.stdout.take() {
                std::io::Read::read_to_end(&mut out, &mut stdout)
                    .unwrap_or_else(|e| panic!("failed to read stdout: {e}"));
            }
            let mut stderr = Vec::new();
            if let Some(mut err) = child.stderr.take() {
                std::io::Read::read_to_end(&mut err, &mut stderr)
                    .unwrap_or_else(|e| panic!("failed to read stderr: {e}"));
            }
            return Output {
                status,
                stdout,
                stderr,
            };
        }
        assert!(
            started.elapsed() < timeout,
            "tracedecay hung after {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_ok(fixture: &Fixture, args: &[&str]) -> Output {
    let mut command = fixture.command();
    command.args(args);
    let output = run_with_timeout(command, Duration::from_secs(120));
    assert!(
        output.status.success(),
        "`tracedecay {}` failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// Runs a scalar SQL query against the fixture DB. The connection is opened
/// fresh and dropped before any further binary invocation, so the test never
/// holds a lock across subprocess writes.
fn query_scalar(fixture: &Fixture, sql: &str) -> i64 {
    let db_path = fixture.db_path();
    runtime().block_on(async move {
        let db = libsql::Builder::new_local(&db_path)
            .build()
            .await
            .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
        let conn = db.connect().expect("db connect");
        let mut rows = conn
            .query(sql, ())
            .await
            .unwrap_or_else(|e| panic!("query `{sql}`: {e}"));
        let row = rows
            .next()
            .await
            .unwrap_or_else(|e| panic!("row for `{sql}`: {e}"))
            .unwrap_or_else(|| panic!("no rows for `{sql}`"));
        row.get::<i64>(0)
            .unwrap_or_else(|e| panic!("scalar for `{sql}`: {e}"))
    })
}

fn execute_sql(fixture: &Fixture, sql: &str, params: impl libsql::params::IntoParams) {
    let db_path = fixture.db_path();
    let sql = sql.to_string();
    runtime().block_on(async move {
        let db = libsql::Builder::new_local(&db_path)
            .build()
            .await
            .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
        let conn = db.connect().expect("db connect");
        conn.execute(&sql, params)
            .await
            .unwrap_or_else(|e| panic!("execute `{sql}`: {e}"));
    });
}

fn fact_ids_by_source(fixture: &Fixture) -> HashMap<String, HashSet<i64>> {
    let db_path = fixture.db_path();
    runtime().block_on(async move {
        let db = libsql::Builder::new_local(&db_path)
            .build()
            .await
            .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
        let conn = db.connect().expect("db connect");
        let mut rows = conn
            .query("SELECT source, fact_id FROM memory_facts", ())
            .await
            .expect("list fact sources");
        let mut map: HashMap<String, HashSet<i64>> = HashMap::new();
        while let Some(row) = rows.next().await.expect("source row") {
            let source = row.get::<String>(0).expect("source column");
            let fact_id = row.get::<i64>(1).expect("fact_id column");
            map.entry(source).or_default().insert(fact_id);
        }
        map
    })
}

fn build_fixture(setup: &Setup) -> Fixture {
    let fixture = Fixture {
        home: TempDir::new().expect("home tempdir"),
        project: TempDir::new().expect("project tempdir"),
    };
    let src = fixture.project.path().join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(src.join("lib.rs"), "pub fn eval_fixture_marker() {}\n").expect("write lib.rs");
    for (name, contents) in &setup.files {
        std::fs::write(fixture.project.path().join(name), contents)
            .unwrap_or_else(|e| panic!("write fixture file {name}: {e}"));
    }
    run_ok(&fixture, &["init"]);
    for fact in &setup.facts {
        let args = serde_json::json!({
            "action": "add",
            "content": fact.content,
            "category": fact.category,
        });
        run_ok(
            &fixture,
            &["tool", "fact_store", "--args", &args.to_string()],
        );
        execute_sql(
            &fixture,
            "UPDATE memory_facts SET trust_score = ?1, retrieval_count = ?2, source = ?3 \
             WHERE content = ?4",
            libsql::params![
                fact.trust,
                fact.retrieval_count,
                fact.source.as_str(),
                fact.content.as_str()
            ],
        );
    }
    fixture
}

struct StepResult {
    succeeded: bool,
}

/// Executes one scripted step; returns whether the underlying write/command
/// was accepted. A refused step (non-zero exit) is a legal outcome for
/// violation sequences once the hygiene write path defends against them.
fn execute_step(fixture: &Fixture, step: &Step, dry_run_report: &mut Option<Value>) -> StepResult {
    match step {
        Step::Tool { tool, args } => {
            let mut command = fixture.command();
            command.args(["tool", tool, "--args", &args.to_string()]);
            let output = run_with_timeout(command, Duration::from_secs(120));
            StepResult {
                succeeded: output.status.success(),
            }
        }
        Step::Curate { apply } => {
            let mut args = vec!["memory", "curate"];
            if *apply {
                args.push("--apply");
            }
            let output = run_ok(fixture, &args);
            if !*apply {
                let report: Value = serde_json::from_slice(&output.stdout)
                    .unwrap_or_else(|e| panic!("curate dry-run output was not JSON: {e}"));
                *dry_run_report = Some(report);
            }
            StepResult { succeeded: true }
        }
    }
}

fn compare(op: CompareOp, actual: i64, expected: i64) -> bool {
    match op {
        CompareOp::Eq => actual == expected,
        CompareOp::Ne => actual != expected,
        CompareOp::Gt => actual > expected,
        CompareOp::Gte => actual >= expected,
        CompareOp::Lt => actual < expected,
        CompareOp::Lte => actual <= expected,
    }
}

fn op_symbol(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Eq => "==",
        CompareOp::Ne => "!=",
        CompareOp::Gt => ">",
        CompareOp::Gte => ">=",
        CompareOp::Lt => "<",
        CompareOp::Lte => "<=",
    }
}

fn curate_delete_ids(report: &Value) -> HashSet<i64> {
    let mut ids: HashSet<i64> = report
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|action| action.get("op").and_then(Value::as_str) == Some("delete"))
        .filter_map(|action| action.get("fact_id").and_then(Value::as_i64))
        .collect();

    for key in ["secret_like", "transient", "supersession"] {
        if let Some(entries) = report
            .get("hygiene_candidates")
            .and_then(|hygiene_candidates| hygiene_candidates.get(key))
            .and_then(Value::as_array)
        {
            ids.extend(
                entries
                    .iter()
                    .filter(|candidate| {
                        candidate.get("recommended_op").and_then(Value::as_str) == Some("delete")
                            && candidate
                                .get("review_required")
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                    })
                    .filter_map(|action| action.get("fact_id").and_then(Value::as_i64)),
            );
        }
    }

    ids
}

fn evaluate_assertions(
    scenario: &Scenario,
    fixture: &Fixture,
    phase: Phase,
    seeded_sources: &HashMap<String, HashSet<i64>>,
    dry_run_report: &Option<Value>,
) -> Vec<AssertionOutcome> {
    let mut outcomes = Vec::new();
    for assertion in &scenario.assertions {
        match assertion {
            Assertion::Sql {
                name,
                sql,
                op,
                value,
                phase: assertion_phase,
                deterministic_only: _,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let actual = query_scalar(fixture, sql);
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: compare(*op, actual, *value),
                    detail: format!("{actual} {} {value} (`{sql}`)", op_symbol(*op)),
                });
            }
            Assertion::CurateDeletesSource {
                name,
                source,
                expected,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let Some(report) = dry_run_report else {
                    if phase == Phase::Violation {
                        continue;
                    }
                    panic!(
                        "[{}] assertion `{name}` needs a curate dry-run step before it",
                        scenario.id
                    );
                };
                let delete_ids = curate_delete_ids(report);
                let source_ids = seeded_sources.get(source).cloned().unwrap_or_default();
                let any_deleted = delete_ids.intersection(&source_ids).next().is_some();
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: any_deleted == *expected,
                    detail: format!(
                        "delete ops touching source `{source}`: {} (expected: {expected})",
                        any_deleted
                    ),
                });
            }
        }
    }
    outcomes
}

fn format_outcomes(outcomes: &[AssertionOutcome]) -> String {
    outcomes
        .iter()
        .map(|o| {
            format!(
                "  [{}] {} — {}",
                if o.passed { "pass" } else { "FAIL" },
                o.name,
                o.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_scenario(id: &str) {
    let scenario = load_scenario(id);

    // Phase A: a well-behaved agent's tool sequence must leave a compliant
    // end-state.
    let fixture = build_fixture(&scenario.setup);
    let seeded_sources = fact_ids_by_source(&fixture);
    let mut dry_run_report = None;
    for step in &scenario.deterministic.well_behaved {
        let result = execute_step(&fixture, step, &mut dry_run_report);
        assert!(
            result.succeeded,
            "[{id}] well-behaved step was refused; compliant writes must be accepted"
        );
    }
    let outcomes = evaluate_assertions(
        &scenario,
        &fixture,
        Phase::WellBehaved,
        &seeded_sources,
        &dry_run_report,
    );
    assert!(
        outcomes.iter().all(|o| o.passed),
        "[{id}] well-behaved phase failed:\n{}",
        format_outcomes(&outcomes)
    );
    println!("[{id}] well-behaved phase:\n{}", format_outcomes(&outcomes));

    // Phase B: a misbehaving sequence must be either defended against by the
    // write path or detected by the assertion set (instrument self-check).
    let Some(violation) = &scenario.deterministic.violation else {
        return;
    };
    let fixture = build_fixture(&scenario.setup);
    let seeded_sources = fact_ids_by_source(&fixture);
    let mut dry_run_report = None;
    let mut any_step_succeeded = false;
    for step in &violation.steps {
        let result = execute_step(&fixture, step, &mut dry_run_report);
        any_step_succeeded |= result.succeeded;
    }
    let outcomes = evaluate_assertions(
        &scenario,
        &fixture,
        Phase::Violation,
        &seeded_sources,
        &dry_run_report,
    );
    let all_passed = outcomes.iter().all(|o| o.passed);
    match violation.expectation {
        Expectation::Detect => {
            assert!(
                !all_passed,
                "[{id}] violation went undetected — the assertion set is blind \
                 (or unexpected machinery now defends this scenario; if so, move it \
                 to defend-or-detect):\n{}",
                format_outcomes(&outcomes)
            );
            println!(
                "[{id}] violation phase: DETECTED (instrument works)\n{}",
                format_outcomes(&outcomes)
            );
        }
        Expectation::DefendOrDetect => {
            if all_passed {
                println!(
                    "[{id}] violation phase: DEFENDED — machinery contract landed\n{}",
                    format_outcomes(&outcomes)
                );
            } else if any_step_succeeded {
                assert!(
                    scenario.contract == ContractStatus::PendingSibling,
                    "[{id}] defense regressed: violation was accepted and left a bad \
                     end-state on a stable-contract scenario:\n{}",
                    format_outcomes(&outcomes)
                );
                println!(
                    "[{id}] violation phase: PENDING-SIBLING — instrument detected the \
                     violation; write-path defense not landed yet\n{}",
                    format_outcomes(&outcomes)
                );
            } else {
                panic!(
                    "[{id}] inconsistent: every violation step was refused but the \
                     end-state is still bad:\n{}",
                    format_outcomes(&outcomes)
                );
            }
        }
    }
}

#[test]
fn eval_memory_no_pollution() {
    run_scenario("memory-no-pollution");
}

#[test]
fn eval_memory_secret_rejection() {
    run_scenario("memory-secret-rejection");
}

#[test]
fn eval_memory_skip_local() {
    run_scenario("memory-skip-local");
}

#[test]
fn eval_memory_supersede_without_dup() {
    run_scenario("memory-supersede-without-dup");
}

#[test]
fn eval_memory_multiturn_continuity() {
    run_scenario("memory-multiturn-continuity");
}

#[test]
fn eval_memory_curation_conservatism() {
    run_scenario("memory-curation-conservatism");
}

/// Every scenario file must have a matching `#[test]` above; this guards
/// against silently-unwired scenarios.
#[test]
fn every_scenario_file_is_wired() {
    let wired: HashSet<&str> = [
        "memory-no-pollution",
        "memory-secret-rejection",
        "memory-skip-local",
        "memory-supersede-without-dup",
        "memory-multiturn-continuity",
        "memory-curation-conservatism",
    ]
    .into_iter()
    .collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/scenarios");
    let mut found = HashSet::new();
    for entry in std::fs::read_dir(&dir).expect("read eval/scenarios") {
        let path = entry.expect("scenario entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("scenario file stem")
                .to_string();
            // Validates the file parses with the harness schema.
            load_scenario(&id);
            found.insert(id);
        }
    }
    let found_refs: HashSet<&str> = found.iter().map(String::as_str).collect();
    assert_eq!(
        found_refs, wired,
        "eval/scenarios/*.json and the #[test] list must stay in sync"
    );
}
