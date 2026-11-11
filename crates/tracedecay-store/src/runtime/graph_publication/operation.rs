use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use thiserror::Error;

use super::super::{
    RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StorageRuntimeContractErrorV1,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphPublicationStoreErrorV1 {
    #[error("invalid graph publication request: {0}")]
    InvalidRequest(#[from] StorageRuntimeContractErrorV1),
    #[error("graph publication interrupted: {0:?}")]
    Interrupted(RuntimeInterruptionV1),
    #[error("graph publication persistence is unavailable: {0}")]
    Infrastructure(String),
    /// The operation's commit grant refused the commit: the context already
    /// committed once, or the probe's arbitration denied it.
    #[error("graph publication commit refused by the operation's commit grant")]
    CommitRefused,
    #[error("graph publication persistence is corrupt: {0}")]
    Corrupt(String),
}

pub type GraphPublicationStoreResultV1<T> = Result<T, GraphPublicationStoreErrorV1>;

/// A real graph writer event. Encoding is never a durable page/segment;
/// applied stage pages and verified installed segments are durable boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphNativeWorkKindV1 {
    Entities,
    Relations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphNativeWorkV1 {
    CompactRowsEncoded {
        kind: GraphNativeWorkKindV1,
        completed_rows: u64,
        total_rows: u64,
        /// The sealed manifest's relation bound is known before any relation
        /// window completes. Zero is distinct from a bound not yet reported.
        total_relations: u64,
    },
    StagePageApplied {
        ordinal: u64,
        kind: GraphNativeWorkKindV1,
        mutations: u64,
        live_bytes: u64,
    },
    CompactSegmentVerified {
        entities: u64,
        relations: u64,
        canonical_bytes: u64,
    },
}

pub type GraphNativeWorkObserverV1 = Arc<dyn Fn(GraphNativeWorkV1) + Send + Sync>;

/// Existing caller-owned cancellation and monotonic deadline authority.
pub struct GraphPublicationOperationContextV1<'a> {
    probe: &'a dyn RuntimeRequestProbeV1,
    commit_started: AtomicBool,
    native_work_observer: Option<GraphNativeWorkObserverV1>,
}

impl<'a> GraphPublicationOperationContextV1<'a> {
    pub fn new(
        control: &RuntimeRequestControlV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        control.validate()?;
        if probe.cancellation_identity() != &control.cancellation {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph publication cancellation probe identity",
            });
        }
        if probe.deadline_identity() != &control.deadline {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph publication deadline probe identity",
            });
        }
        Ok(Self {
            probe,
            commit_started: AtomicBool::new(false),
            native_work_observer: None,
        })
    }

    #[must_use]
    pub fn with_native_work_observer(mut self, observer: GraphNativeWorkObserverV1) -> Self {
        self.native_work_observer = Some(observer);
        self
    }

    /// A journaled publication replaces its one-shot commit probe before
    /// writing native rows. Carry the same writer observation authority into
    /// that new commit context without carrying its spent commit grant.
    #[must_use]
    pub fn inheriting_native_work_from(
        mut self,
        original: &GraphPublicationOperationContextV1<'_>,
    ) -> Self {
        self.native_work_observer = original.native_work_observer.clone();
        self
    }

    pub fn observe_native_work(&self, work: GraphNativeWorkV1) {
        if let Some(observer) = &self.native_work_observer {
            observer(work);
        }
    }

    pub fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.probe.interruption()
    }

    /// The registered deadline identity this context enforces, so a typed
    /// `DeadlineExceeded` can be attributed to the operation that armed it.
    pub fn deadline_id(&self) -> &str {
        self.probe.deadline_identity().deadline_id.as_str()
    }

    /// A context over the same probe with an unspent commit grant, for an
    /// operation that commits more than once (superseded-replay retirement
    /// commits per retired replay and per cleanup finalization). The probe
    /// still arbitrates every commit.
    #[must_use]
    pub fn next_commit(&self) -> Self {
        Self {
            probe: self.probe,
            commit_started: AtomicBool::new(false),
            native_work_observer: self.native_work_observer.clone(),
        }
    }

    pub fn try_begin_verified_commit(&self) -> bool {
        self.try_begin_commit()
    }

    pub fn try_begin_replay_retirement_commit(&self) -> bool {
        self.try_begin_commit()
    }

    pub fn try_begin_pending_discard_commit(&self) -> bool {
        self.try_begin_commit()
    }

    pub fn try_begin_retired_cleanup_finalize_commit(&self) -> bool {
        self.try_begin_commit()
    }

    fn try_begin_commit(&self) -> bool {
        self.commit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && self.probe.try_begin_commit()
    }
}
