//! Write cost of the production Codex history pass over a bounded corpus:
//! the first import, an idle re-pass, and the pass a restarted daemon runs
//! against the same store. This probe owns its process so writer telemetry
//! includes only its own work.

// The shared support module reads WAL frames through Unix file APIs.
#![cfg(unix)]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use support::DrainFixture;
use tracedecay_contracts::ProfileIdentityReadPort;
use tracedecay_domain::{BrainId, ObservationScopeV1, ObservationSourceCursorV1, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::tests::harness::writer_telemetry;
use tracedecay_private_fs::RewriteWitness;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::resident_memory::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::ProjectSessionHistoricalIngestor;
use tracedecay_session_runtime::session_temporal_refresh_scheduler::history::{
    SessionHistoricalIngestOutcome, SessionHistoricalIngestor,
};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub;

const DAYS: usize = 6;
const ROLLOUTS_PER_DAY: usize = 4;
const MAX_PASSES: usize = 64;
/// The rollout count of the corpus the re-offer cost was first measured on.
const MEASURED_ROLLOUTS: usize = 2_010;
/// The control corpus: the same layout and project at a size where per-file
/// store work would be two orders of magnitude smaller.
const CONTROL_ROLLOUTS: usize = 12;
const MAX_MEASURED_PASSES: usize = 4_096;

struct FixtureIdentity {
    profile_root: PathBuf,
    brain_id: BrainId,
    profile_id: UserProfileId,
}

impl ProfileIdentityReadPort for FixtureIdentity {
    fn profile_root(&self) -> &Path {
        &self.profile_root
    }
    fn brain_id(&self) -> &BrainId {
        &self.brain_id
    }
    fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }
}

fn rollout_path(home: &Path, day: usize, index: usize) -> PathBuf {
    let session = format!("019a0000-0000-7000-8000-{day:04}{index:08}");
    home.join(format!(".codex/sessions/2026/01/{:02}", day + 1))
        .join(format!(
            "rollout-2026-01-{:02}T00-00-00-{session}.jsonl",
            day + 1
        ))
}

/// A rollout whose mtime is a day old, as on a daemon re-importing history
/// after a store reset. Its change time is still now; see
/// [`settle_codex_tree`].
fn write_rollout(home: &Path, project: &Path, day: usize, index: usize) {
    let dir = home.join(format!(".codex/sessions/2026/01/{:02}", day + 1));
    std::fs::create_dir_all(&dir).unwrap();
    let session = format!("019a0000-0000-7000-8000-{day:04}{index:08}");
    let at = |second: usize| format!("2026-01-{:02}T00:00:{second:02}.000Z", day + 1);
    let records = [
        json!({"timestamp": at(0), "type": "session_meta",
               "payload": {"id": session, "cwd": project, "model_provider": "openai"}}),
        json!({"timestamp": at(1), "type": "event_msg",
               "payload": {"type": "user_message", "message": format!("prompt {day}/{index}")}}),
        json!({"timestamp": at(2), "type": "event_msg",
               "payload": {"type": "agent_message", "message": format!("reply {day}/{index}")}}),
    ];
    let body: String = records.iter().map(|record| format!("{record}\n")).collect();
    let path = rollout_path(home, day, index);
    std::fs::write(&path, body).unwrap();
    let settled = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(settled)
        .unwrap();
}

/// A process-wide discovery hub with the preparation resources the daemon
/// mounts at bootstrap.
fn mounted_hub(cpu: &Arc<ProcessBackgroundCpuV1>) -> Arc<CodexDiscoveryHub> {
    let hub = Arc::new(CodexDiscoveryHub::default());
    hub.configure_preparation_resources(
        Arc::new(ProcessResidentMemoryV1::new(
            DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        )),
        Arc::clone(cpu),
    )
    .unwrap();
    hub
}

fn ingestor(
    fixture: &DrainFixture,
    database: &RegisteredGlobalDbLeaseV1,
    home: &Path,
    hub: &Arc<CodexDiscoveryHub>,
    cpu: &Arc<ProcessBackgroundCpuV1>,
) -> ProjectSessionHistoricalIngestor {
    let shard = &database.binding().shard_id;
    ProjectSessionHistoricalIngestor::new(
        database.clone(),
        Arc::new(FixtureIdentity {
            profile_root: fixture._tmp.path().join("profile"),
            brain_id: shard.brain_id.clone(),
            profile_id: shard.profile_id.clone(),
        }),
        fixture.project.clone(),
        fixture.project_id.clone(),
        ProfileRoot::under_home(home),
        Arc::clone(hub),
        Arc::clone(cpu),
    )
}

/// Days alternate between this project and another checkout, so the
/// newest-first sweep yields at each out-of-scope day after it persisted
/// in-scope frames and the import spans several passes.
fn write_corpus(fixture: &DrainFixture, home: &Path) -> usize {
    let elsewhere = fixture._tmp.path().join("other-checkout");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut in_scope = 0;
    for day in 0..DAYS {
        let cwd = if day % 2 == 0 {
            in_scope += ROLLOUTS_PER_DAY;
            fixture.project.as_path()
        } else {
            elsewhere.as_path()
        };
        for index in 0..ROLLOUTS_PER_DAY {
            write_rollout(home, cwd, day, index);
        }
    }
    settle_codex_tree(home);
    in_scope
}

/// Waits until the production discovery witness vouches for every entry of
/// the Codex tree. A file or directory identity read inside its change-time
/// quantum is unvouched and matches no later stat, so the next sweep must
/// re-verify it; a corpus written just before its import is in that state
/// wherever the clock that stamps change times is coarse. Settling first
/// makes every sweep observe the same corpus state on every filesystem.
fn settle_codex_tree(home: &Path) {
    fn settled(path: &Path) -> bool {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        RewriteWitness::NATIVE.vouches_for_unchanged_bytes(&metadata)
            && (!metadata.is_dir()
                || std::fs::read_dir(path)
                    .unwrap()
                    .all(|entry| settled(&entry.unwrap().path())))
    }
    let root = home.join(".codex");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !settled(&root) {
        assert!(
            Instant::now() < deadline,
            "the Codex tree never settled under {}",
            root.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A corpus of settled rollouts that all belong to this one project, so
/// after the import the next sweep finds every one unchanged.
fn write_single_project_corpus(fixture: &DrainFixture, home: &Path, rollouts: usize) {
    for index in 0..rollouts {
        write_rollout(home, &fixture.project, index % DAYS, index);
    }
    settle_codex_tree(home);
}

#[derive(Debug, Default, Clone, Copy)]
struct Cost {
    passes: usize,
    transactions: u64,
    vm_steps: u64,
    wall: Duration,
}

/// Runs passes until history completes, returning the writer work they did.
async fn run_to_completion(
    ingestor: &ProjectSessionHistoricalIngestor,
    database: &RegisteredGlobalDbLeaseV1,
) -> Cost {
    run_to_completion_bounded(ingestor, database, MAX_PASSES).await
}

async fn run_to_completion_bounded(
    ingestor: &ProjectSessionHistoricalIngestor,
    database: &RegisteredGlobalDbLeaseV1,
    max_passes: usize,
) -> Cost {
    let before = writer_telemetry(database);
    let started = Instant::now();
    let mut passes = 0;
    let mut previous_transactions = before.transactions.committed_transactions;
    let mut previous_vm_steps = before.sqlite_vm.vm_steps;
    let mut pass_costs = Vec::new();
    loop {
        passes += 1;
        let pass_started = Instant::now();
        let pass = ingestor.run_pass().await;
        let pass_wall = pass_started.elapsed();
        let after_pass = writer_telemetry(database);
        pass_costs.push((
            pass.clone(),
            after_pass.transactions.committed_transactions - previous_transactions,
            after_pass.sqlite_vm.vm_steps - previous_vm_steps,
            pass_wall,
        ));
        previous_transactions = after_pass.transactions.committed_transactions;
        previous_vm_steps = after_pass.sqlite_vm.vm_steps;
        assert!(
            passes <= max_passes,
            "history never completed; last pass {:?} units {:?}",
            pass.outcome,
            pass.provider_units
        );
        match pass.outcome {
            SessionHistoricalIngestOutcome::Complete => break,
            SessionHistoricalIngestOutcome::Pending { .. }
            | SessionHistoricalIngestOutcome::Retryable { .. } => {}
            outcome => panic!("history pass failed: {outcome:?}"),
        }
    }
    let wall = started.elapsed();
    let after = writer_telemetry(database);
    let cost = Cost {
        passes,
        transactions: after.transactions.committed_transactions
            - before.transactions.committed_transactions,
        vm_steps: after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps,
        wall,
    };
    // Report after measuring so diagnostic output does not inflate sweep time.
    for (index, (pass, transactions, vm_steps, wall)) in pass_costs.iter().enumerate() {
        eprintln!(
            "codex history pass {}/{}: outcome={:?} made_progress={} units={:?} \
             transactions={transactions} vm_steps={vm_steps} wall={wall:?}",
            index + 1,
            cost.passes,
            pass.outcome,
            pass.made_progress,
            pass.provider_units,
        );
    }
    cost
}

async fn open() -> (
    DrainFixture,
    RegisteredGlobalDbLeaseV1,
    PathBuf,
    Arc<ProcessBackgroundCpuV1>,
) {
    let fixture = DrainFixture::open().await;
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let home = fixture._tmp.path().join("home");
    let cpu = Arc::new(ProcessBackgroundCpuV1::new(
        std::num::NonZeroUsize::new(4).unwrap(),
    ));
    (fixture, database, home, cpu)
}

/// After a completed sweep, neither an idle re-pass nor a restarted daemon
/// commits anything: the restart resumes from durable coverage (#4756).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_completed_codex_import_stays_idle_across_restart() {
    let (fixture, database, home, cpu) = open().await;
    let rollouts = write_corpus(&fixture, &home);

    let first = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    let import = run_to_completion(&first, &database).await;
    let idle = run_to_completion(&first, &database).await;
    first.cancel();
    drop(first);

    // A restarted daemon: a fresh process-wide hub over the same store.
    let restarted = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    let restart = run_to_completion(&restarted, &database).await;
    restarted.cancel();

    eprintln!(
        "codex history cost over {rollouts} in-scope rollouts: import={import:?} \
         idle={idle:?} restart={restart:?}"
    );
    assert!(import.passes > 1, "the corpus must span several passes");
    assert!(import.transactions > 0, "the import commits its rollouts");
    assert_eq!(idle.transactions, 0, "an idle re-pass commits nothing");
    assert!(
        idle.vm_steps.saturating_mul(20) < import.vm_steps,
        "an idle re-pass must not redo import-scale reads: {idle:?} vs {import:?}"
    );
    assert_eq!(
        restart.transactions, 0,
        "a restart must resume from durable coverage, not re-import"
    );
    assert_eq!(restart.passes, 1, "a settled restart needs one pass");
}

/// A daemon restarted between the passes of an import finishes it with no
/// more writer work than an uninterrupted import: committed rollouts are not
/// re-admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_mid_import_resumes_without_rewriting_committed_rollouts() {
    let (baseline_fixture, baseline_database, baseline_home, cpu) = open().await;
    write_corpus(&baseline_fixture, &baseline_home);
    let baseline = ingestor(
        &baseline_fixture,
        &baseline_database,
        &baseline_home,
        &mounted_hub(&cpu),
        &cpu,
    );
    let uninterrupted = run_to_completion(&baseline, &baseline_database).await;
    baseline.cancel();

    let (fixture, database, home, _) = open().await;
    write_corpus(&fixture, &home);
    let before = writer_telemetry(&database)
        .transactions
        .committed_transactions;
    let first = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    let pass = first.run_pass().await;
    assert!(
        matches!(
            pass.outcome,
            SessionHistoricalIngestOutcome::Pending {
                made_progress: true
            } | SessionHistoricalIngestOutcome::Retryable {
                made_progress: true,
                ..
            }
        ),
        "the first pass must commit part of the import: {:?}",
        pass.outcome
    );
    first.cancel();
    drop(first);
    let restarted = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    let resumed = run_to_completion(&restarted, &database).await;
    restarted.cancel();
    let interrupted = writer_telemetry(&database)
        .transactions
        .committed_transactions
        - before;

    eprintln!(
        "codex import across a restart: uninterrupted={uninterrupted:?} \
         interrupted_transactions={interrupted} resumed={resumed:?}"
    );
    assert!(
        interrupted <= uninterrupted.transactions,
        "a restart re-committed work: {interrupted} > {}",
        uninterrupted.transactions
    );
}

/// An unbatched replay paid five transactions per rollout. This ceiling
/// protects batching; durable observations and cursors separately prove
/// that the measured import covered the whole corpus.
const IMPORT_TRANSACTIONS_PER_ROLLOUT_CEILING: u64 = 4;

fn assert_corpus_is_admitted(fixture: &DrainFixture, home: &Path, rollouts: usize, appended: bool) {
    let connection = rusqlite::Connection::open_with_flags(
        support::session_store_path(fixture),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let expected: BTreeMap<String, i64> = (0..rollouts)
        .map(|index| {
            let day = index % DAYS;
            (
                format!("019a0000-0000-7000-8000-{day:04}{index:08}"),
                3 + i64::from(appended && index == 0),
            )
        })
        .collect();
    let actual: BTreeMap<String, i64> = connection
        .prepare(
            "SELECT json_extract(observation_json, '$.identity.source.session_id'), COUNT(*)
             FROM observations
             GROUP BY json_extract(observation_json, '$.identity.source.session_id')",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        actual, expected,
        "every rollout must retain each generated frame exactly once"
    );

    let cursors: Vec<ObservationSourceCursorV1> = connection
        .prepare("SELECT cursor_json FROM source_cursors")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect();
    assert_eq!(cursors.len(), rollouts);
    let mut positions = BTreeMap::new();
    for cursor in cursors {
        assert_eq!(cursor.source().provider().as_str(), "codex");
        assert_eq!(
            cursor.scope(),
            &ObservationScopeV1::Project {
                project_id: fixture.project_id.clone()
            }
        );
        assert!(
            positions
                .insert(
                    cursor.source().session_id().as_str().to_owned(),
                    cursor.position()
                )
                .is_none()
        );
    }
    for index in 0..rollouts {
        let day = index % DAYS;
        let session = format!("019a0000-0000-7000-8000-{day:04}{index:08}");
        assert_eq!(
            positions.get(&session).copied(),
            Some(
                std::fs::metadata(rollout_path(home, day, index))
                    .unwrap()
                    .len()
            ),
            "the durable cursor must cover all bytes of {session}"
        );
    }
}

/// Writer work of each transition the re-offer acceptance compares, all over
/// one settled single-project corpus and one ingestor.
#[derive(Debug)]
struct SweepTransitions {
    /// The import that admits every rollout.
    import: Cost,
    /// The first sweep after the import, over the unchanged corpus.
    first_idle: Cost,
    /// A later sweep, after one that already found the corpus unchanged.
    later_idle: Cost,
    /// The sweep after one record is appended to one rollout.
    appended: Cost,
}

async fn sweep_transitions(rollouts: usize) -> SweepTransitions {
    let (fixture, database, home, cpu) = open().await;
    write_single_project_corpus(&fixture, &home, rollouts);
    let ingestor = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    let import = run_to_completion_bounded(&ingestor, &database, MAX_MEASURED_PASSES).await;
    assert_corpus_is_admitted(&fixture, &home, rollouts, false);
    let first_idle = run_to_completion_bounded(&ingestor, &database, MAX_MEASURED_PASSES).await;
    assert_corpus_is_admitted(&fixture, &home, rollouts, false);
    let later_idle = run_to_completion_bounded(&ingestor, &database, MAX_MEASURED_PASSES).await;
    assert_corpus_is_admitted(&fixture, &home, rollouts, false);

    let appended = rollout_path(&home, 0, 0);
    assert!(appended.is_file(), "the appended rollout must exist");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&appended)
        .unwrap();
    writeln!(
        file,
        "{}",
        json!({"timestamp": "2026-01-01T00:00:03.000Z", "type": "event_msg",
               "payload": {"type": "user_message", "message": "appended prompt"}})
    )
    .unwrap();
    drop(file);
    settle_codex_tree(&home);
    let appended = run_to_completion_bounded(&ingestor, &database, MAX_MEASURED_PASSES).await;
    assert_corpus_is_admitted(&fixture, &home, rollouts, true);
    ingestor.cancel();

    let transitions = SweepTransitions {
        import,
        first_idle,
        later_idle,
        appended,
    };
    eprintln!("codex sweep transitions over {rollouts} settled rollouts: {transitions:?}");
    transitions
}

/// The re-offer acceptance measurement for #4374: a full second sweep over
/// 2,010 settled rollouts in one project commits nothing and does no
/// per-file store work, where the import that first admitted them paid a
/// durable write for every rollout.
///
/// Store work is compared transition by transition against a 12-rollout
/// control built by the same code: equal writer work across a 167x size
/// difference is what "no per-file work" means, whatever the fixed per-pass
/// cost is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_sweep_over_the_measured_corpus_commits_nothing() {
    let measured = sweep_transitions(MEASURED_ROLLOUTS).await;
    let control = sweep_transitions(CONTROL_ROLLOUTS).await;

    // Exact observation and cursor coverage above makes this a cost check
    // over the whole corpus at both sizes.
    for (transitions, rollouts) in [(&measured, MEASURED_ROLLOUTS), (&control, CONTROL_ROLLOUTS)] {
        assert!(
            transitions.import.transactions
                <= IMPORT_TRANSACTIONS_PER_ROLLOUT_CEILING * rollouts as u64,
            "projection replay must stay batched: {} transactions for {rollouts} rollouts",
            transitions.import.transactions
        );
    }

    for (label, large, small) in [
        ("first", measured.first_idle, control.first_idle),
        ("later", measured.later_idle, control.later_idle),
    ] {
        assert_eq!(
            (large.transactions, small.transactions),
            (0, 0),
            "a {label} sweep over unchanged rollouts must commit nothing: \
             large={large:?} small={small:?}"
        );
        assert_eq!(
            (large.passes, small.passes),
            (1, 1),
            "a {label} sweep over settled unchanged rollouts needs one pass: \
             large={large:?} small={small:?}"
        );
        assert_eq!(
            large.vm_steps, small.vm_steps,
            "a {label} unchanged sweep must do only fixed per-pass store work: \
             large={large:?} small={small:?}"
        );
    }
    let reoffer = measured.first_idle;
    let import = measured.import;
    assert!(
        reoffer.vm_steps.saturating_mul(10) < import.vm_steps,
        "the second sweep must do far less store work than the import: \
         {reoffer:?} vs {import:?}"
    );
    assert!(
        reoffer.wall < import.wall,
        "the second sweep must take less wall time than the import: \
         {reoffer:?} vs {import:?}"
    );

    // The empty sweep is a live verdict, not a dead probe: one appended
    // rollout is discovered and committed on the next sweep, and only it, so
    // its writer work does not depend on how many rollouts sit beside it.
    let (large, small) = (measured.appended, control.appended);
    assert!(
        large.transactions > 0,
        "the appended frames must be committed: {large:?}"
    );
    assert!(
        large.transactions < measured.import.transactions,
        "only the appended rollout may be re-admitted: {large:?} vs {:?}",
        measured.import
    );
    assert_eq!(
        (large.transactions, large.vm_steps),
        (small.transactions, small.vm_steps),
        "only the appended rollout may be re-admitted: large={large:?} small={small:?}"
    );
}
