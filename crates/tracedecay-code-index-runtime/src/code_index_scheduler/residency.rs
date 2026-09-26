//! A mounted worktree's decoded generations as owners in the process
//! resident-memory inventory.
//!
//! The decoded generation a worktree serves was held until the daemon exited:
//! the first read that needed the whole generation set a latch that nothing
//! cleared. Here that residency is a lease renewed by those reads. Once it
//! lapses, or under pressure, the inventory releases the decode and the
//! worktree returns to the state a restart leaves it in: exact, lexical and
//! graph reads keep serving from the text artifact and the durable graph, and
//! the next read that needs the whole generation re-decodes it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::CodeGenerationId;
use tracedecay_runtime_core::resident_memory::{
    ResidentOwnerBytesV1, ResidentOwnerKindV1, ResidentOwnerRegistrationV1, ResidentOwnerReleaseV1,
    ResidentOwnerSampleV1, ResidentOwnerScopeV1, ResidentOwnerV1, ResidentOwnersV1,
};

use super::DaemonCodeIndexPublicationStoreV1;
use super::reconcile::ReconcilePassesV1;
use super::registry::ServingGenerationSlot;

pub(super) struct WorktreeResidencyV1 {
    serving_generation: Arc<ServingGenerationSlot>,
    serving_generation_epoch: Arc<AtomicU64>,
    serving_generation_changed: Arc<tokio::sync::watch::Sender<()>>,
    complete_generation_requested: Arc<AtomicBool>,
    reconcile_in_progress: Arc<ReconcilePassesV1>,
    publication: DaemonCodeIndexPublicationStoreV1,
    last_used: Mutex<Instant>,
    /// The generation the seat held when the inventory released it. The
    /// worktree still serves it from disk, so freshness keeps reporting it
    /// as seated until a later generation takes the seat.
    released_seat: Mutex<Option<CodeGenerationId>>,
}

pub(super) struct WorktreeResidencyPartsV1 {
    pub(super) serving_generation: Arc<ServingGenerationSlot>,
    pub(super) serving_generation_epoch: Arc<AtomicU64>,
    pub(super) serving_generation_changed: Arc<tokio::sync::watch::Sender<()>>,
    pub(super) complete_generation_requested: Arc<AtomicBool>,
    pub(super) reconcile_in_progress: Arc<ReconcilePassesV1>,
    pub(super) publication: DaemonCodeIndexPublicationStoreV1,
}

impl WorktreeResidencyV1 {
    pub(super) fn new(parts: WorktreeResidencyPartsV1) -> Self {
        Self {
            serving_generation: parts.serving_generation,
            serving_generation_epoch: parts.serving_generation_epoch,
            serving_generation_changed: parts.serving_generation_changed,
            complete_generation_requested: parts.complete_generation_requested,
            reconcile_in_progress: parts.reconcile_in_progress,
            publication: parts.publication,
            last_used: Mutex::new(Instant::now()),
            released_seat: Mutex::new(None),
        }
    }

    /// Renew the lease: a read needed the whole decoded generation.
    pub(super) fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn last_used(&self) -> Instant {
        *self
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn released_seat(&self) -> Option<CodeGenerationId> {
        self.released_seat
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn seated(&self) -> Option<Arc<CodeIndexPublishedGenerationV1>> {
        self.serving_generation
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(super::LatestCompleteCodeIndexV1::generation_handle)
    }

    fn busy(&self) -> bool {
        self.reconcile_in_progress.running()
    }

    /// Register both of this worktree's owners with `owners`. The returned
    /// guard keeps the owners alive and registered for the mount's lifetime.
    pub(super) fn register(
        self: Arc<Self>,
        owners: &Arc<ResidentOwnersV1>,
        scope: ResidentOwnerScopeV1,
    ) -> WorktreeResidencyRegistrationV1 {
        let serving: Arc<dyn ResidentOwnerV1> = Arc::new(ServingDecodeOwnerV1(Arc::clone(&self)));
        let superseded: Arc<dyn ResidentOwnerV1> = Arc::new(SupersededDecodesOwnerV1(self));
        let registrations = [
            (ResidentOwnerKindV1::DecodedGeneration, &serving),
            (ResidentOwnerKindV1::SupersededGeneration, &superseded),
        ]
        .into_iter()
        .filter_map(|(kind, owner)| {
            owners
                .register(scope.clone(), kind, Arc::downgrade(owner))
                .inspect_err(|error| {
                    tracing::error!(
                        event = "resident_owner_registration_failed",
                        kind = kind.as_str(),
                        error = %error,
                        "a retained code-index owner is not visible to the resident-memory inventory"
                    );
                })
                .ok()
        })
        .collect();
        WorktreeResidencyRegistrationV1 {
            _owners: [serving, superseded],
            _registrations: registrations,
        }
    }
}

/// Keeps a mount's owners registered until the mount drops.
pub(super) struct WorktreeResidencyRegistrationV1 {
    _owners: [Arc<dyn ResidentOwnerV1>; 2],
    _registrations: Vec<ResidentOwnerRegistrationV1>,
}

fn distinct_bytes(generations: &[Arc<CodeIndexPublishedGenerationV1>]) -> u64 {
    generations
        .iter()
        .enumerate()
        .filter(|(index, generation)| {
            !generations[..*index]
                .iter()
                .any(|earlier| Arc::ptr_eq(earlier, generation))
        })
        .map(|(_, generation)| generation.retained_bytes())
        .fold(0, u64::saturating_add)
}

/// The generation the worktree serves: the serving seat and the publication
/// cache's active decode, which are usually one allocation.
struct ServingDecodeOwnerV1(Arc<WorktreeResidencyV1>);

impl ResidentOwnerV1 for ServingDecodeOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let residency = &self.0;
        let held = residency
            .seated()
            .into_iter()
            .chain(residency.publication.decoded_active())
            .collect::<Vec<_>>();
        let generation_id = held.first()?.manifest().generation_id.clone();
        Some(ResidentOwnerSampleV1 {
            generation_id,
            bytes: ResidentOwnerBytesV1::Measured(distinct_bytes(&held)),
            last_used: residency.last_used(),
            serving: true,
        })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        let residency = &self.0;
        if residency.busy() {
            return ResidentOwnerReleaseV1::Busy;
        }
        let seated = residency
            .serving_generation
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(|latest| latest.generation_handle());
        residency
            .complete_generation_requested
            .store(false, Ordering::Release);
        if let Some(seated) = &seated {
            *residency
                .released_seat
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                Some(seated.manifest().generation_id.clone());
            residency
                .serving_generation_epoch
                .fetch_add(1, Ordering::AcqRel);
            residency.serving_generation_changed.send_replace(());
        }
        let released = seated
            .into_iter()
            .chain(residency.publication.release_decoded_active())
            .collect::<Vec<_>>();
        if released.is_empty() {
            return ResidentOwnerReleaseV1::Empty;
        }
        ResidentOwnerReleaseV1::Released {
            bytes: ResidentOwnerBytesV1::Measured(distinct_bytes(&released)),
        }
    }
}

/// Decoded generations the publication cache keeps besides the active one.
struct SupersededDecodesOwnerV1(Arc<WorktreeResidencyV1>);

impl ResidentOwnerV1 for SupersededDecodesOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let held = self.0.publication.superseded_decodes();
        let generation_id = held.last()?.manifest().generation_id.clone();
        Some(ResidentOwnerSampleV1 {
            generation_id,
            bytes: ResidentOwnerBytesV1::Measured(distinct_bytes(&held)),
            last_used: self.0.last_used(),
            serving: false,
        })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        let released = self.0.publication.release_superseded_decodes();
        if released.is_empty() {
            return ResidentOwnerReleaseV1::Empty;
        }
        ResidentOwnerReleaseV1::Released {
            bytes: ResidentOwnerBytesV1::Measured(distinct_bytes(&released)),
        }
    }
}
