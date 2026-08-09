//! Durable types retained for the combined product synthesis-admission port.

use tracedecay_domain::{WorkAttemptIdentityV1, WorkAuthority};

use crate::work_synthesis::{WorkSynthesisAdmissionRecordV1, WorkSynthesisAdmissionV1};

use super::{WorkAttemptStorageError, WorkAttemptStoragePort};

/// Outcome of atomically inserting an admitted synthesis and its attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkSynthesisInsertOutcome {
    Inserted,
    Replayed(Box<WorkSynthesisAdmissionV1>),
}

/// Which durable admission authority owns an attempt identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkAttemptAdmissionKind {
    Ordinary,
    Synthesis,
}

/// Durable synthesis record access used by the combined product transaction.
/// Public synthesis admission never calls these row-level writes directly.
pub trait WorkSynthesisAdmissionStoragePort: WorkAttemptStoragePort {
    fn insert_synthesis(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError>;

    fn insert_synthesis_bounded(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
        concurrency: &tracedecay_domain::configuration::TopologyConcurrencyPolicyV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError>;

    /// Loads the immutable synthesis record. An ordinary row is a typed
    /// conflict, while absence remains a typed not-found result.
    fn load_synthesis(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkSynthesisAdmissionRecordV1, WorkAttemptStorageError>;
}
