//! Ledger dispositions on a bare project store: fresh commit, exact replay,
//! idempotency conflict, revoked authority, and the effect payloads the
//! repository executor rejects.

use std::sync::Arc;

use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::{
    RuntimeWriteAuthorityStage,
    repository::{RepositoryPhysicalAttachmentFactory, RepositoryRuntimePhysicalAttachment},
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::super::{
    fixtures,
    harness::{Authority, Probe, Trace, digest, submit_request},
};

fn binding(scope: serde_json::Value) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.parity",
            "profile_id": "profile.parity",
            "scope": scope
        },
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .expect("parity binding")
}

pub fn project_binding() -> StoreRuntimeBindingV1 {
    binding(serde_json::json!({ "kind": "project", "project_id": "project.parity" }))
}

fn initialize(
    binding: &StoreRuntimeBindingV1,
    path: &std::path::Path,
) -> RepositoryRuntimePhysicalAttachment {
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(digest('d')).expect("locator digest"),
    );
    let attachment = RepositoryPhysicalAttachmentFactory
        .initialize(
            binding.clone(),
            locator,
            path.to_path_buf(),
            AdmissionConfigV1::default(),
        )
        .expect("initialize parity store");
    attachment
        .commit_initialization()
        .expect("commit parity store initialization");
    attachment
}

async fn submit(
    attachment: &RepositoryRuntimePhysicalAttachment,
    trace: &mut Trace,
    step: &str,
    request: tracedecay_store::RuntimeSubmitRequestV1,
    revoked_at: Option<RuntimeWriteAuthorityStage>,
) {
    let probe = Probe::for_request(&request);
    let result = attachment
        .dispatch_submit(request, probe, Arc::new(Authority { revoked_at }))
        .await;
    trace.record(step, result);
}

fn close(attachment: RepositoryRuntimePhysicalAttachment) {
    attachment.drain().expect("drain parity store");
    attachment.close_and_join().expect("close parity store");
}

pub async fn ledger() -> String {
    let directory = tempfile::tempdir().expect("ledger store directory");
    let path = directory.path().join("ledger.db");
    let binding = project_binding();
    let attachment = initialize(&binding, &path);
    let mut trace = Trace::new(directory.path());
    let dispatched = || {
        RepositoryWritePayloadV1::EnqueueOutbox(Box::new(fixtures::effects::outbox_entry(
            &binding,
            "effect.parity.dispatched",
            "dispatched",
        )))
    };
    let stream = [
        ("fresh", "key.enqueue", 'a', None),
        ("exact_replay", "key.enqueue", 'a', None),
        ("idempotency_conflict", "key.enqueue", 'b', None),
        (
            "revoked_before_admission",
            "key.revoked.admission",
            'c',
            Some(RuntimeWriteAuthorityStage::BeforeAdmission),
        ),
        (
            "revoked_after_dequeue",
            "key.revoked.dequeue",
            'c',
            Some(RuntimeWriteAuthorityStage::Dequeued),
        ),
        (
            "revoked_before_commit",
            "key.revoked.commit",
            'c',
            Some(RuntimeWriteAuthorityStage::BeforeCommit),
        ),
        ("after_revocation", "key.after", 'e', None),
    ];
    for (step, key, byte, revoked_at) in stream {
        let request = submit_request(&binding, step, key, byte, dispatched());
        submit(&attachment, &mut trace, step, request, revoked_at).await;
    }
    let effects = [
        (
            "enqueue_pending_rejected",
            RepositoryWritePayloadV1::EnqueueOutbox(Box::new(fixtures::effects::outbox_entry(
                &binding,
                "effect.parity.pending",
                "pending",
            ))),
        ),
        (
            "apply_inbox_rejected",
            RepositoryWritePayloadV1::ApplyInbox(Box::new(fixtures::effects::outbox_entry(
                &binding,
                "effect.parity.inbox",
                "pending",
            ))),
        ),
        (
            "acknowledge_outbox",
            RepositoryWritePayloadV1::AcknowledgeOutbox(Box::new(fixtures::effects::inbox_receipt(
                &binding,
                "effect.parity.dispatched",
            ))),
        ),
    ];
    for (step, payload) in effects {
        let request = submit_request(&binding, step, &format!("key.{step}"), 'f', payload);
        submit(&attachment, &mut trace, step, request, None).await;
    }
    close(attachment);
    trace.finish(&path)
}

/// `GitIndexTransaction` targets a code shard, and the repository executor
/// does not own it.
pub async fn code() -> String {
    let directory = tempfile::tempdir().expect("code store directory");
    let path = directory.path().join("code.db");
    let binding = binding(fixtures::project::code_scope());
    let attachment = initialize(&binding, &path);
    let mut trace = Trace::new(directory.path());
    let request = submit_request(
        &binding,
        "git_index_transaction",
        "key.git_index_transaction",
        'a',
        RepositoryWritePayloadV1::GitIndexTransaction(Box::new(
            fixtures::project::git_index_transaction(),
        )),
    );
    submit(
        &attachment,
        &mut trace,
        "git_index_transaction_rejected",
        request,
        None,
    )
    .await;
    close(attachment);
    trace.finish(&path)
}
