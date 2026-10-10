//! A mounted worktree's decoded generations as owners in the process
//! resident-memory inventory.
//!
//! The decoded generation a worktree serves was held until the daemon exited:
//! the first read that needed the whole generation set a latch that nothing
//! cleared. Here that residency is a lease renewed by those reads. The worker
//! also drops the seated decode when it parks after a publish: exact, lexical,
//! and callers already serve from the sealed text artifact and the warm
//! catalog/engine, so keeping the whole generation was a third copy of the
//! same index (#3328). Once the lease lapses, or under pressure, the inventory
//! releases the decode and the graph engine, and the worktree returns to the
//! state a restart leaves it in: exact and lexical reads keep serving from
//! the text artifact, graph reads answer warming while the engine reopens
//! from the durable graph, and the next read that needs the whole generation
//! re-decodes it.
//!
//! Reads that only report on the seat (the status census, freshness) do not
//! renew the lease, and a refresh of the worktree refused for memory takes
//! the serving graph back between requests: that graph is what the refresh
//! replaces, so protecting it until it idles would only hold the refresh.

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::Instant;

use tracedecay_code_index::graph_projection::{
    CodeGraphCatalogReleaseV1, CodeGraphEngineReleaseV1,
};
use tracedecay_code_index::production::{
    CodeIndexPublishedGenerationV1, DecodedGenerationContentV1,
};
use tracedecay_code_index::retained_parse::{RetainedParsePoolReleaseV1, SharedRetainedParsePool};
use tracedecay_runtime_core::resident_memory::{
    ResidentHoldingV1, ResidentOwnerBytesV1, ResidentOwnerKindV1, ResidentOwnerRegistrationV1,
    ResidentOwnerReleaseV1, ResidentOwnerSampleV1, ResidentOwnerScopeV1, ResidentOwnerV1,
    ResidentOwnersV1, ResidentSharedContentV1,
};

use super::reconcile::ReconcilePassesV1;
use super::registry::{CodeIndexSchedulerRegistryV1, ServingGenerationSlot};
use super::{DaemonCodeIndexPublicationStoreV1, LatestCodeTextGenerationV1};

/// Whether a serving read renews its worktree's residency lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServingReadLeaseV1 {
    /// The read serves from the decoded generation or its graph.
    Renew,
    /// The read only reports on the seat, so it must not keep it resident.
    Observe,
}

pub(super) struct WorktreeResidencyV1 {
    serving_generation: Arc<ServingGenerationSlot>,
    serving_generation_epoch: Arc<AtomicU64>,
    serving_generation_changed: Arc<tokio::sync::watch::Sender<()>>,
    serving_seats: Arc<tokio::sync::watch::Sender<u64>>,
    complete_generation_requested: Arc<AtomicBool>,
    reconcile_in_progress: Arc<ReconcilePassesV1>,
    publication: DaemonCodeIndexPublicationStoreV1,
    text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>>,
    retained_parses: SharedRetainedParsePool,
    last_used: Mutex<Instant>,
    refresh_waits_for_memory: AtomicBool,
}

pub(super) struct WorktreeResidencyPartsV1 {
    pub(super) serving_generation: Arc<ServingGenerationSlot>,
    pub(super) serving_generation_epoch: Arc<AtomicU64>,
    pub(super) serving_generation_changed: Arc<tokio::sync::watch::Sender<()>>,
    pub(super) serving_seats: Arc<tokio::sync::watch::Sender<u64>>,
    pub(super) complete_generation_requested: Arc<AtomicBool>,
    pub(super) reconcile_in_progress: Arc<ReconcilePassesV1>,
    pub(super) publication: DaemonCodeIndexPublicationStoreV1,
    pub(super) text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>>,
    pub(super) retained_parses: SharedRetainedParsePool,
}

impl WorktreeResidencyV1 {
    pub(super) fn new(parts: WorktreeResidencyPartsV1) -> Self {
        Self {
            serving_generation: parts.serving_generation,
            serving_generation_epoch: parts.serving_generation_epoch,
            serving_generation_changed: parts.serving_generation_changed,
            serving_seats: parts.serving_seats,
            complete_generation_requested: parts.complete_generation_requested,
            reconcile_in_progress: parts.reconcile_in_progress,
            publication: parts.publication,
            text_generation: parts.text_generation,
            retained_parses: parts.retained_parses,
            last_used: Mutex::new(Instant::now()),
            refresh_waits_for_memory: AtomicBool::new(false),
        }
    }

    /// Record whether this worktree's last reconcile was refused for resident
    /// memory. While it is, memory given back anywhere in the process is its
    /// retry.
    pub(super) fn set_refresh_waits_for_memory(&self, waits: bool) {
        self.refresh_waits_for_memory
            .store(waits, Ordering::Release);
    }

    pub(super) fn refresh_waits_for_memory(&self) -> bool {
        self.refresh_waits_for_memory.load(Ordering::Acquire)
    }

    /// Give back the serving graph a refused refresh of this worktree is
    /// waiting on. An engine a graph read holds answers busy and stays, so
    /// only the gaps between requests are taken; graph reads meanwhile answer
    /// warming, as after an idle release. A release is announced as headroom.
    pub(super) fn yield_serving_graph_to_refresh(self: &Arc<Self>, owners: &ResidentOwnersV1) {
        let released = [
            (
                ResidentOwnerKindV1::GraphEngine,
                OutgoingGraphOwnerV1(Arc::clone(self)).release(),
            ),
            (
                ResidentOwnerKindV1::GraphCatalog,
                GraphCatalogOwnerV1(Arc::clone(self)).release(),
            ),
            (
                ResidentOwnerKindV1::GraphEngine,
                GraphEngineOwnerV1(Arc::clone(self)).release(),
            ),
        ]
        .into_iter()
        .filter_map(|(kind, release)| match release {
            ResidentOwnerReleaseV1::Released { bytes } => Some((kind, bytes)),
            ResidentOwnerReleaseV1::Busy | ResidentOwnerReleaseV1::Empty => None,
        })
        .inspect(|(kind, bytes)| {
            tracing::info!(
                event = "code_index_serving_graph_yielded_to_refresh",
                kind = kind.as_str(),
                bytes = bytes.measured(),
                "a refresh refused for memory took back the serving graph it replaces"
            );
        })
        .count();
        if released > 0 {
            owners.note_headroom();
        }
    }

    /// Drop the seated decode once the worker parks.
    ///
    /// Exact, lexical, and callers already serve from the sealed text
    /// artifact and the warm catalog/engine. The seated
    /// [`CodeIndexPublishedGenerationV1`] is a third copy of that
    /// generation. Keep it while a complete read demands the seat, a
    /// reconcile is still running, or the installed text owner cannot yet
    /// serve: a text owner whose projection is still warming or that
    /// latched a failure leaves the seat as the only servable generation,
    /// and taking it then turns serve-old into `GenerationUnavailable`.
    /// Do not clear `complete_generation_requested`: a demand that
    /// arrives during the take must still wake a successor pass to
    /// re-decode.
    pub(super) fn release_decode_when_parked(self: &Arc<Self>, owners: &ResidentOwnersV1) {
        if self.busy() || self.complete_generation_requested.load(Ordering::Acquire) {
            return;
        }
        if !self
            .serving_text()
            .is_some_and(|text| text.query_owners_are_ready())
        {
            return;
        }
        let seated = {
            let mut slot = self
                .serving_generation
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if self.complete_generation_requested.load(Ordering::Acquire) {
                return;
            }
            slot.take()
        };
        if seated.is_some() {
            self.serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
            CodeIndexSchedulerRegistryV1::record_serving_seat(&self.serving_seats);
            self.serving_generation_changed.send_replace(());
        }
        let serving = HeldDecodesV1::release(
            seated
                .map(|latest| latest.generation_handle())
                .into_iter()
                .chain(self.publication.release_decoded_active())
                .collect(),
        );
        let superseded = HeldDecodesV1::release(self.publication.release_superseded_decodes());
        let released = [serving, superseded]
            .into_iter()
            .filter_map(|release| match release {
                ResidentOwnerReleaseV1::Released { bytes } => Some(bytes),
                ResidentOwnerReleaseV1::Busy | ResidentOwnerReleaseV1::Empty => None,
            })
            .collect::<Vec<_>>();
        if released.is_empty() {
            return;
        }
        owners.note_headroom();
        tracing::info!(
            event = "code_index_serving_decode_released_on_park",
            bytes = released
                .iter()
                .map(|bytes| bytes.measured().unwrap_or(0))
                .sum::<u64>(),
            "a parked worktree gave back its seated decode; text and graph keep serving"
        );
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

    /// Register every one of this worktree's owners with `owners`. The returned
    /// guard keeps the owners alive and registered for the mount's lifetime.
    pub(super) fn register(
        self: Arc<Self>,
        owners: &Arc<ResidentOwnersV1>,
        scope: ResidentOwnerScopeV1,
    ) -> WorktreeResidencyRegistrationV1 {
        let serving: Arc<dyn ResidentOwnerV1> = Arc::new(ServingDecodeOwnerV1(Arc::clone(&self)));
        let superseded: Arc<dyn ResidentOwnerV1> =
            Arc::new(SupersededDecodesOwnerV1(Arc::clone(&self)));
        let graph_catalog: Arc<dyn ResidentOwnerV1> =
            Arc::new(GraphCatalogOwnerV1(Arc::clone(&self)));
        let retained_parses: Arc<dyn ResidentOwnerV1> =
            Arc::new(RetainedParsesOwnerV1(Arc::clone(&self)));
        let outgoing_graph: Arc<dyn ResidentOwnerV1> =
            Arc::new(OutgoingGraphOwnerV1(Arc::clone(&self)));
        let graph_engine: Arc<dyn ResidentOwnerV1> = Arc::new(GraphEngineOwnerV1(self));
        let registrations = [
            (ResidentOwnerKindV1::RetainedParses, &retained_parses),
            (ResidentOwnerKindV1::DecodedGeneration, &serving),
            (ResidentOwnerKindV1::SupersededGeneration, &superseded),
            (ResidentOwnerKindV1::GraphCatalog, &graph_catalog),
            (ResidentOwnerKindV1::GraphEngine, &graph_engine),
            (ResidentOwnerKindV1::GraphEngine, &outgoing_graph),
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
            _owners: [
                retained_parses,
                serving,
                superseded,
                graph_catalog,
                graph_engine,
                outgoing_graph,
            ],
            _registrations: registrations,
        }
    }
}

/// Keeps a mount's owners registered until the mount drops.
pub(super) struct WorktreeResidencyRegistrationV1 {
    _owners: [Arc<dyn ResidentOwnerV1>; 6],
    _registrations: Vec<ResidentOwnerRegistrationV1>,
}

/// What a set of held generations retains: the decoded content the first of
/// them shares with other worktrees, reported apart, and everything else,
/// each distinct allocation once.
struct HeldDecodesV1 {
    own_bytes: u64,
    shared: Option<Arc<DecodedGenerationContentV1>>,
}

impl HeldDecodesV1 {
    fn of(generations: &[Arc<CodeIndexPublishedGenerationV1>]) -> Self {
        let shared = generations
            .first()
            .and_then(|generation| generation.shared_content())
            .cloned();
        let own_bytes = generations
            .iter()
            .enumerate()
            .filter(|(index, generation)| {
                !generations[..*index]
                    .iter()
                    .any(|earlier| Arc::ptr_eq(earlier, generation))
            })
            .map(|(_, generation)| {
                let content = generation
                    .shared_content()
                    .filter(|content| {
                        shared
                            .as_ref()
                            .is_some_and(|shared| Arc::ptr_eq(shared, content))
                    })
                    .map_or(0, |content| content.retained_bytes());
                generation.retained_bytes().saturating_sub(content)
            })
            .fold(0, u64::saturating_add);
        Self { own_bytes, shared }
    }

    fn sample(
        generations: &[Arc<CodeIndexPublishedGenerationV1>],
        last_used: Instant,
        serving: bool,
    ) -> Option<ResidentOwnerSampleV1> {
        let holding =
            ResidentHoldingV1::Generation(generations.first()?.manifest().generation_id.clone());
        let held = Self::of(generations);
        Some(ResidentOwnerSampleV1 {
            holding,
            bytes: ResidentOwnerBytesV1::Measured(held.own_bytes),
            last_used,
            serving,
            shared: held.shared.map(|content| ResidentSharedContentV1 {
                digest: content.digest().clone(),
                bytes: content.retained_bytes(),
                allocation: Arc::downgrade(&content) as Weak<dyn Any + Send + Sync>,
            }),
        })
    }

    /// Drop `generations` and report what that gave back: their own bytes,
    /// and the shared content only once no other worktree references it.
    fn release(generations: Vec<Arc<CodeIndexPublishedGenerationV1>>) -> ResidentOwnerReleaseV1 {
        if generations.is_empty() {
            return ResidentOwnerReleaseV1::Empty;
        }
        let Self { own_bytes, shared } = Self::of(&generations);
        let content = shared.map(|content| (Arc::downgrade(&content), content.retained_bytes()));
        drop(generations);
        let freed_content = content
            .filter(|(content, _)| content.strong_count() == 0)
            .map_or(0, |(_, bytes)| bytes);
        ResidentOwnerReleaseV1::Released {
            bytes: ResidentOwnerBytesV1::Measured(own_bytes.saturating_add(freed_content)),
        }
    }
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
        HeldDecodesV1::sample(&held, residency.last_used(), true)
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
        if seated.is_some() {
            residency
                .serving_generation_epoch
                .fetch_add(1, Ordering::AcqRel);
            CodeIndexSchedulerRegistryV1::record_serving_seat(&residency.serving_seats);
            residency.serving_generation_changed.send_replace(());
        }
        HeldDecodesV1::release(
            seated
                .into_iter()
                .chain(residency.publication.release_decoded_active())
                .collect(),
        )
    }
}

/// Decoded generations the publication cache keeps besides the active one.
struct SupersededDecodesOwnerV1(Arc<WorktreeResidencyV1>);

impl ResidentOwnerV1 for SupersededDecodesOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let mut held = self.0.publication.superseded_decodes();
        held.reverse();
        HeldDecodesV1::sample(&held, self.0.last_used(), false)
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        HeldDecodesV1::release(self.0.publication.release_superseded_decodes())
    }
}

/// The documents the worktree's increments retained for incremental reparse,
/// charged by the pages of the pool's own heap.
struct RetainedParsesOwnerV1(Arc<WorktreeResidencyV1>);

impl ResidentOwnerV1 for RetainedParsesOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let held = self.0.retained_parses.holding()?;
        Some(ResidentOwnerSampleV1 {
            holding: ResidentHoldingV1::Worktree,
            bytes: held.bytes.map_or(
                ResidentOwnerBytesV1::Unmeasured,
                ResidentOwnerBytesV1::Measured,
            ),
            last_used: held.last_retained,
            serving: false,
            shared: None,
        })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        match self.0.retained_parses.release() {
            RetainedParsePoolReleaseV1::Released { bytes } => ResidentOwnerReleaseV1::Released {
                bytes: bytes.map_or(
                    ResidentOwnerBytesV1::Unmeasured,
                    ResidentOwnerBytesV1::Measured,
                ),
            },
            RetainedParsePoolReleaseV1::Busy => ResidentOwnerReleaseV1::Busy,
            RetainedParsePoolReleaseV1::Empty => ResidentOwnerReleaseV1::Empty,
        }
    }
}

/// The native graph engine pinned for the worktree's serving text generation.
struct GraphEngineOwnerV1(Arc<WorktreeResidencyV1>);

impl WorktreeResidencyV1 {
    fn serving_text(&self) -> Option<LatestCodeTextGenerationV1> {
        self.text_generation
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The interactive catalog built over the worktree's serving graph.
struct GraphCatalogOwnerV1(Arc<WorktreeResidencyV1>);

impl ResidentOwnerV1 for GraphCatalogOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let text = self.0.serving_text()?;
        let bytes = text
            .interactive_graph_store()
            .ok()?
            .interactive_catalog_bytes()?;
        Some(ResidentOwnerSampleV1 {
            holding: ResidentHoldingV1::Generation(
                text.metadata().manifest().generation_id.clone(),
            ),
            bytes: ResidentOwnerBytesV1::Measured(bytes),
            last_used: self.0.last_used(),
            serving: true,
            shared: None,
        })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        let Some(store) = self
            .0
            .serving_text()
            .and_then(|text| text.interactive_graph_store().ok())
        else {
            return ResidentOwnerReleaseV1::Empty;
        };
        match store.release_interactive_catalog() {
            CodeGraphCatalogReleaseV1::Released { bytes } => ResidentOwnerReleaseV1::Released {
                bytes: ResidentOwnerBytesV1::Measured(bytes),
            },
            CodeGraphCatalogReleaseV1::Busy => ResidentOwnerReleaseV1::Busy,
            CodeGraphCatalogReleaseV1::NotReady => ResidentOwnerReleaseV1::Empty,
        }
    }
}

impl ResidentOwnerV1 for GraphEngineOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let text = self.0.serving_text()?;
        let store = text.interactive_graph_store().ok()?;
        let bytes = match store.serving_engine_bytes() {
            Ok(None) => return None,
            Ok(Some(bytes)) => ResidentOwnerBytesV1::Measured(bytes),
            Err(_) => ResidentOwnerBytesV1::Unmeasured,
        };
        Some(ResidentOwnerSampleV1 {
            holding: ResidentHoldingV1::Generation(
                text.metadata().manifest().generation_id.clone(),
            ),
            bytes,
            last_used: self.0.last_used(),
            serving: true,
            shared: None,
        })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        let Some(store) = self
            .0
            .serving_text()
            .and_then(|text| text.interactive_graph_store().ok())
        else {
            return ResidentOwnerReleaseV1::Empty;
        };
        match store.release_serving_engine() {
            Ok(CodeGraphEngineReleaseV1::Released { bytes }) => ResidentOwnerReleaseV1::Released {
                bytes: bytes.map_or(
                    ResidentOwnerBytesV1::Unmeasured,
                    ResidentOwnerBytesV1::Measured,
                ),
            },
            Ok(CodeGraphEngineReleaseV1::Busy) => ResidentOwnerReleaseV1::Busy,
            Ok(CodeGraphEngineReleaseV1::NotPinned) => ResidentOwnerReleaseV1::Empty,
            Err(error) => {
                tracing::warn!(
                    event = "code_graph_engine_release_failed",
                    error = %error,
                    "the serving graph engine could not be released; it stays resident"
                );
                ResidentOwnerReleaseV1::Busy
            }
        }
    }
}

/// The outgoing generation's warm graph, which the serving text holds so
/// graph reads keep answering while its own graph publishes and warms. It is
/// not the serving graph: a refresh refused for memory takes it back first.
struct OutgoingGraphOwnerV1(Arc<WorktreeResidencyV1>);

impl ResidentOwnerV1 for OutgoingGraphOwnerV1 {
    fn sample(&self) -> Option<ResidentOwnerSampleV1> {
        let held = self.0.serving_text()?.held_graph_predecessor()?;
        let store = held.interactive_graph_store().ok()?;
        let catalog = store.interactive_catalog_bytes();
        let bytes = match store.serving_engine_bytes() {
            Ok(None) if catalog.is_none() => return None,
            Ok(engine) => ResidentOwnerBytesV1::Measured(
                engine.unwrap_or(0).saturating_add(catalog.unwrap_or(0)),
            ),
            Err(_) => ResidentOwnerBytesV1::Unmeasured,
        };
        Some(ResidentOwnerSampleV1 {
            holding: ResidentHoldingV1::Generation(
                held.metadata().manifest().generation_id.clone(),
            ),
            bytes,
            last_used: self.0.last_used(),
            serving: false,
            shared: None,
        })
    }

    fn release(&self) -> ResidentOwnerReleaseV1 {
        let Some(text) = self.0.serving_text() else {
            return ResidentOwnerReleaseV1::Empty;
        };
        let Some(store) = text
            .held_graph_predecessor()
            .and_then(|held| held.interactive_graph_store().ok())
        else {
            return ResidentOwnerReleaseV1::Empty;
        };
        let catalog = match store.release_interactive_catalog() {
            CodeGraphCatalogReleaseV1::Released { bytes } => bytes,
            CodeGraphCatalogReleaseV1::Busy => return ResidentOwnerReleaseV1::Busy,
            CodeGraphCatalogReleaseV1::NotReady => 0,
        };
        let engine = match store.release_serving_engine() {
            Ok(CodeGraphEngineReleaseV1::Released { bytes }) => bytes,
            Ok(CodeGraphEngineReleaseV1::NotPinned) => Some(0),
            busy => {
                if let Err(error) = busy {
                    tracing::warn!(
                        event = "code_graph_engine_release_failed",
                        error = %error,
                        "the outgoing graph engine could not be released; it stays resident"
                    );
                }
                return if catalog > 0 {
                    ResidentOwnerReleaseV1::Released {
                        bytes: ResidentOwnerBytesV1::Measured(catalog),
                    }
                } else {
                    ResidentOwnerReleaseV1::Busy
                };
            }
        };
        text.release_graph_predecessor();
        ResidentOwnerReleaseV1::Released {
            bytes: engine.map_or(ResidentOwnerBytesV1::Unmeasured, |engine| {
                ResidentOwnerBytesV1::Measured(engine.saturating_add(catalog))
            }),
        }
    }
}
