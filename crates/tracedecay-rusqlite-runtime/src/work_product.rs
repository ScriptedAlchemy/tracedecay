//! The production Work product graph authority over the registered exact-SQL
//! channel.
//!
//! The application crate owns the Work product family as pure ports:
//! `WorkProductEventPortV1` atomically appends to an immutable journal and
//! records its verified graph version, while `WorkGraphReadPortV1` serves those
//! verified versions with every projection derived from the same version.
//! This adapter is the producer for the whole family, including the DAG,
//! timeline, causal, workload, and critical-path projections the Work views
//! draw.
//!
//! Four rules hold this adapter to what it can actually prove.
//!
//! 1. **Declared, never derived.** Item effort, causal candidates,
//!    `scheduled_at`, and `deadline` reach a projection only by having been
//!    written into a `WorkProductEventPayloadV1` by the caller that declared
//!    them. This module reads them back out of the journal and folds them; it
//!    never estimates one, never backfills one, and never reads one off an
//!    attempt row.
//! 2. **Explicit runtime authority.** `work_attempts_v1` is scoped by
//!    [`WorkAuthority`](tracedecay_domain::WorkAuthority); the product journal
//!    is scoped by the registered profile owner. Plain product storage does not
//!    correlate them and reports unavailable coverage. A daemon route may bind
//!    its registered Work authority explicitly and then hydrates only attempts
//!    the product graph already accepted.
//! 3. **Only verified versions are readable.** The event, its verified version
//!    and the new current state commit in one transaction, so a caller can never
//!    observe an event without the graph authority that verified and digested
//!    it.
//! 4. **Nothing replays the journal.** Every query is keyed by the owner and
//!    bounded by a page size or by the checkpoint interval (see [`journal`]).
//!    The current graph is a projection the append maintains in its own
//!    transaction, older versions are rebuilt from the nearest checkpoint, and
//!    where a selection's covered prefix ends is read from a registry of scope
//!    sets. `docs/plans/work-current-state.md` has the design.

use tracedecay_contracts::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkEvidenceReadPortErrorV1,
    WorkEvidenceRootReadErrorV1, WorkGraphReadPortErrorV1, WorkProductApplicationErrorV1,
    WorkProductAttemptAdmissionErrorV1, WorkProductEventPortErrorV1, WorkStorageFailureV1,
};
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkGraphVersionV1, WorkProductEventSequenceV1, WorkProductEventV1,
    WorkProductGraphV1, WorkProductSourceWatermarkV1, canonical_sha256,
};

use crate::exact_sql::{ExactSqlError, ExactSqlReaderRefusalV1, ExactSqlValue};

mod attempt_admission;
pub(crate) mod journal;
pub(crate) use attempt_admission::{admit_retry, admit_synthesis};
mod authorization;
mod events;
mod evidence;
mod history;
pub mod instrument;
mod migration;
mod read;
mod rooted_evidence;

pub use read::AuthorizedWorkProductReadStorageV1;

/// The digest domain separator for a recovered Work product graph.
pub(crate) const WORK_PRODUCT_GRAPH_DIGEST_DOMAIN: &str =
    "tracedecay.rusqlite-runtime.work-product-graph.v1";

/// The digest domain separator for a minted Work product event identity.
pub(crate) const WORK_PRODUCT_EVENT_ID_DOMAIN: &str =
    "tracedecay.rusqlite-runtime.work-product-event-id.v1";

/// One journal row: the durable sequence the port assigned, and the event.
#[derive(Clone, Debug)]
pub(crate) struct WorkProductJournalEntryV1 {
    pub(crate) sequence: WorkProductEventSequenceV1,
    pub(crate) event: WorkProductEventV1,
}

/// One published, verified graph version and the two instants that place it.
///
/// `valid_at` is the event's own `occurred_at`, when the change became true.
/// `observed_at` is when this authority verified and published it. They are
/// distinct on purpose: a forensic read asks about the second, an as-of read
/// about the first.
#[derive(Clone, Debug)]
pub(crate) struct WorkProductPublishedVersionV1 {
    pub(crate) graph_version: WorkGraphVersionV1,
    pub(crate) event_sequence: WorkProductEventSequenceV1,
    pub(crate) valid_at: UtcMicros,
    pub(crate) observed_at: UtcMicros,
    pub(crate) source_watermark: WorkProductSourceWatermarkV1,
    pub(crate) recovered_graph_digest: String,
}

/// Why a Work product storage read or write failed.
///
/// The exact SQL layer already separates busy, result-limit, expiry, authority,
/// and SQLite failures, and a row that does not decode is the one cause it
/// cannot see. This keeps both so each port can answer with the category the
/// caller needs instead of one unit `Unavailable`.
#[derive(Debug)]
pub(crate) enum StoreFault {
    /// Persisted bytes did not decode into the shape the store wrote. The
    /// label names the loader and never carries row content.
    Corrupt(&'static str),
    Sql(ExactSqlError),
}

impl std::fmt::Display for StoreFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(what) => write!(formatter, "stored {what} did not decode"),
            Self::Sql(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<ExactSqlError> for StoreFault {
    fn from(error: ExactSqlError) -> Self {
        Self::Sql(error)
    }
}

/// The answer a port gives for a [`StoreFault`], in the port's own vocabulary.
///
/// Only the expected-revision or uniqueness conflict is a `VersionConflict`,
/// and that is decided where the statement runs, never here: nothing in this
/// trait can produce a conflict, so a storage fault cannot be mistaken for
/// stale input.
pub(crate) trait FromStoreFault: Sized {
    fn storage(failure: WorkStorageFailureV1) -> Self;
    fn denied() -> Self;
    fn timed_out() -> Self;
    fn unavailable() -> Self;

    /// Classifies `fault`, and records its retained cause in tracing. The
    /// public answer carries the category only, so SQL parameters, paths, and
    /// foreign authority detail stay in the log.
    fn from_fault(fault: impl Into<StoreFault>) -> Self {
        let fault = fault.into();
        tracing::warn!(
            target: "tracedecay.work_product.storage",
            cause = %fault,
            "Work product storage failed"
        );
        match fault {
            StoreFault::Corrupt(_) => Self::storage(WorkStorageFailureV1::StoredDataInvalid),
            StoreFault::Sql(
                ExactSqlError::Busy
                | ExactSqlError::ReaderRefused(ExactSqlReaderRefusalV1::Saturated),
            ) => Self::storage(WorkStorageFailureV1::Busy),
            StoreFault::Sql(ExactSqlError::QueryLimitExceeded) => {
                Self::storage(WorkStorageFailureV1::ResultLimitExceeded)
            }
            StoreFault::Sql(ExactSqlError::TransactionExpired) => Self::timed_out(),
            StoreFault::Sql(
                ExactSqlError::AuthorityDenied(_) | ExactSqlError::AuthorityMismatch,
            ) => Self::denied(),
            StoreFault::Sql(_) => Self::unavailable(),
        }
    }
}

/// The ports that share the `Storage` / `NotFoundOrNotAuthorized` / `TimedOut`
/// / `Unavailable` vocabulary.
macro_rules! store_fault_port {
    ($($port:ident),+) => {$(
        impl FromStoreFault for $port {
            fn storage(failure: WorkStorageFailureV1) -> Self {
                Self::Storage(failure)
            }
            fn denied() -> Self {
                Self::NotFoundOrNotAuthorized
            }
            fn timed_out() -> Self {
                Self::TimedOut
            }
            fn unavailable() -> Self {
                Self::Unavailable
            }
        }
    )+};
}
store_fault_port!(
    WorkProductEventPortErrorV1,
    WorkGraphReadPortErrorV1,
    WorkEvidenceReadPortErrorV1,
    WorkEvidenceRootReadErrorV1,
    WorkProductAttemptAdmissionErrorV1
);

/// The history read answers in the application error directly.
impl FromStoreFault for WorkProductApplicationErrorV1 {
    fn storage(failure: WorkStorageFailureV1) -> Self {
        Self::Storage(failure)
    }
    fn denied() -> Self {
        Self::NotFoundOrNotAuthorized
    }
    fn timed_out() -> Self {
        Self::TimedOut
    }
    fn unavailable() -> Self {
        Self::EventAuthorityUnavailable
    }
}

pub(crate) fn owner_params(scope: &AuthorizedWorkProductScopeV1) -> Vec<ExactSqlValue> {
    vec![
        ExactSqlValue::Text(scope.owner_brain_id().as_str().to_owned()),
        ExactSqlValue::Text(scope.owner_profile_id().as_str().to_owned()),
    ]
}

/// The exact digest a verified version records for a folded graph.
pub(crate) fn recovered_graph_digest(graph: &WorkProductGraphV1) -> Option<ManifestDigest> {
    canonical_sha256(&(WORK_PRODUCT_GRAPH_DIGEST_DOMAIN, graph)).ok()
}

/// Rebuild the verified version identity for one published row.
pub(crate) fn verified_version(
    published: &WorkProductPublishedVersionV1,
) -> Option<VerifiedWorkGraphVersionV1> {
    if published.observed_at < published.valid_at {
        return None;
    }
    VerifiedWorkGraphVersionV1::new(
        published.graph_version,
        published.event_sequence,
        published.source_watermark.clone(),
        ManifestDigest::new(published.recovered_graph_digest.clone()).ok()?,
    )
    .ok()
}

#[cfg(test)]
mod store_fault_tests {
    use super::journal::WriteFault;
    use super::*;

    type Port = WorkProductEventPortErrorV1;

    fn sqlite(extended_code: i32) -> ExactSqlError {
        ExactSqlError::Sqlite {
            operation: "execute",
            code: Some(extended_code & 0xff),
            extended_code: Some(extended_code),
            message: "injected".to_owned(),
        }
    }

    /// What a port answers for a failed row write.
    fn write_answer(error: ExactSqlError) -> Port {
        match WriteFault::from(error) {
            WriteFault::Conflict => Port::VersionConflict,
            WriteFault::Store(fault) => Port::from_fault(fault),
        }
    }

    #[test]
    fn only_a_primary_key_or_unique_violation_is_a_version_conflict() {
        for conflict in [1555, 2067] {
            assert_eq!(write_answer(sqlite(conflict)), Port::VersionConflict);
        }
        // CHECK, trigger abort, I/O error, busy, and a lost writer all fail
        // the insert without the revision having moved.
        for other in [275, 1811, 778, 5] {
            assert_eq!(
                write_answer(sqlite(other)),
                Port::Unavailable,
                "SQLite code {other} must not read as a stale revision"
            );
        }
        assert_eq!(
            write_answer(ExactSqlError::Busy),
            Port::Storage(WorkStorageFailureV1::Busy)
        );
        assert_eq!(
            write_answer(ExactSqlError::WriterUnavailable),
            Port::Unavailable
        );
    }

    #[test]
    fn each_exact_sql_category_keeps_its_own_answer() {
        let classify = |fault: StoreFault| Port::from_fault(fault);
        assert_eq!(
            classify(ExactSqlError::Busy.into()),
            Port::Storage(WorkStorageFailureV1::Busy)
        );
        assert_eq!(
            classify(ExactSqlError::ReaderRefused(ExactSqlReaderRefusalV1::Saturated).into()),
            Port::Storage(WorkStorageFailureV1::Busy)
        );
        assert_eq!(
            classify(ExactSqlError::QueryLimitExceeded.into()),
            Port::Storage(WorkStorageFailureV1::ResultLimitExceeded)
        );
        assert_eq!(
            classify(StoreFault::Corrupt("journal row")),
            Port::Storage(WorkStorageFailureV1::StoredDataInvalid)
        );
        assert_eq!(
            classify(ExactSqlError::TransactionExpired.into()),
            Port::TimedOut
        );
        assert_eq!(
            classify(ExactSqlError::AuthorityDenied("secret".to_owned()).into()),
            Port::NotFoundOrNotAuthorized
        );
        assert_eq!(
            classify(ExactSqlError::WriterUnavailable.into()),
            Port::Unavailable
        );
        // Draining admits nothing further, so it is not capacity to wait out.
        assert_eq!(
            classify(ExactSqlError::ReaderRefused(ExactSqlReaderRefusalV1::Draining).into()),
            Port::Unavailable
        );
    }
}
