//! Write cost of the production Codex history pass over a bounded corpus:
//! the first import, an idle re-pass, and the pass a restarted daemon runs
//! against the same store. This probe owns its process so writer telemetry
//! includes only its own work.

// The shared support module reads WAL frames through Unix file APIs.
#![cfg(unix)]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use support::DrainFixture;
use tracedecay_contracts::ProfileIdentityReadPort;
use tracedecay_domain::{BrainId, UserProfileId};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::tests::harness::writer_telemetry;
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

/// A settled rollout: its mtime is a day old, as on a daemon re-importing
/// history after a store reset.
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
    in_scope
}

/// The measured corpus: every rollout belongs to this one project and is
/// settled, so after the import the next sweep finds every one unchanged.
fn write_measured_corpus(fixture: &DrainFixture, home: &Path) -> usize {
    for index in 0..MEASURED_ROLLOUTS {
        write_rollout(home, &fixture.project, index % DAYS, index);
    }
    MEASURED_ROLLOUTS
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
    loop {
        passes += 1;
        let pass = ingestor.run_pass().await;
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
    Cost {
        passes,
        transactions: after.transactions.committed_transactions
            - before.transactions.committed_transactions,
        vm_steps: after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps,
        wall,
    }
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

/// The store work of one empty sweep over a settled corpus, whatever its
/// size: the fixed per-pass cost the daemon pays on every idle poll.
async fn empty_sweep_vm_steps() -> u64 {
    let (fixture, database, home, cpu) = open().await;
    write_corpus(&fixture, &home);
    let ingestor = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    run_to_completion(&ingestor, &database).await;
    let idle = run_to_completion(&ingestor, &database).await;
    ingestor.cancel();
    assert_eq!(idle.transactions, 0, "an idle re-pass commits nothing");
    idle.vm_steps
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

/// Committed transactions the import pays per settled rollout it admits.
/// The product with the corpus size proves every rollout was admitted once:
/// a skipped rollout falls short and a re-admitted one overshoots.
const IMPORT_TRANSACTIONS_PER_ROLLOUT: u64 = 5;

/// The re-offer acceptance measurement for #4374: a full second sweep over
/// 2,010 settled rollouts in one project commits nothing and does no
/// per-file store work, where the import that first admitted them paid a
/// durable write for every rollout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_sweep_over_the_measured_corpus_commits_nothing() {
    let (fixture, database, home, cpu) = open().await;
    let rollouts = write_measured_corpus(&fixture, &home);

    let first = ingestor(&fixture, &database, &home, &mounted_hub(&cpu), &cpu);
    let import = run_to_completion_bounded(&first, &database, MAX_MEASURED_PASSES).await;
    let reoffer = run_to_completion_bounded(&first, &database, MAX_MEASURED_PASSES).await;

    eprintln!(
        "codex re-offer cost over {rollouts} settled rollouts in one project: \
         import={import:?} reoffer={reoffer:?}"
    );
    // Every rollout was discovered and admitted: the import's durable writes
    // are exactly the per-rollout cost times the corpus.
    assert_eq!(
        import.transactions,
        IMPORT_TRANSACTIONS_PER_ROLLOUT * rollouts as u64,
        "the import must admit every rollout exactly once"
    );
    assert_eq!(
        reoffer.transactions, 0,
        "a second sweep over unchanged rollouts must commit nothing"
    );
    // The second sweep's store work is the fixed per-pass cost, not per file:
    // it equals one empty sweep over the 12-rollout corpus, whatever that
    // costs. A per-file cost would scale with the 2,010 rollouts.
    let small = empty_sweep_vm_steps().await;
    assert_eq!(
        reoffer.vm_steps, small,
        "the second sweep must do only the fixed per-pass store work"
    );
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
    // rollout is discovered and admitted on the next sweep, and only it.
    let appended = rollout_path(&home, 0, 0);
    assert!(appended.is_file(), "the appended rollout must exist");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&appended)
        .unwrap();
    use std::io::Write as _;
    writeln!(
        file,
        "{}",
        json!({"timestamp": "2026-01-01T00:00:03.000Z", "type": "event_msg",
               "payload": {"type": "user_message", "message": "appended prompt"}})
    )
    .unwrap();
    drop(file);
    let live = run_to_completion_bounded(&first, &database, MAX_MEASURED_PASSES).await;
    first.cancel();
    eprintln!("codex sweep after one appended rollout: live={live:?}");
    assert!(
        live.transactions > 0,
        "the appended frames must be committed: {live:?}"
    );
    assert!(
        live.transactions < import.transactions,
        "only the appended rollout may be re-admitted: {live:?} vs {import:?}"
    );
}
