//! Request, probe, authority and trace plumbing shared by every stream.

use std::{
    fmt::Display,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::{Value as Json, json};
use tracedecay_domain::test_fixtures::repeated_sha256_text;
use tracedecay_rusqlite_runtime::{
    RuntimeWriteAuthority, RuntimeWriteAuthorityError, RuntimeWriteAuthorityStage,
};
use tracedecay_store::{
    RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreOperationMetadataV1,
    StoreRuntimeBindingV1,
};

use super::dump::{logical_dump, normalize_json, scrub_path};

/// `sha256:` followed by 64 copies of `byte`, a valid command digest.
pub fn digest(byte: char) -> String {
    repeated_sha256_text(byte)
}

/// One idempotent submit against `binding`. `digest_byte` stands for the
/// command digest: equal bytes under one key replay, different bytes conflict.
pub fn submit_request(
    binding: &StoreRuntimeBindingV1,
    operation: &str,
    key: &str,
    digest_byte: char,
    payload: RepositoryWritePayloadV1,
) -> RuntimeSubmitRequestV1 {
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(json!({
        "operation_id": format!("operation.parity.{operation}"),
        "client_id": "client.parity",
        "shard_id": binding.shard_id,
        "incarnation": binding.incarnation,
        "authority_epoch": binding.authority_epoch,
        "idempotency": { "key": key, "command_digest": digest(digest_byte) },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 4096,
        "admitted_at": 1_000_000
    }))
    .expect("valid parity metadata");
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!("transaction.parity.{operation}"))
            .expect("transaction id"),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata)
            .expect("batch compatibility"),
        opened_at: metadata.admitted_at,
    };
    let control: RuntimeRequestControlV1 = serde_json::from_value(json!({
        "requested_at": 1_000_000,
        "deadline": { "deadline_id": format!("deadline.parity.{operation}") },
        "cancellation": {
            "cancellation_id": format!("cancellation.parity.{operation}"),
            "generation": 1
        }
    }))
    .expect("request control");
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .unwrap_or_else(|error| panic!("valid parity submit request {operation}: {error}"))
}

pub struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    commit_started: AtomicBool,
}

impl Probe {
    pub fn for_request(request: &RuntimeSubmitRequestV1) -> Arc<Self> {
        Arc::new(Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            commit_started: AtomicBool::new(false),
        })
    }
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        self.commit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// Write authority that is valid until `revoked_at`, then denies.
pub struct Authority {
    pub revoked_at: Option<RuntimeWriteAuthorityStage>,
}

impl RuntimeWriteAuthority for Authority {
    fn verify(&self, stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        let rank = |stage| match stage {
            RuntimeWriteAuthorityStage::BeforeAdmission => 0,
            RuntimeWriteAuthorityStage::Dequeued => 1,
            RuntimeWriteAuthorityStage::BeforeCommit => 2,
        };
        match self.revoked_at {
            Some(revoked) if rank(stage) >= rank(revoked) => {
                Err(RuntimeWriteAuthorityError::denied(format!(
                    "parity authority revoked at {stage:?}"
                )))
            }
            _ => Ok(()),
        }
    }
}

/// The receipt trace of one stream plus where its store lives.
pub struct Trace {
    store_dir: PathBuf,
    lines: Vec<String>,
}

impl Trace {
    pub fn new(store_dir: &Path) -> Self {
        Self {
            store_dir: store_dir.to_path_buf(),
            lines: Vec::new(),
        }
    }

    pub fn record(&mut self, step: &str, result: Result<RuntimeSubmitOutcomeV1, impl Display>) {
        let entry = match result {
            Ok(outcome) => {
                let mut outcome = serde_json::to_value(outcome).expect("serialize outcome");
                normalize_json(&mut outcome);
                json!({ "step": step, "outcome": outcome })
            }
            Err(error) => self.error_entry(step, &error),
        };
        self.lines.push(entry.to_string());
    }

    /// A step whose result is not a submit outcome (exact-SQL store calls).
    pub fn note(&mut self, step: &str, result: Result<impl serde::Serialize, impl Display>) {
        let entry = match result {
            Ok(value) => {
                let mut value: Json = serde_json::to_value(value).expect("serialize result");
                normalize_json(&mut value);
                json!({ "step": step, "result": value })
            }
            Err(error) => self.error_entry(step, &error),
        };
        self.lines.push(entry.to_string());
    }

    fn error_entry(&self, step: &str, error: &dyn Display) -> Json {
        json!({ "step": step, "error": scrub_path(&error.to_string(), &self.store_dir) })
    }

    /// Receipt trace followed by the logical dump of `store`.
    pub fn finish(self, store: &Path) -> String {
        let mut out = String::from("# receipt trace\n");
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("# logical dump\n");
        out.push_str(&logical_dump(store, &self.store_dir));
        out
    }
}
