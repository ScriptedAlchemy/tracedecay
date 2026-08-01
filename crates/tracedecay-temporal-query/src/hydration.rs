use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;
use tracedecay_domain::{HydrationStateV1, RetrievalAnchorId};
use zeroize::Zeroizing;

use super::ports::{TemporalExecutionSnapshot, TemporalPortError, await_controlled};

/// Fallible pre-allocation ceiling for a single authorized payload buffer.
const MAX_HYDRATION_PREALLOC_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HydrationDenial {
    state: HydrationStateV1,
}

impl HydrationDenial {
    pub fn new(state: HydrationStateV1) -> Result<Self, HydrationError> {
        if state == HydrationStateV1::Available {
            return Err(HydrationError::InvalidDenial);
        }
        Ok(Self { state })
    }

    pub const fn state(&self) -> HydrationStateV1 {
        self.state
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HydrationAuthorization {
    Authorized,
    Denied(HydrationDenial),
}

pub struct HydrationGrant<'a> {
    snapshot: &'a TemporalExecutionSnapshot,
    anchor_id: &'a RetrievalAnchorId,
    max_bytes: usize,
    max_chunk_bytes: usize,
    remaining_total_bytes: usize,
}

impl<'a> HydrationGrant<'a> {
    pub const fn snapshot(&self) -> &'a TemporalExecutionSnapshot {
        self.snapshot
    }

    pub const fn anchor_id(&self) -> &'a RetrievalAnchorId {
        self.anchor_id
    }

    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub const fn max_chunk_bytes(&self) -> usize {
        self.max_chunk_bytes
    }
}

pub struct HydrationSink<'a> {
    grant: &'a HydrationGrant<'a>,
    bytes: Zeroizing<Vec<u8>>,
}

impl<'a> HydrationSink<'a> {
    fn with_grant(grant: &'a HydrationGrant<'a>) -> Result<Self, HydrationError> {
        let capacity = grant
            .max_bytes
            .min(grant.remaining_total_bytes)
            .min(MAX_HYDRATION_PREALLOC_BYTES);
        let mut bytes = Vec::new();
        bytes
            .try_reserve(capacity)
            .map_err(|_| HydrationError::BudgetExceeded {
                resource: "allocation",
            })?;
        Ok(Self {
            grant,
            bytes: Zeroizing::new(bytes),
        })
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.bytes.capacity()
    }

    pub fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), HydrationError> {
        self.grant
            .snapshot
            .request()
            .execution_control()
            .checkpoint()?;
        if chunk.len() > self.grant.max_chunk_bytes {
            return Err(HydrationError::BudgetExceeded {
                resource: "chunk bytes",
            });
        }
        let next_len =
            self.bytes
                .len()
                .checked_add(chunk.len())
                .ok_or(HydrationError::BudgetExceeded {
                    resource: "payload bytes",
                })?;
        if next_len > self.grant.max_bytes {
            return Err(HydrationError::BudgetExceeded {
                resource: "payload bytes",
            });
        }
        if next_len > self.grant.remaining_total_bytes {
            return Err(HydrationError::BudgetExceeded {
                resource: "total bytes",
            });
        }
        self.bytes.extend_from_slice(chunk);
        self.grant
            .snapshot
            .request()
            .execution_control()
            .checkpoint()?;
        Ok(())
    }
}

pub type HydrationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HydrationError>> + Send + 'a>>;

pub trait TemporalHydrationPort: Send + Sync {
    fn authorize_hydration<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        anchor_id: &'a RetrievalAnchorId,
    ) -> HydrationFuture<'a, HydrationAuthorization>;

    fn read_authorized<'a>(
        &'a self,
        grant: &'a HydrationGrant<'_>,
        sink: &'a mut HydrationSink<'_>,
    ) -> HydrationFuture<'a, ()>;
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HydrationError {
    #[error("hydration payload is unavailable")]
    Unavailable,
    #[error("available hydration cannot be represented as a denial")]
    InvalidDenial,
    #[error("hydration exceeded its frozen {resource} budget")]
    BudgetExceeded { resource: &'static str },
    #[error("hydration execution control interrupted work")]
    Interrupted(#[from] TemporalPortError),
}

#[derive(Clone, PartialEq, Eq)]
pub struct HydratedPayload {
    anchor_id: RetrievalAnchorId,
    bytes: Zeroizing<Vec<u8>>,
}

impl HydratedPayload {
    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn into_parts(self) -> (RetrievalAnchorId, Zeroizing<Vec<u8>>) {
        (self.anchor_id, self.bytes)
    }
}

impl fmt::Debug for HydratedPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HydratedPayload")
            .field("anchor_id", &self.anchor_id)
            .field("bytes", &"REDACTED")
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnavailableHydration {
    anchor_id: RetrievalAnchorId,
    state: HydrationStateV1,
}

impl UnavailableHydration {
    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub const fn state(&self) -> HydrationStateV1 {
        self.state
    }

    pub(super) fn into_parts(self) -> (RetrievalAnchorId, HydrationStateV1) {
        (self.anchor_id, self.state)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HydrationBatch {
    pub available: Vec<HydratedPayload>,
    pub unavailable: Vec<UnavailableHydration>,
}

pub async fn hydrate_selected(
    port: &impl TemporalHydrationPort,
    snapshot: &TemporalExecutionSnapshot,
    anchors: &[RetrievalAnchorId],
) -> Result<HydrationBatch, HydrationError> {
    let limits = snapshot.request().limits();
    snapshot.request().execution_control().checkpoint()?;
    let reserve = anchors.len().min(limits.hydration_limit);
    let mut batch = HydrationBatch::default();
    batch
        .available
        .try_reserve(reserve)
        .map_err(|_| HydrationError::BudgetExceeded {
            resource: "allocation",
        })?;
    batch
        .unavailable
        .try_reserve(reserve)
        .map_err(|_| HydrationError::BudgetExceeded {
            resource: "allocation",
        })?;
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0_usize;
    // This loop is deliberately sequential and must stay that way. Each
    // authorized read is granted `remaining_total = hydration_total_bytes -
    // total_bytes`, where `total_bytes` is the running sum of every prior
    // anchor's read. That gate bounds the sink and decides whether a read
    // truncates, succeeds, or trips `BudgetExceeded` at a specific anchor; the
    // resulting payloads are also appended to `batch` in anchor order. Running
    // anchors concurrently would have to grant each read a budget computed
    // before its predecessors finished, changing truncation, the first
    // over-budget anchor, and batch ordering — i.e. changing the output.
    // Bounded concurrency cannot preserve this running-budget semantics, so the
    // sequential walk is the correct implementation.
    for anchor_id in anchors {
        snapshot.request().execution_control().checkpoint()?;
        if !seen.insert(anchor_id.clone()) {
            continue;
        }
        if seen.len() > limits.hydration_limit {
            return Err(HydrationError::BudgetExceeded {
                resource: "record count",
            });
        }
        let control = snapshot.request().execution_control();
        let authorization =
            await_controlled(control, port.authorize_hydration(snapshot, anchor_id)).await?;
        match authorization {
            HydrationAuthorization::Denied(denial) => {
                batch.unavailable.push(UnavailableHydration {
                    anchor_id: anchor_id.clone(),
                    state: denial.state(),
                });
            }
            HydrationAuthorization::Authorized => {
                let remaining_total = limits
                    .hydration_total_bytes
                    .checked_sub(total_bytes)
                    .ok_or(HydrationError::BudgetExceeded {
                        resource: "total bytes",
                    })?;
                let grant = HydrationGrant {
                    snapshot,
                    anchor_id,
                    max_bytes: limits.hydration_payload_bytes,
                    max_chunk_bytes: limits.hydration_chunk_bytes,
                    remaining_total_bytes: remaining_total,
                };
                let mut sink = HydrationSink::with_grant(&grant)?;
                await_controlled(control, port.read_authorized(&grant, &mut sink)).await?;
                total_bytes = total_bytes.checked_add(sink.bytes.len()).ok_or(
                    HydrationError::BudgetExceeded {
                        resource: "total bytes",
                    },
                )?;
                batch.available.push(HydratedPayload {
                    anchor_id: anchor_id.clone(),
                    bytes: sink.bytes,
                });
            }
        }
        snapshot.request().execution_control().checkpoint()?;
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use tracedecay_domain::{
        HydrationStateV1, RetrievalAnchorId, RetrievalGrainV1, SessionId, TemporalModeV1,
    };

    use super::*;
    use crate::ports::{
        BindingDigest, ExecutionControl, ExecutionLimits, KernelVersions,
        TemporalExecutionSnapshot, TemporalPortError, TemporalSnapshotRequest, TemporalWatermarks,
    };
    use crate::resolution::types::ValidatedAuthorization;
    use crate::test_support::block_on;

    fn anchor(value: &str) -> RetrievalAnchorId {
        serde_json::from_str(&format!("\"{value}\"")).expect("valid anchor")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot() -> TemporalExecutionSnapshot {
        snapshot_with_limits(ExecutionLimits::default())
    }

    fn snapshot_with_limits(limits: ExecutionLimits) -> TemporalExecutionSnapshot {
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        TemporalExecutionSnapshot::new_authorized(
            TemporalSnapshotRequest::new(
                session_id,
                digest('0'),
                digest('1'),
                digest('2'),
                TemporalModeV1::Current,
                RetrievalGrainV1::LogicalMessage,
            )
            .expect("valid request")
            .with_limits(limits),
            TemporalWatermarks {
                generation: 1,
                source: 2,
                projection: 3,
                index: 4,
                summary: 5,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            None,
            ValidatedAuthorization::Authorized,
        )
        .expect("valid snapshot")
    }

    struct OrderedHydrator {
        calls: Mutex<Vec<&'static str>>,
    }

    impl TemporalHydrationPort for OrderedHydrator {
        fn authorize_hydration<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> HydrationFuture<'a, HydrationAuthorization> {
            Box::pin(async move {
                self.calls.lock().expect("calls").push("authorize");
                Ok(HydrationAuthorization::Authorized)
            })
        }

        fn read_authorized<'a>(
            &'a self,
            grant: &'a HydrationGrant<'_>,
            sink: &'a mut HydrationSink<'_>,
        ) -> HydrationFuture<'a, ()> {
            Box::pin(async move {
                self.calls.lock().expect("calls").push("read");
                assert_eq!(grant.anchor_id(), &anchor("ordered"));
                sink.write_chunk(b"privacy-canary-secret")?;
                Ok(())
            })
        }
    }

    #[test]
    fn authorization_grant_is_minted_before_any_payload_read() {
        block_on(async {
            let hydrator = OrderedHydrator {
                calls: Mutex::new(Vec::new()),
            };
            let requested = anchor("ordered");

            let batch = hydrate_selected(&hydrator, &snapshot(), &[requested])
                .await
                .expect("authorized hydration");

            assert_eq!(
                hydrator.calls.lock().expect("calls").as_slice(),
                ["authorize", "read"]
            );
            assert_eq!(batch.available[0].bytes(), b"privacy-canary-secret");
            assert!(!format!("{batch:?}").contains("privacy-canary-secret"));
        });
    }

    struct DenyingHydrator {
        reads: AtomicUsize,
    }

    impl TemporalHydrationPort for DenyingHydrator {
        fn authorize_hydration<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> HydrationFuture<'a, HydrationAuthorization> {
            Box::pin(async {
                Ok(HydrationAuthorization::Denied(
                    HydrationDenial::new(HydrationStateV1::Unauthorized)
                        .expect("unauthorized is a denial"),
                ))
            })
        }

        fn read_authorized<'a>(
            &'a self,
            _grant: &'a HydrationGrant<'_>,
            _sink: &'a mut HydrationSink<'_>,
        ) -> HydrationFuture<'a, ()> {
            Box::pin(async move {
                self.reads.fetch_add(1, Ordering::SeqCst);
                panic!("denied hydration must never reach payload read")
            })
        }
    }

    #[test]
    fn denied_variant_has_no_payload_and_never_reads_bytes() {
        block_on(async {
            let hydrator = DenyingHydrator {
                reads: AtomicUsize::new(0),
            };
            let denied = anchor("denied");

            let batch = hydrate_selected(&hydrator, &snapshot(), &[denied])
                .await
                .expect("denial is an unavailable result");

            assert!(batch.available.is_empty());
            assert_eq!(batch.unavailable[0].state(), HydrationStateV1::Unauthorized);
            assert_eq!(hydrator.reads.load(Ordering::SeqCst), 0);
        });
    }

    struct OversizedHydrator {
        observed_max: AtomicUsize,
    }

    impl TemporalHydrationPort for OversizedHydrator {
        fn authorize_hydration<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> HydrationFuture<'a, HydrationAuthorization> {
            Box::pin(async { Ok(HydrationAuthorization::Authorized) })
        }

        fn read_authorized<'a>(
            &'a self,
            grant: &'a HydrationGrant<'_>,
            sink: &'a mut HydrationSink<'_>,
        ) -> HydrationFuture<'a, ()> {
            Box::pin(async move {
                self.observed_max.store(grant.max_bytes(), Ordering::SeqCst);
                sink.write_chunk(&vec![0; grant.max_bytes() + 1])
            })
        }
    }

    #[test]
    fn hydration_sink_enforces_payload_bound_before_crossing_boundary() {
        block_on(async {
            let hydrator = OversizedHydrator {
                observed_max: AtomicUsize::new(0),
            };
            let requested = anchor("bounded");
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 8,
                hydration_total_bytes: 8,
                ..ExecutionLimits::default()
            });

            assert_eq!(
                hydrate_selected(&hydrator, &snapshot, &[requested]).await,
                Err(HydrationError::BudgetExceeded {
                    resource: "payload bytes"
                })
            );
            assert_eq!(hydrator.observed_max.load(Ordering::SeqCst), 8);
        });
    }

    struct FixedPayloadHydrator;

    impl TemporalHydrationPort for FixedPayloadHydrator {
        fn authorize_hydration<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> HydrationFuture<'a, HydrationAuthorization> {
            Box::pin(async { Ok(HydrationAuthorization::Authorized) })
        }

        fn read_authorized<'a>(
            &'a self,
            _grant: &'a HydrationGrant<'_>,
            sink: &'a mut HydrationSink<'_>,
        ) -> HydrationFuture<'a, ()> {
            Box::pin(async move { sink.write_chunk(b"12345") })
        }
    }

    #[test]
    fn hydration_sink_enforces_total_bound_across_authorized_reads() {
        block_on(async {
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 8,
                hydration_total_bytes: 8,
                ..ExecutionLimits::default()
            });

            assert_eq!(
                hydrate_selected(
                    &FixedPayloadHydrator,
                    &snapshot,
                    &[anchor("first"), anchor("second")],
                )
                .await,
                Err(HydrationError::BudgetExceeded {
                    resource: "total bytes"
                })
            );
        });
    }

    #[test]
    fn hydration_sink_rejects_adapter_chunks_above_the_frozen_chunk_cap() {
        block_on(async {
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 8,
                hydration_total_bytes: 8,
                hydration_chunk_bytes: 4,
                ..ExecutionLimits::default()
            });

            assert_eq!(
                hydrate_selected(&FixedPayloadHydrator, &snapshot, &[anchor("chunk")]).await,
                Err(HydrationError::BudgetExceeded {
                    resource: "chunk bytes"
                })
            );
        });
    }

    struct CancellingHydrator;

    impl TemporalHydrationPort for CancellingHydrator {
        fn authorize_hydration<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> HydrationFuture<'a, HydrationAuthorization> {
            Box::pin(async { Ok(HydrationAuthorization::Authorized) })
        }

        fn read_authorized<'a>(
            &'a self,
            grant: &'a HydrationGrant<'_>,
            sink: &'a mut HydrationSink<'_>,
        ) -> HydrationFuture<'a, ()> {
            Box::pin(async move {
                sink.write_chunk(b"first")?;
                grant.snapshot().request().execution_control().cancel();
                sink.write_chunk(b"second")
            })
        }
    }

    #[test]
    fn hydration_observes_live_cancellation_midstream() {
        block_on(async {
            let requested = anchor("cancelled");
            assert_eq!(
                hydrate_selected(&CancellingHydrator, &snapshot(), &[requested]).await,
                Err(HydrationError::Interrupted(TemporalPortError::Cancelled))
            );
        });
    }

    struct CapacityProbeHydrator {
        capacity: AtomicUsize,
    }

    impl TemporalHydrationPort for CapacityProbeHydrator {
        fn authorize_hydration<'a>(
            &'a self,
            _snapshot: &'a TemporalExecutionSnapshot,
            _anchor_id: &'a RetrievalAnchorId,
        ) -> HydrationFuture<'a, HydrationAuthorization> {
            Box::pin(async { Ok(HydrationAuthorization::Authorized) })
        }

        fn read_authorized<'a>(
            &'a self,
            _grant: &'a HydrationGrant<'_>,
            sink: &'a mut HydrationSink<'_>,
        ) -> HydrationFuture<'a, ()> {
            Box::pin(async move {
                self.capacity.store(sink.capacity(), Ordering::SeqCst);
                sink.write_chunk(b"ok")
            })
        }
    }

    #[test]
    fn hydration_sink_preallocates_within_frozen_effective_bounds() {
        block_on(async {
            let hydrator = CapacityProbeHydrator {
                capacity: AtomicUsize::new(0),
            };
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 8,
                hydration_total_bytes: 8,
                ..ExecutionLimits::default()
            });

            hydrate_selected(&hydrator, &snapshot, &[anchor("prealloc")])
                .await
                .expect("authorized hydration");
            assert!(hydrator.capacity.load(Ordering::SeqCst) >= 8);
            assert!(hydrator.capacity.load(Ordering::SeqCst) <= MAX_HYDRATION_PREALLOC_BYTES);
        });
    }

    #[test]
    fn hydration_sink_does_not_preallocate_unbounded_configured_limits() {
        block_on(async {
            let hydrator = CapacityProbeHydrator {
                capacity: AtomicUsize::new(0),
            };
            // Stay inside ports validation ceilings while exceeding the sink prealloc cap.
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 8 * 1024 * 1024,
                hydration_total_bytes: 8 * 1024 * 1024,
                hydration_chunk_bytes: 64 * 1024,
                ..ExecutionLimits::default()
            });

            hydrate_selected(&hydrator, &snapshot, &[anchor("huge-limit")])
                .await
                .expect("tiny write under huge configured limit");
            assert!(hydrator.capacity.load(Ordering::SeqCst) <= MAX_HYDRATION_PREALLOC_BYTES);
            assert!(hydrator.capacity.load(Ordering::SeqCst) >= 1);
        });
    }

    #[test]
    fn hydration_payload_bound_accepts_exact_and_rejects_over() {
        block_on(async {
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 5,
                hydration_total_bytes: 5,
                hydration_chunk_bytes: 5,
                ..ExecutionLimits::default()
            });
            let exact = hydrate_selected(&FixedPayloadHydrator, &snapshot, &[anchor("exact")])
                .await
                .expect("exact payload");
            assert_eq!(exact.available[0].bytes(), b"12345");

            let over = snapshot_with_limits(ExecutionLimits {
                hydration_payload_bytes: 4,
                hydration_total_bytes: 4,
                // Chunk cap must allow the adapter write so payload accounting rejects it.
                hydration_chunk_bytes: 5,
                ..ExecutionLimits::default()
            });
            assert_eq!(
                hydrate_selected(&FixedPayloadHydrator, &over, &[anchor("over")]).await,
                Err(HydrationError::BudgetExceeded {
                    resource: "payload bytes"
                })
            );
        });
    }

    #[test]
    fn hydration_record_count_bound_is_exact() {
        block_on(async {
            let snapshot = snapshot_with_limits(ExecutionLimits {
                hydration_limit: 1,
                ..ExecutionLimits::default()
            });
            let one = hydrate_selected(&FixedPayloadHydrator, &snapshot, &[anchor("only")])
                .await
                .expect("one record");
            assert_eq!(one.available.len(), 1);

            assert_eq!(
                hydrate_selected(
                    &FixedPayloadHydrator,
                    &snapshot,
                    &[anchor("first"), anchor("second")],
                )
                .await,
                Err(HydrationError::BudgetExceeded {
                    resource: "record count"
                })
            );
        });
    }

    #[test]
    fn hydration_checkpoints_between_anchors() {
        block_on(async {
            struct CancelAfterFirstRead {
                control: ExecutionControl,
                reads: AtomicUsize,
            }

            impl TemporalHydrationPort for CancelAfterFirstRead {
                fn authorize_hydration<'a>(
                    &'a self,
                    _snapshot: &'a TemporalExecutionSnapshot,
                    _anchor_id: &'a RetrievalAnchorId,
                ) -> HydrationFuture<'a, HydrationAuthorization> {
                    Box::pin(async { Ok(HydrationAuthorization::Authorized) })
                }

                fn read_authorized<'a>(
                    &'a self,
                    _grant: &'a HydrationGrant<'_>,
                    sink: &'a mut HydrationSink<'_>,
                ) -> HydrationFuture<'a, ()> {
                    Box::pin(async move {
                        sink.write_chunk(b"ok")?;
                        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                            self.control.cancel();
                        }
                        Ok(())
                    })
                }
            }

            let control = ExecutionControl::default();
            let session_id: SessionId =
                serde_json::from_str("\"session-1\"").expect("valid session id");
            let snap = TemporalExecutionSnapshot::new_authorized(
                TemporalSnapshotRequest::new(
                    session_id,
                    digest('0'),
                    digest('1'),
                    digest('2'),
                    TemporalModeV1::Current,
                    RetrievalGrainV1::LogicalMessage,
                )
                .expect("valid request")
                .with_limits(ExecutionLimits::default())
                .with_execution_control(control.clone()),
                TemporalWatermarks {
                    generation: 1,
                    source: 2,
                    projection: 3,
                    index: 4,
                    summary: 5,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                        .expect("valid digest"),
                },
                None,
                ValidatedAuthorization::Authorized,
            )
            .expect("valid snapshot");
            let hydrator = CancelAfterFirstRead {
                control,
                reads: AtomicUsize::new(0),
            };

            assert_eq!(
                hydrate_selected(&hydrator, &snap, &[anchor("first"), anchor("second")],).await,
                Err(HydrationError::Interrupted(TemporalPortError::Cancelled))
            );
            assert_eq!(hydrator.reads.load(Ordering::SeqCst), 1);
        });
    }
}
