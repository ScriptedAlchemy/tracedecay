//! Payload GC deletes follow the payloads they reap, not the store size.

use std::ops::Range;
use std::path::Path;

use tempfile::TempDir;
use tracedecay_lcm::LcmGcConfig;
use tracedecay_lcm::payload::{self, ExternalPayloadWrite, PayloadFileRollback};
use tracedecay_runtime_core::db::engine::params;

use crate::RegisteredGlobalDb;
use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, seed_projected_messages, writer_telemetry,
};

/// Writes one payload per projected message in `owners`, owned by that
/// message but no longer referenced by its row, each already an aged GC
/// candidate. Then runs one applied payload GC pass and returns the SQLite VM
/// steps its write transactions executed.
async fn reap_steps(
    database: &RegisteredGlobalDb,
    storage_root: &Path,
    owners: Range<usize>,
) -> u64 {
    let expected = owners.len();
    let mut rollback = PayloadFileRollback::begin_cancellation_safe(storage_root);
    let transaction = database.begin_write_transaction().await.unwrap();
    for index in owners {
        let message_id = format!("message.record.audit-batch-{index}");
        let written = payload::write_external_payload_tracked(
            storage_root,
            ExternalPayloadWrite {
                provider: "codex",
                session_id: "session.audit-batch",
                message_id: &message_id,
                kind: "message",
                content: &format!("payload owned by audit batch {index}"),
                metadata_json: None,
            },
            &mut rollback,
        )
        .unwrap();
        payload::upsert_payload_metadata(&transaction, &written)
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO lcm_gc_marks(payload_ref, state, first_seen_at, updated_at)
                 VALUES (?1, 'unreferenced', 1, 1)",
                params![written.payload_ref.as_str()],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
    rollback.disarm();

    let before = writer_telemetry(database).sqlite_vm.vm_steps;
    let report = database
        .lcm_run_payload_gc_apply(
            storage_root,
            "all",
            None,
            &LcmGcConfig::default(),
            tracedecay_runtime_core::tracedecay::current_timestamp(),
        )
        .await
        .unwrap();
    assert_eq!(report.unreferenced.count, expected);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    writer_telemetry(database).sqlite_vm.vm_steps - before
}

/// Each delete rewrites only its payload's owner row, found through the
/// `(provider, message_id)` unique index. Reaping the same number of payloads
/// on a store 8x larger costs the GC write transactions the same work.
#[tokio::test]
async fn payload_gc_deletes_do_not_scale_with_the_session_store() {
    const BASE: usize = 48;
    const GROWN: usize = 8 * BASE;
    const REAPED: usize = 16;

    let directory = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let storage_root = database.db_path().parent().unwrap().to_path_buf();

    seed_projected_messages(&runtime, 0..BASE).await;
    let base_steps = reap_steps(database, &storage_root, 0..REAPED).await;
    seed_projected_messages(&runtime, BASE..GROWN).await;
    let grown_steps = reap_steps(database, &storage_root, REAPED..2 * REAPED).await;

    eprintln!("payload GC reap VM steps: base={base_steps} grown={grown_steps}");
    assert!(
        grown_steps * 10 <= base_steps * 12,
        "reaping {REAPED} payloads on an 8x larger store must stay within 1.2x of the base \
         store's GC work: base={base_steps} grown={grown_steps}"
    );
}
