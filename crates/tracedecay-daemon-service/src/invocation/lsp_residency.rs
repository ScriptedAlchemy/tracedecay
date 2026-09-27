//! Each open LSP session as an owner in the process resident-memory
//! inventory, so the memory an editor's unsaved documents hold is reported
//! beside the code index's retained state.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use tracedecay_daemon_protocol::LspSessionId;
use tracedecay_runtime_core::resident_memory::{
    ResidentHoldingV1, ResidentOwnerBytesV1, ResidentOwnerKindV1,
    ResidentOwnerRegistrationFailureV1, ResidentOwnerRegistrationV1, ResidentOwnerReleaseV1,
    ResidentOwnerSampleV1, ResidentOwnerScopeV1, ResidentOwnerV1, ResidentOwnersV1,
};

struct LspSessionResidentStateV1 {
    session: String,
    bytes: AtomicU64,
    last_used: Mutex<Instant>,
}

impl ResidentOwnerV1 for LspSessionResidentStateV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        Some(ResidentOwnerSampleV1 {
            holding: ResidentHoldingV1::Session(self.session.clone()),
            bytes: ResidentOwnerBytesV1::Measured(self.bytes.load(Ordering::Acquire)),
            last_used: *self
                .last_used
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            serving: true,
            shared: None,
        })
    }

    /// Unsaved editor text has no durable copy to fall back to, so the
    /// inventory never drops it; the session's lease does.
    fn release(&self) -> ResidentOwnerReleaseV1 {
        ResidentOwnerReleaseV1::Busy
    }
}

/// Keeps one session's row in the inventory until the session is dropped.
pub(super) struct LspSessionResidencyV1 {
    state: Arc<LspSessionResidentStateV1>,
    _registration: ResidentOwnerRegistrationV1,
}

impl LspSessionResidencyV1 {
    pub(super) fn register(
        owners: &Arc<ResidentOwnersV1>,
        scope: ResidentOwnerScopeV1,
        session: &LspSessionId,
        bytes: u64,
    ) -> Result<Self, ResidentOwnerRegistrationFailureV1> {
        let state = Arc::new(LspSessionResidentStateV1 {
            session: session.as_str().to_owned(),
            bytes: AtomicU64::new(bytes),
            last_used: Mutex::new(Instant::now()),
        });
        let owner: Arc<dyn ResidentOwnerV1> = state.clone();
        let registration =
            owners.register(scope, ResidentOwnerKindV1::Session, Arc::downgrade(&owner))?;
        Ok(Self {
            state,
            _registration: registration,
        })
    }

    /// Record the session's current holding after the client used it.
    pub(super) fn observe(&self, bytes: u64) {
        self.state.bytes.store(bytes, Ordering::Release);
        *self
            .state
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }
}
