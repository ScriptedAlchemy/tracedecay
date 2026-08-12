//! Behavioral retained-memory evals over the exact retained-memory tools.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_application::retained_surfaces::{
    FactProjectionV1, FactSearchHitV1, FactStoreAddCommitV1, FactStoreAddResultV1,
    FactStoreGetResultV1, FactStoreListResultV1, FactStoreSearchResultV1, FactV1,
    MemoryAutomationCommittedReceiptV1, MemoryAutomationCurationOperationEffectV1,
    MemoryAutomationCurationRemoveDispositionV1, MemoryAutomationRunResultV1,
    MemoryAutomationTaskV1, MemoryStatusResultV1,
};
use tracedecay_domain::FactId;

use crate::common;

#[path = "memory_eval/assertions.rs"]
mod assertions;

use assertions::{Assertion, AssertionOutcome, CompareOp, Phase, should_skip_assertion};

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
    #[serde(default)]
    preload_query: Option<String>,
    #[serde(default)]
    preload_searches: usize,
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
    Detect,
    DefendOrDetect,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Step {
    Tool { tool: String, args: Value },
    MemoryCuration,
}

type FactIndex = BTreeMap<String, Vec<FactId>>;

struct Fixture {
    /// Declared first so the daemon terminates before temporary directories are removed.
    _daemon: Option<common::DaemonProcess>,
    _home: TempDir,
    home_path: PathBuf,
    _project: TempDir,
    project_path: PathBuf,
}

impl Fixture {
    fn start_daemon(&mut self) {
        assert!(self._daemon.is_none(), "fixture daemon already running");
        self._daemon = Some(common::spawn_tracedecay_daemon(&self.home_path));
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
        command
            .current_dir(&self.project_path)
            .env("HOME", &self.home_path)
            .env("USERPROFILE", &self.home_path)
            .env("XDG_CONFIG_HOME", self.home_path.join(".config"))
            .env(
                tracedecay::config::USER_DATA_DIR_ENV,
                self.home_path.join(".tracedecay"),
            )
            .env(
                "TRACEDECAY_GLOBAL_DB",
                self.home_path.join(".tracedecay/global.db"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

fn scenario_path(id: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("eval/scenarios")
        .join(format!("{id}.json"))
}

fn load_scenario(id: &str) -> Scenario {
    let path = scenario_path(id);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let scenario: Scenario = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    assert_eq!(scenario.schema_version, 1, "unsupported scenario schema");
    assert_eq!(scenario.id, id, "scenario id must match its file name");
    scenario
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("failed to spawn tracedecay: {error}"));
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .unwrap_or_else(|error| panic!("failed to poll tracedecay: {error}"))
        {
            let mut stdout = Vec::new();
            if let Some(mut output) = child.stdout.take() {
                std::io::Read::read_to_end(&mut output, &mut stdout)
                    .unwrap_or_else(|error| panic!("failed to read stdout: {error}"));
            }
            let mut stderr = Vec::new();
            if let Some(mut output) = child.stderr.take() {
                std::io::Read::read_to_end(&mut output, &mut stderr)
                    .unwrap_or_else(|error| panic!("failed to read stderr: {error}"));
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
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn run_ok(fixture: &Fixture, args: &[&str]) -> Output {
    let mut command = fixture.command();
    command.args(args);
    let output = run_with_timeout(command, Duration::from_secs(120));
    assert!(
        output.status.success(),
        "`tracedecay {}` failed with status {}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn exact_args(mut args: Value) -> Value {
    let object = args
        .as_object_mut()
        .expect("exact fact-store arguments must be an object");
    object.entry("format").or_insert_with(|| json!("json"));
    args
}

fn run_exact_value(fixture: &Fixture, tool: &str, args: Value) -> Value {
    assert!(
        tool.starts_with("tracedecay_"),
        "evals must use the exact registered tool name: {tool}"
    );
    let args = exact_args(args);
    let output = run_ok(fixture, &["tool", tool, "--args", &args.to_string()]);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{tool} output was not FactRecordV1 JSON: {error}"))
}

fn run_exact<T: serde::de::DeserializeOwned>(fixture: &Fixture, tool: &str, args: Value) -> T {
    let response = run_exact_value(fixture, tool, args);
    serde_json::from_value(response).unwrap_or_else(|error| {
        panic!("{tool} output violated its retained result schema: {error}")
    })
}

fn canonical_test_dir(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path)
        .unwrap_or_else(|error| panic!("failed to create test dir {}: {error}", path.display()));
    path.canonicalize().unwrap_or_else(|error| {
        panic!(
            "failed to canonicalize test dir {}: {error}",
            path.display()
        )
    })
}

fn initialize_fixture_project(fixture: &Fixture) {
    let profile_root = fixture.home_path.join(".tracedecay");
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async {
        common::write_empty_global_db_schema(&profile_root.join("global.db")).await;
        let tracedecay = TraceDecay::init_with_options(&fixture.project_path, options)
            .await
            .unwrap_or_else(|error| panic!("initialize fixture project: {error}"));
        tracedecay
            .checkpoint()
            .await
            .unwrap_or_else(|error| panic!("checkpoint fixture project: {error}"));
        tracedecay.close();
    });
}

fn seed_setup_facts(fixture: &Fixture, facts: &[SeedFact]) -> FactIndex {
    let mut index = FactIndex::new();
    for seed in facts {
        let added: FactStoreAddResultV1 = run_exact(
            fixture,
            "tracedecay_fact_store_add",
            json!({
                "content": seed.content,
                "category": seed.category,
                "source_label": seed.source,
                "trust": seed.trust,
            }),
        );
        let projection = match added {
            FactStoreAddResultV1::SecretRejected => panic!("seed fact rejected: {}", seed.content),
            FactStoreAddResultV1::NormalizedDuplicate { fact, .. }
            | FactStoreAddResultV1::Committed {
                result:
                    FactStoreAddCommitV1::Added { fact, .. }
                    | FactStoreAddCommitV1::NearDuplicate { fact, .. }
                    | FactStoreAddCommitV1::PossibleConflict { fact, .. },
            } => fact,
        };
        let FactProjectionV1::Available { fact } = projection else {
            panic!("seed fact has no available payload: {}", seed.content)
        };
        let ids = index.entry(seed.source.clone()).or_default();
        ids.push(fact.fact_id.clone());

        if seed.preload_searches == 0 {
            continue;
        }
        let query = seed.preload_query.as_deref().unwrap_or_else(|| {
            panic!(
                "seed source `{}` sets preload_searches without a preload_query",
                seed.source
            )
        });
        for _ in 0..seed.preload_searches {
            let hits = run_search(fixture, query, 1);
            assert!(
                hits.iter().any(|hit| hit.fact.fact_id == fact.fact_id),
                "preload search `{query}` did not return the requested FactId {}",
                fact.fact_id
            );
        }
    }
    index
}

fn wait_for_memory_ready(fixture: &Fixture, expected_facts: usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last_count = 0;
    while Instant::now() < deadline {
        let status: MemoryStatusResultV1 =
            run_exact(fixture, "tracedecay_memory_status", json!({}));
        last_count = status.memory.fact_count;
        if last_count >= expected_facts as u64 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "fixture memory never settled through tracedecay_memory_status ({last_count}/{expected_facts} facts)"
    );
}

fn build_fixture(setup: &Setup) -> (Fixture, FactIndex) {
    let home = TempDir::new().expect("home tempdir");
    let project = TempDir::new().expect("project tempdir");
    let home_path = canonical_test_dir(home.path());
    let project_path = canonical_test_dir(project.path());
    let mut fixture = Fixture {
        _daemon: None,
        _home: home,
        home_path,
        _project: project,
        project_path,
    };
    let src = fixture.project_path.join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(src.join("lib.rs"), "pub fn eval_fixture_marker() {}\n").expect("write lib.rs");
    for (name, contents) in &setup.files {
        std::fs::write(fixture.project_path.join(name), contents)
            .unwrap_or_else(|error| panic!("write fixture file {name}: {error}"));
    }
    initialize_fixture_project(&fixture);
    fixture.start_daemon();
    let index = seed_setup_facts(&fixture, &setup.facts);
    wait_for_memory_ready(&fixture, setup.facts.len());
    (fixture, index)
}

fn resolve_fact_references(value: &mut Value, facts: &FactIndex) {
    match value {
        Value::String(reference) => {
            let Some(source) = reference.strip_prefix("$fact:") else {
                return;
            };
            let ids = facts
                .get(source)
                .unwrap_or_else(|| panic!("unknown FactId source reference `{source}`"));
            let [fact_id] = ids.as_slice() else {
                panic!("FactId source reference `{source}` is ambiguous");
            };
            *value = Value::from(fact_id.as_str());
        }
        Value::Array(values) => {
            for entry in values {
                resolve_fact_references(entry, facts);
            }
        }
        Value::Object(values) => {
            for entry in values.values_mut() {
                resolve_fact_references(entry, facts);
            }
        }
        _ => {}
    }
}

struct StepResult {
    succeeded: bool,
}

fn execute_step(
    fixture: &Fixture,
    step: &Step,
    facts: &FactIndex,
    curation_run: &mut Option<MemoryAutomationRunResultV1>,
) -> StepResult {
    match step {
        Step::Tool { tool, args } => {
            assert!(
                tool.starts_with("tracedecay_"),
                "evals must use an exact registered tool name: {tool}"
            );
            let mut args = args.clone();
            resolve_fact_references(&mut args, facts);
            let args = exact_args(args);
            let mut command = fixture.command();
            command.args(["tool", tool, "--args", &args.to_string()]);
            let output = run_with_timeout(command, Duration::from_secs(120));
            StepResult {
                succeeded: output.status.success(),
            }
        }
        Step::MemoryCuration => {
            let mut command = fixture.command();
            command.args(["automation", "run", "memory-curation"]);
            let output = run_with_timeout(command, Duration::from_secs(120));
            if output.status.success() {
                let run: MemoryAutomationRunResultV1 = serde_json::from_slice(&output.stdout)
                    .unwrap_or_else(|error| {
                        panic!("memory-curation terminal was not canonical JSON: {error}")
                    });
                assert_eq!(
                    run.task,
                    MemoryAutomationTaskV1::MemoryCurator,
                    "memory-curation command returned another automation task"
                );
                assert!(
                    run.matches_terminal(),
                    "memory-curation command returned an invalid terminal"
                );
                *curation_run = Some(run);
            }
            StepResult {
                succeeded: output.status.success(),
            }
        }
    }
}

fn compare_i64(op: CompareOp, actual: i64, expected: i64) -> bool {
    match op {
        CompareOp::Eq => actual == expected,
        CompareOp::Ne => actual != expected,
        CompareOp::Gt => actual > expected,
        CompareOp::Gte => actual >= expected,
        CompareOp::Lt => actual < expected,
        CompareOp::Lte => actual <= expected,
    }
}

fn compare_f64(op: CompareOp, actual: f64, expected: f64) -> bool {
    match op {
        CompareOp::Eq => actual == expected,
        CompareOp::Ne => actual != expected,
        CompareOp::Gt => actual > expected,
        CompareOp::Gte => actual >= expected,
        CompareOp::Lt => actual < expected,
        CompareOp::Lte => actual <= expected,
    }
}

fn millionths(value: u32) -> f64 {
    value as f64 / 1_000_000.0
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

fn current_facts(fixture: &Fixture) -> Vec<FactV1> {
    let listed: FactStoreListResultV1 =
        run_exact(fixture, "tracedecay_fact_store_list", json!({"limit": 200}));
    listed
        .facts
        .into_iter()
        .map(|projection| match projection {
            FactProjectionV1::Available { fact } => fact,
            other => panic!("fact-store list returned unavailable fact: {other:?}"),
        })
        .collect()
}

fn run_search(fixture: &Fixture, query: &str, limit: usize) -> Vec<FactSearchHitV1> {
    let searched: FactStoreSearchResultV1 = run_exact(
        fixture,
        "tracedecay_fact_store_search",
        json!({"query": query, "limit": limit}),
    );
    searched.hits
}

fn curation_removed_ids(run: &MemoryAutomationRunResultV1) -> HashSet<FactId> {
    let mut ids = HashSet::new();
    for receipt in &run.committed_receipts {
        if let MemoryAutomationCommittedReceiptV1::Curation(receipt) = receipt {
            for effect in &receipt.receipt.operation_effects {
                if let MemoryAutomationCurationOperationEffectV1::Remove {
                    target_fact_id,
                    disposition: MemoryAutomationCurationRemoveDispositionV1::Removed,
                    ..
                } = effect
                {
                    ids.insert(target_fact_id.clone());
                }
            }
        }
    }
    ids
}

fn source_fact_id(facts: &FactIndex, source: &str) -> FactId {
    let ids = facts
        .get(source)
        .unwrap_or_else(|| panic!("missing seeded source `{source}`"));
    let [fact_id] = ids.as_slice() else {
        panic!("seeded source `{source}` does not identify one FactId");
    };
    fact_id.clone()
}

fn format_search_results(results: &[FactSearchHitV1]) -> String {
    if results.is_empty() {
        return "<no results>".to_owned();
    }
    results
        .iter()
        .enumerate()
        .map(|(index, hit)| {
            format!(
                "#{} source=`{}` fact_id={} score={:.6} content=`{}` why={:?}",
                index + 1,
                hit.fact.source_label.as_deref().unwrap_or("<none>"),
                hit.fact.fact_id,
                millionths(hit.scores.score_millionths),
                hit.fact.content,
                hit.why
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn evaluate_assertions(
    scenario: &Scenario,
    fixture: &Fixture,
    phase: Phase,
    facts: &FactIndex,
    curation_run: &Option<MemoryAutomationRunResultV1>,
) -> Vec<AssertionOutcome> {
    let mut outcomes = Vec::new();
    for assertion in &scenario.assertions {
        match assertion {
            Assertion::FactCount {
                name,
                op,
                value,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let actual = current_facts(fixture).len() as i64;
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: compare_i64(*op, actual, *value),
                    detail: format!(
                        "FactRecordV1 count {actual}; expected {actual} {} {value}",
                        op_symbol(*op)
                    ),
                });
            }
            Assertion::SourceCount {
                name,
                source,
                op,
                value,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let actual = current_facts(fixture)
                    .iter()
                    .filter(|fact| fact.source_label.as_deref() == Some(source))
                    .count() as i64;
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: compare_i64(*op, actual, *value),
                    detail: format!(
                        "source `{source}` has {actual} facts; expected {actual} {} {value}",
                        op_symbol(*op)
                    ),
                });
            }
            Assertion::ContentCount {
                name,
                contains,
                op,
                value,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let actual = current_facts(fixture)
                    .iter()
                    .filter(|fact| fact.content.contains(contains))
                    .count() as i64;
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: compare_i64(*op, actual, *value),
                    detail: format!("content containing `{contains}` appears {actual} times; expected {actual} {} {value}", op_symbol(*op)),
                });
            }
            Assertion::SourceTrust {
                name,
                source,
                op,
                value,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let matching = current_facts(fixture)
                    .into_iter()
                    .filter(|fact| fact.source_label.as_deref() == Some(source))
                    .collect::<Vec<_>>();
                let passed = !matching.is_empty()
                    && matching.iter().all(|fact| {
                        compare_f64(*op, millionths(fact.trust_score_millionths), *value)
                    });
                let values = matching
                    .iter()
                    .map(|fact| format!("{:.6}", millionths(fact.trust_score_millionths)))
                    .collect::<Vec<_>>()
                    .join(", ");
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed,
                    detail: format!(
                        "source `{source}` trust scores [{values}]; expected each {} {value}",
                        op_symbol(*op)
                    ),
                });
            }
            Assertion::RetrievalTotal {
                name,
                source,
                op,
                value,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let actual = current_facts(fixture)
                    .iter()
                    .filter(|fact| fact.source_label.as_deref() == Some(source))
                    .map(|fact| i64::try_from(fact.telemetry.retrieval_count).unwrap())
                    .sum::<i64>();
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: compare_i64(*op, actual, *value),
                    detail: format!(
                        "source `{source}` retrieval total {actual}; expected {actual} {} {value}",
                        op_symbol(*op)
                    ),
                });
            }
            Assertion::FeedbackHistory {
                name,
                source,
                action,
                op,
                value,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let fact_id = source_fact_id(facts, source);
                let result: FactStoreGetResultV1 = run_exact(
                    fixture,
                    "tracedecay_fact_store_get",
                    json!({"fact_id": fact_id}),
                );
                let actual = result
                    .trust_history
                    .iter()
                    .filter(|entry| entry.action == *action)
                    .count() as i64;
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: compare_i64(*op, actual, *value),
                    detail: format!("source `{source}` has {actual} {action:?} feedback events; expected {actual} {} {value}", op_symbol(*op)),
                });
            }
            Assertion::CurationRemovesSource {
                name,
                source,
                expected,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let Some(run) = curation_run else {
                    panic!(
                        "[{}] assertion `{name}` needs a completed memory-curation run",
                        scenario.id
                    );
                };
                let removed_ids = curation_removed_ids(run);
                let source_ids = facts.get(source).cloned().unwrap_or_default();
                let any_removed = source_ids
                    .iter()
                    .any(|fact_id| removed_ids.contains(fact_id));
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed: any_removed == *expected,
                    detail: format!("curation remove operations touch source `{source}`: {any_removed} (expected {expected})"),
                });
            }
            Assertion::SearchRank {
                name,
                query,
                top_fact_source,
                min_rank_gap,
                limit,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let results = run_search(fixture, query, *limit);
                let expected_rank = results
                    .iter()
                    .position(|hit| hit.fact.source_label.as_deref() == Some(top_fact_source));
                let closest_other_rank = results
                    .iter()
                    .enumerate()
                    .find(|(_, hit)| hit.fact.source_label.as_deref() != Some(top_fact_source))
                    .map(|(index, _)| index);
                let rendered = format_search_results(&results);
                let (passed, detail) = match (expected_rank, closest_other_rank) {
                    (Some(target), Some(other)) => {
                        let gap = other as isize - target as isize;
                        (
                            gap >= *min_rank_gap as isize,
                            format!(
                                "query `{query}` expected `{top_fact_source}` rank={} nearest rival rank={} gap={gap} required>={min_rank_gap}; {rendered}",
                                target + 1,
                                other + 1,
                            ),
                        )
                    }
                    (Some(target), None) => (
                        false,
                        format!(
                            "query `{query}` returned only `{top_fact_source}` at rank {}; cannot prove a rank gap; {rendered}",
                            target + 1,
                        ),
                    ),
                    (None, _) => (
                        false,
                        format!("query `{query}` did not return `{top_fact_source}`; {rendered}"),
                    ),
                };
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed,
                    detail,
                });
            }
            Assertion::SearchSource {
                name,
                query,
                source,
                limit,
                phase: assertion_phase,
            } => {
                if should_skip_assertion(phase, *assertion_phase) {
                    continue;
                }
                let results = run_search(fixture, query, *limit);
                let passed = results
                    .iter()
                    .any(|hit| hit.fact.source_label.as_deref() == Some(source));
                outcomes.push(AssertionOutcome {
                    name: name.clone(),
                    passed,
                    detail: format!(
                        "query `{query}` must return source `{source}`; {}",
                        format_search_results(&results)
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
        .map(|outcome| {
            format!(
                "  [{}] {} — {}",
                if outcome.passed { "pass" } else { "FAIL" },
                outcome.name,
                outcome.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_scenario(id: &str, run_violation: bool) {
    let scenario = load_scenario(id);

    let (fixture, facts) = build_fixture(&scenario.setup);
    let mut curation_run = None;
    for step in &scenario.deterministic.well_behaved {
        let result = execute_step(&fixture, step, &facts, &mut curation_run);
        assert!(
            result.succeeded,
            "[{id}] well-behaved step was refused; compliant writes must be accepted"
        );
    }
    let outcomes = evaluate_assertions(
        &scenario,
        &fixture,
        Phase::WellBehaved,
        &facts,
        &curation_run,
    );
    assert!(
        outcomes.iter().all(|outcome| outcome.passed),
        "[{id}] well-behaved phase failed:\n{}",
        format_outcomes(&outcomes)
    );

    let Some(violation) = &scenario.deterministic.violation else {
        return;
    };
    if !run_violation {
        return;
    }
    let (fixture, facts) = build_fixture(&scenario.setup);
    let mut curation_run = None;
    let mut any_step_succeeded = false;
    for step in &violation.steps {
        let result = execute_step(&fixture, step, &facts, &mut curation_run);
        any_step_succeeded |= result.succeeded;
    }
    let outcomes =
        evaluate_assertions(&scenario, &fixture, Phase::Violation, &facts, &curation_run);
    let all_passed = outcomes.iter().all(|outcome| outcome.passed);
    match violation.expectation {
        Expectation::Detect => assert!(
            !all_passed,
            "[{id}] violation went undetected:\n{}",
            format_outcomes(&outcomes)
        ),
        Expectation::DefendOrDetect => {
            if all_passed {
                return;
            }
            assert!(
                any_step_succeeded && scenario.contract == ContractStatus::PendingSibling,
                "[{id}] accepted a stable-contract violation:\n{}",
                format_outcomes(&outcomes)
            );
        }
    }
}

#[test]
fn eval_memory_no_pollution() {
    run_scenario("memory-no-pollution", true);
}

#[test]
fn eval_memory_secret_rejection() {
    run_scenario("memory-secret-rejection", true);
}

#[test]
fn eval_memory_skip_local() {
    run_scenario("memory-skip-local", true);
}

#[test]
fn eval_memory_supersede_without_dup() {
    run_scenario("memory-supersede-without-dup", false);
}

#[test]
#[ignore = "requires a configured automation backend"]
fn eval_memory_supersede_without_dup_curation() {
    run_scenario("memory-supersede-without-dup", true);
}

#[test]
fn eval_memory_multiturn_continuity() {
    run_scenario("memory-multiturn-continuity", true);
}

#[test]
#[ignore = "requires a configured automation backend"]
fn eval_memory_curation_conservatism() {
    run_scenario("memory-curation-conservatism", true);
}

#[test]
fn eval_memory_ranking_trust_bias() {
    run_scenario("memory-ranking-trust-bias", true);
}

#[test]
fn eval_memory_ranking_supersession() {
    run_scenario("memory-ranking-supersession", true);
}

#[test]
fn eval_memory_ranking_morphology() {
    run_scenario("memory-ranking-morphology", true);
}

#[test]
fn eval_memory_feedback_trust() {
    run_scenario("memory-feedback-trust", true);
}

#[test]
fn eval_memory_ranking_retrieval_reinforcement() {
    run_scenario("memory-ranking-retrieval-reinforcement", true);
}

#[test]
fn eval_memory_ranking_feedback_promotes() {
    run_scenario("memory-ranking-feedback-promotes", true);
}

/// Every scenario must have a test so unwired JSON cannot silently escape production coverage.
#[test]
fn every_scenario_file_is_wired() {
    let wired: HashSet<&str> = [
        "memory-no-pollution",
        "memory-secret-rejection",
        "memory-skip-local",
        "memory-supersede-without-dup",
        "memory-multiturn-continuity",
        "memory-curation-conservatism",
        "memory-ranking-trust-bias",
        "memory-ranking-supersession",
        "memory-ranking-morphology",
        "memory-feedback-trust",
        "memory-ranking-retrieval-reinforcement",
        "memory-ranking-feedback-promotes",
    ]
    .into_iter()
    .collect();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/scenarios");
    let found = std::fs::read_dir(&directory)
        .expect("read eval/scenarios")
        .map(|entry| entry.expect("scenario entry").path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .map(|path| {
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("scenario file stem")
                .to_owned();
            load_scenario(&id);
            id
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        found.iter().map(String::as_str).collect::<HashSet<_>>(),
        wired,
        "eval/scenarios/*.json and the test list must stay in sync"
    );
}
