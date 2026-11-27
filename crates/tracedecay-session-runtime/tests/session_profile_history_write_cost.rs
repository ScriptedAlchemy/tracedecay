//! Per-transaction write cost of the production profile history import
//! (`user-sessions.db`, the `profile_sessions` shard) as the store grows to
//! incident scale. #4756 reports 125-160 ms per write on a 1.3 GB store and a
//! cost that rose with the store's size; this probe measures whether the
//! import's own writes depend on how much is already stored.
//!
//! It drives the production `ProfileSessionHistoricalIngestor` over a
//! generated Codex corpus and samples the writer's telemetry after every pass:
//! committed transactions, the time they held the writer, the SQLite work they
//! did, and the store's size on disk. This probe owns its process so the
//! telemetry includes only its own work.
//!
//! The corpus is 16 rollouts per generated day at roughly 3.6 MB of store per
//! rollout. The default four days land a few hundred MB; an incident-scale run
//! is `TRACEDECAY_PROFILE_COST_DAYS=20 bazel test //crates/tracedecay-session-runtime:session_profile_history_write_cost --test_output=all --test_arg=--nocapture --test_timeout=3600`,
//! which grows the store past 1 GB.
//!
//! The profile store owns the sessions no registered project claims: a rollout
//! whose `cwd` falls under a registered root belongs to that project's store.
//! So the corpus runs in an unregistered directory beside a registered one,
//! which is the shape of the incident's `profile_sessions` writes.

// The shared support module reads WAL frames through Unix file APIs.
#![cfg(unix)]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

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
use tracedecay_session_runtime::session_temporal_refresh_scheduler::history::{
    ProfileSessionHistoricalIngestor, SessionHistoricalIngestOutcome, SessionHistoricalIngestor,
};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub;

/// One pass admits at most one 16 MiB Codex unit, so a 1 GB import needs
/// dozens of passes; bound the loop well above that.
const MAX_PASSES: usize = 4_096;
/// Rollouts per generated day directory.
const ROLLOUTS_PER_DAY: usize = 16;
/// Messages per rollout: enough for several capture windows per file.
const MESSAGES_PER_ROLLOUT: usize = 96;
/// Bytes of prose per message. Real rollouts carry kilobytes of text per
/// record; this keeps the generated rows in that shape.
const MESSAGE_WORDS: usize = 400;

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

/// A settled rollout whose records have the shape production admits:
/// `session_meta`, then alternating user and assistant `response_item`s and
/// `event_msg`s with kilobyte-scale text. Its mtime is a day old, as on a
/// daemon re-importing history after a store reset.
fn write_rollout(home: &Path, cwd: &Path, day: usize, index: usize) {
    let dir = home.join(format!(".codex/sessions/2026/01/{:02}", day + 1));
    std::fs::create_dir_all(&dir).unwrap();
    let session = format!("019a0000-0000-7000-8000-{day:04}{index:08}");
    let at = |second: usize| {
        format!(
            "2026-01-{:02}T00:{:02}:{:02}.000Z",
            day + 1,
            second / 60,
            second % 60
        )
    };
    let prose = "context ".repeat(MESSAGE_WORDS);
    let mut body = String::new();
    body.push_str(
        &json!({"timestamp": at(0), "type": "session_meta",
                "payload": {"id": session, "cwd": cwd, "model_provider": "openai"}})
        .to_string(),
    );
    body.push('\n');
    for message in 0..MESSAGES_PER_ROLLOUT {
        let second = message + 1;
        let (role, kind, text) = if message % 2 == 0 {
            (
                "user",
                "user_message",
                format!("prompt {day}/{index}/{message} {prose}"),
            )
        } else {
            (
                "assistant",
                "agent_message",
                format!("reply {day}/{index}/{message} {prose}"),
            )
        };
        body.push_str(
            &json!({"timestamp": at(second), "type": "response_item",
                    "payload": {"type": "message", "role": role,
                                "content": [{"type": "input_text", "text": text}]}})
            .to_string(),
        );
        body.push('\n');
        body.push_str(
            &json!({"timestamp": at(second), "type": "event_msg",
                    "payload": {"type": kind, "message": text}})
            .to_string(),
        );
        body.push('\n');
    }
    let path = dir.join(format!(
        "rollout-2026-01-{:02}T00-00-00-{session}.jsonl",
        day + 1
    ));
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
    registry: &RegisteredGlobalDbLeaseV1,
    home: &Path,
    hub: &Arc<CodexDiscoveryHub>,
    cpu: &Arc<ProcessBackgroundCpuV1>,
) -> ProfileSessionHistoricalIngestor {
    let shard = &database.binding().shard_id;
    ProfileSessionHistoricalIngestor::new(
        database.clone(),
        registry.clone(),
        Arc::new(FixtureIdentity {
            profile_root: fixture._tmp.path().join("profile"),
            brain_id: shard.brain_id.clone(),
            profile_id: shard.profile_id.clone(),
        }),
        ProfileRoot::under_home(home),
        Arc::clone(hub),
        Arc::clone(cpu),
    )
}

/// The writer's cumulative work on the profile store.
#[derive(Clone, Copy, Debug, Default)]
struct WriterWork {
    committed: u64,
    rolled_back: u64,
    transaction_micros: u64,
    lock_held_micros: u64,
    commands: u64,
    rows: u64,
    statements: u64,
    fullscan_steps: u64,
    sort_steps: u64,
    vm_steps: u64,
}

impl WriterWork {
    fn read(database: &RegisteredGlobalDbLeaseV1) -> Self {
        let writer = writer_telemetry(database);
        Self {
            committed: writer.transactions.committed_transactions,
            rolled_back: writer.transactions.rolled_back_transactions,
            transaction_micros: writer.transactions.transaction_micros,
            lock_held_micros: writer.transactions.lock_held_micros,
            commands: writer.transactions.commands,
            rows: writer.transactions.rows,
            statements: writer.sqlite_vm.statements,
            fullscan_steps: writer.sqlite_vm.fullscan_steps,
            sort_steps: writer.sqlite_vm.sort_steps,
            vm_steps: writer.sqlite_vm.vm_steps,
        }
    }

    fn since(self, earlier: Self) -> Self {
        Self {
            committed: self.committed - earlier.committed,
            rolled_back: self.rolled_back - earlier.rolled_back,
            transaction_micros: self.transaction_micros - earlier.transaction_micros,
            lock_held_micros: self.lock_held_micros - earlier.lock_held_micros,
            commands: self.commands - earlier.commands,
            rows: self.rows - earlier.rows,
            statements: self.statements - earlier.statements,
            fullscan_steps: self.fullscan_steps - earlier.fullscan_steps,
            sort_steps: self.sort_steps - earlier.sort_steps,
            vm_steps: self.vm_steps - earlier.vm_steps,
        }
    }
}

/// One pass's writer work beside the store size it ran against.
#[derive(Clone, Copy, Debug)]
struct PassSample {
    pass: usize,
    store_bytes: u64,
    work: WriterWork,
}

fn store_bytes(path: &Path) -> u64 {
    let size = |suffix: &str| {
        std::fs::metadata(path.with_extension(suffix))
            .map(|meta| meta.len())
            .unwrap_or(0)
    };
    size("db") + size("db-wal")
}

/// A per-transaction average over the committed transactions in `work`;
/// zero when nothing committed, so an idle pass reports no cost rather than
/// dividing by zero.
fn per_transaction(total: u64, work: &WriterWork) -> u64 {
    total.checked_div(work.committed).unwrap_or(0)
}

fn micros_per_transaction(work: &WriterWork) -> u64 {
    per_transaction(work.transaction_micros, work)
}

/// Runs passes until history completes, sampling the writer after each.
async fn run_to_completion(
    ingestor: &ProfileSessionHistoricalIngestor,
    database: &RegisteredGlobalDbLeaseV1,
    store: &Path,
) -> (Vec<PassSample>, WriterWork) {
    let start = WriterWork::read(database);
    let mut previous = start;
    let mut samples = Vec::new();
    let mut passes = 0;
    loop {
        passes += 1;
        let pass = ingestor.run_pass().await;
        assert!(
            passes <= MAX_PASSES,
            "history never completed; last pass {:?} units {:?}",
            pass.outcome,
            pass.provider_units
        );
        let now = WriterWork::read(database);
        eprintln!(
            "profile history pass {passes}: outcome={:?} progress={} units={:?}",
            pass.outcome, pass.made_progress, pass.provider_units
        );
        samples.push(PassSample {
            pass: passes,
            store_bytes: store_bytes(store),
            work: now.since(previous),
        });
        previous = now;
        match pass.outcome {
            SessionHistoricalIngestOutcome::Complete => break,
            SessionHistoricalIngestOutcome::Pending { .. }
            | SessionHistoricalIngestOutcome::Retryable { .. } => {}
            outcome => panic!("history pass failed: {outcome:?}"),
        }
    }
    (samples, WriterWork::read(database).since(start))
}

async fn open() -> (
    DrainFixture,
    RegisteredGlobalDbLeaseV1,
    RegisteredGlobalDbLeaseV1,
    PathBuf,
    Arc<ProcessBackgroundCpuV1>,
) {
    let fixture = DrainFixture::open().await;
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Profile)
        .unwrap();
    let registry = fixture.runtime.profile_registry_lease();
    let home = fixture._tmp.path().join("home");
    let cpu = Arc::new(ProcessBackgroundCpuV1::new(
        std::num::NonZeroUsize::new(4).unwrap(),
    ));
    (fixture, database, registry, home, cpu)
}

/// The profile sweep admits a rollout only when its `cwd` is under a
/// registered project root, so the fixture's project must be in the registry.
async fn register_project_root(registry: &RegisteredGlobalDbLeaseV1, project: &Path) {
    registry
        .try_upsert_project_tokens(project, 0)
        .await
        .unwrap();
    let roots = registry.try_list_project_paths().await.unwrap();
    eprintln!(
        "registered roots: {roots:?}; fixture project: {}; canonical: {:?}",
        project.display(),
        std::fs::canonicalize(project)
    );
}

/// Rollouts run in a scratch directory no project registered, as the
/// sessions the profile store owns do.
fn write_corpus(fixture: &DrainFixture, home: &Path, days: usize) -> usize {
    let scratch = fixture._tmp.path().join("unregistered-scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    for day in 0..days {
        for index in 0..ROLLOUTS_PER_DAY {
            write_rollout(home, &scratch, day, index);
        }
    }
    days * ROLLOUTS_PER_DAY
}

/// What the import left in the store, read back through SQLite: the rows
/// each table holds, so a run that admits less than its corpus says so
/// instead of measuring a smaller import.
struct StoreCensus {
    observations: u64,
    source_mutations: u64,
    pending_projections: u64,
    projection_publications: u64,
}

fn census(store: &Path) -> StoreCensus {
    let connection =
        rusqlite::Connection::open_with_flags(store, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let count = |table: &str| -> u64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
            .try_into()
            .unwrap()
    };
    StoreCensus {
        observations: count("observations"),
        source_mutations: count("external_source_mutations_v1"),
        pending_projections: count("external_source_pending_projections_v1"),
        projection_publications: count("external_source_projection_publications_v2"),
    }
}

/// The plan SQLite chooses for the projection drain's head lookup. It must
/// stay an index lookup: a scan here would make every window cost the whole
/// pending table.
fn assert_pending_head_plan_is_indexed(store: &Path) {
    let connection =
        rusqlite::Connection::open_with_flags(store, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let pending_head = "SELECT revisions.binding_json
         FROM external_source_pending_projections_v1 AS pending
         JOIN external_source_states_v1 AS states
           ON states.binding_id = pending.binding_id
          AND pending.predecessor_frontier_digest =
              COALESCE(states.projection_frontier_digest, 'root')
         JOIN external_source_binding_revisions_v1 AS revisions
           ON revisions.binding_id = states.binding_id
          AND revisions.binding_revision = states.binding_revision
         ORDER BY pending.successor_sequence, pending.binding_id
         LIMIT 1";
    let mut statement = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {pending_head}"))
        .unwrap();
    let plan: Vec<String> = statement
        .query_map([], |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    eprintln!("pending head plan: {plan:?}");
    // `SCAN <table> USING INDEX` is an ordered index walk that stops at
    // LIMIT 1; a bare `SCAN <table>` reads the whole table, and a
    // `TEMP B-TREE` sorts it first. Either would make every window cost the
    // whole pending table.
    let unindexed = plan.iter().find(|step| {
        (step.contains("SCAN") && !step.contains("USING INDEX")) || step.contains("TEMP B-TREE")
    });
    assert!(
        unindexed.is_none(),
        "the pending-head lookup must stay an index walk, got {plan:?}"
    );
}

/// Per-transaction write cost of the profile history import must not grow
/// with the store: the import writes one bounded window at a time, and a
/// window's cost is its own rows, not the rows already stored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_history_write_cost_does_not_grow_with_the_store() {
    let (fixture, database, registry, home, cpu) = open().await;
    register_project_root(&registry, &fixture.project).await;
    // The default corpus lands a few hundred MB so CI finishes in minutes;
    // `TRACEDECAY_PROFILE_COST_DAYS=20` grows it past the incident's 1 GB.
    let days: usize = std::env::var("TRACEDECAY_PROFILE_COST_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    let rollouts = write_corpus(&fixture, &home, days);
    let store = fixture._tmp.path().join("profile").join("user-sessions.db");

    let ingestor = ingestor(
        &fixture,
        &database,
        &registry,
        &home,
        &mounted_hub(&cpu),
        &cpu,
    );
    let (samples, total) = run_to_completion(&ingestor, &database, &store).await;
    ingestor.cancel();
    drop(ingestor);
    let census = census(&store);
    eprintln!(
        "store after import: observations={} source_mutations={} pending_projections={} \
         projection_publications={}",
        census.observations,
        census.source_mutations,
        census.pending_projections,
        census.projection_publications
    );
    for sample in &samples {
        let work = &sample.work;
        eprintln!(
            "profile history pass {:>3}: store={:>10} B committed={:>4} {:>6} us/txn \
             lock={:>6} us/txn cmds={:>5}/txn rows={:>6}/txn vm={:>8}/txn fullscan={:>7}/txn sort={:>6}/txn",
            sample.pass,
            sample.store_bytes,
            work.committed,
            micros_per_transaction(work),
            per_transaction(work.lock_held_micros, work),
            per_transaction(work.commands, work),
            per_transaction(work.rows, work),
            per_transaction(work.vm_steps, work),
            per_transaction(work.fullscan_steps, work),
            per_transaction(work.sort_steps, work),
        );
    }
    eprintln!(
        "profile history import over {rollouts} rollouts: passes={} committed={} rolled_back={} \
         total_us={} us/txn={}",
        samples.len(),
        total.committed,
        total.rolled_back,
        total.transaction_micros,
        micros_per_transaction(&total),
    );

    assert_pending_head_plan_is_indexed(&store);

    // The corpus must have landed: every rollout's records are in the store
    // and every projection head settled, or the probe measured a smaller
    // import than it generated.
    let frames_per_rollout = u64::try_from(2 * MESSAGES_PER_ROLLOUT + 1).unwrap();
    let rollouts = u64::try_from(rollouts).unwrap();
    assert_eq!(
        census.observations,
        rollouts * frames_per_rollout,
        "every generated frame must be admitted"
    );
    assert_eq!(
        census.pending_projections, 0,
        "the import must settle every projection head"
    );
    assert!(total.committed > 0, "the import must commit its rollouts");
    assert_eq!(total.rolled_back, 0, "every import transaction must commit");

    // Batching: the projection drain settles up to 32 heads per writer
    // transaction, so the import's transactions must stay a small fraction
    // of the records it projected, not one per record.
    let records = census.source_mutations.max(1);
    let txns_per_record = total.committed as f64 / records as f64;
    eprintln!("transactions per projected record: {txns_per_record:.4}");
    assert!(
        txns_per_record <= 1.0 / 16.0,
        "projection replay is not batched: {} transactions for {records} records",
        total.committed
    );

    // Store-size independence: compare the first and last thirds of the
    // import by committed work. A window's cost is bounded by its own rows,
    // so the late windows, written into a much larger store, must not cost
    // several times the early ones. Passes that committed nothing carry no
    // per-transaction cost and are left out of both thirds.
    let committing: Vec<&PassSample> = samples
        .iter()
        .filter(|sample| sample.work.committed > 0)
        .collect();
    let third = committing.len() / 3;
    let (mut early, mut late) = (WriterWork::default(), WriterWork::default());
    for (index, sample) in committing.iter().enumerate() {
        if index < third {
            early.committed += sample.work.committed;
            early.transaction_micros += sample.work.transaction_micros;
            early.vm_steps += sample.work.vm_steps;
        } else if index >= committing.len() - third {
            late.committed += sample.work.committed;
            late.transaction_micros += sample.work.transaction_micros;
            late.vm_steps += sample.work.vm_steps;
        }
    }
    let (early_us, late_us) = (
        micros_per_transaction(&early),
        micros_per_transaction(&late),
    );
    eprintln!(
        "early third: {} us/txn, {} vm/txn; late third: {} us/txn, {} vm/txn",
        early_us,
        per_transaction(early.vm_steps, &early),
        late_us,
        per_transaction(late.vm_steps, &late),
    );
    assert!(
        early.committed > 0 && late.committed > 0,
        "both thirds of the import must commit work"
    );
    assert!(
        late_us <= early_us.saturating_mul(4),
        "write cost grew with the store: early={early_us} us/txn late={late_us} us/txn"
    );
}
