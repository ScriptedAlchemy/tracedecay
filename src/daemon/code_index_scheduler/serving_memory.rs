use std::num::NonZeroU64;
#[cfg(test)]
use std::sync::atomic::AtomicU8;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::{ComponentRevision, ExactAdmissionRuleRevision, ScoreDomainId};
use tracedecay_query::retrieval::exact::{CentralExactAdmissionAuthorityV1, ExactLane};
use tracedecay_query::retrieval::graph::{CodeGraphEvidenceAdapterV1, GraphLane};
use tracedecay_query::retrieval::lexical::{
    CodeExactProjectionAdapterV1, CodeLexicalProjectionAdapterV1, CodeLexicalProjectionMetadataV1,
    LexicalLane, code_lexical_ngram_resident_upper_bound_v1,
};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_runtime_core::resident_memory::{
    ResidentMemoryComponentIdV1, ResidentMemoryKeyV1, ResidentMemoryReservationV1,
};

use super::{LatestCompleteCodeIndexV1, record_index::GenerationRecordIndexV1};

pub(super) struct ResidentReadyV1<T> {
    value: T,
    _reservation: ResidentMemoryReservationV1,
}

#[derive(Default)]
pub(super) struct ServingWarmControlV1 {
    cancelled: AtomicBool,
}

impl ServingWarmControlV1 {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn checkpoint(&self) -> Result<(), RetrievalPortError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(RetrievalPortError::Cancelled)
        } else {
            Ok(())
        }
    }
}

struct ServingLaneCellV1<T> {
    ready: OnceLock<Arc<T>>,
    build_gate: Mutex<()>,
}

impl<T> ServingLaneCellV1<T> {
    fn new() -> Self {
        Self {
            ready: OnceLock::new(),
            build_gate: Mutex::new(()),
        }
    }

    fn get_or_try_init(
        &self,
        build: impl FnOnce() -> Result<T, RetrievalPortError>,
    ) -> Result<&T, RetrievalPortError> {
        if let Some(ready) = self.ready.get() {
            return Ok(ready.as_ref());
        }
        let _build = self
            .build_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ready) = self.ready.get() {
            return Ok(ready.as_ref());
        }
        let ready = Arc::new(build()?);
        let _ = self.ready.set(ready);
        self.ready.get().map(Arc::as_ref).ok_or_else(|| {
            RetrievalPortError::AuthorityUnavailable(
                "serving lane completed without publishing its ready value".to_owned(),
            )
        })
    }

    fn get(&self, lane: &'static str) -> Result<&T, RetrievalPortError> {
        self.ready.get().map(Arc::as_ref).ok_or_else(|| {
            RetrievalPortError::AuthorityUnavailable(format!("{lane} serving lane is warming"))
        })
    }

    fn is_ready(&self) -> bool {
        self.ready.get().is_some()
    }
}

pub(super) struct GenerationServingCachesV1 {
    pub(super) serving: Arc<OnceLock<Arc<ProductionCodeIndexQueryOwnersV1>>>,
    pub(super) build_gate: Arc<Mutex<()>>,
    pub(super) control: Arc<ServingWarmControlV1>,
}

impl GenerationServingCachesV1 {
    pub(super) fn new() -> Self {
        Self {
            serving: Arc::new(OnceLock::new()),
            build_gate: Arc::new(Mutex::new(())),
            control: Arc::new(ServingWarmControlV1::default()),
        }
    }

    pub(super) fn cancel(&self) {
        self.control.cancel();
    }
}

struct ExactLexicalOwnersV1 {
    exact: ExactLane<
        CentralExactAdmissionAuthorityV1,
        CodeExactProjectionAdapterV1<CentralExactAdmissionAuthorityV1>,
    >,
    lexical: LexicalLane<CodeLexicalProjectionAdapterV1>,
}

#[derive(Clone)]
pub(super) struct ProductionCodeIndexQueryOwnersV1 {
    generation: Arc<super::ResidentPublishedGenerationV1>,
    control: Arc<ServingWarmControlV1>,
    record_index: Arc<ServingLaneCellV1<ResidentReadyV1<GenerationRecordIndexV1>>>,
    exact_lexical: Arc<ServingLaneCellV1<ResidentReadyV1<ExactLexicalOwnersV1>>>,
    exact: Arc<ServingLaneCellV1<()>>,
    lexical: Arc<ServingLaneCellV1<()>>,
    graph: Arc<ServingLaneCellV1<ResidentReadyV1<GraphLane<CodeGraphEvidenceAdapterV1>>>>,
    #[cfg(test)]
    faulted_lanes: Arc<AtomicU8>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum ServingLaneV1 {
    RecordIndex = 1,
    Exact = 2,
    Lexical = 4,
    Graph = 8,
}

impl ProductionCodeIndexQueryOwnersV1 {
    #[cfg(test)]
    pub(super) fn fail_next(&self, lane: ServingLaneV1) {
        self.faulted_lanes.fetch_or(lane as u8, Ordering::AcqRel);
    }

    #[cfg(test)]
    fn lane_checkpoint(&self, lane: ServingLaneV1) -> Result<(), RetrievalPortError> {
        let mask = lane as u8;
        let previous = self.faulted_lanes.fetch_and(!mask, Ordering::AcqRel);
        if previous & mask == 0 {
            Ok(())
        } else {
            Err(RetrievalPortError::AuthorityUnavailable(
                "injected serving-lane warm failure".to_owned(),
            ))
        }
    }

    pub(super) fn exact(
        &self,
    ) -> Result<
        &ExactLane<
            CentralExactAdmissionAuthorityV1,
            CodeExactProjectionAdapterV1<CentralExactAdmissionAuthorityV1>,
        >,
        RetrievalPortError,
    > {
        self.exact.get("exact")?;
        Ok(&self.exact_lexical.get("exact")?.value.exact)
    }

    fn warm_exact(&self) -> Result<(), RetrievalPortError> {
        self.exact.get_or_try_init(|| {
            #[cfg(test)]
            self.lane_checkpoint(ServingLaneV1::Exact)?;
            self.control.checkpoint()?;
            let _ = self.exact_lexical_owners()?;
            self.control.checkpoint()?;
            Ok(())
        })?;
        Ok(())
    }

    pub(super) fn lexical(
        &self,
    ) -> Result<&LexicalLane<CodeLexicalProjectionAdapterV1>, RetrievalPortError> {
        self.lexical.get("lexical")?;
        Ok(&self.exact_lexical.get("lexical")?.value.lexical)
    }

    fn warm_lexical(&self) -> Result<(), RetrievalPortError> {
        self.lexical.get_or_try_init(|| {
            #[cfg(test)]
            self.lane_checkpoint(ServingLaneV1::Lexical)?;
            self.control.checkpoint()?;
            let _ = self.exact_lexical_owners()?;
            self.control.checkpoint()?;
            Ok(())
        })?;
        Ok(())
    }

    pub(super) fn graph(
        &self,
    ) -> Result<&GraphLane<CodeGraphEvidenceAdapterV1>, RetrievalPortError> {
        self.graph.get("graph").map(|ready| &ready.value)
    }

    fn warm_graph(&self) -> Result<(), RetrievalPortError> {
        self.graph
            .get_or_try_init(|| self.build_graph_owner())
            .map(|_| ())
    }

    fn record_index(&self) -> Result<&GenerationRecordIndexV1, RetrievalPortError> {
        self.record_index
            .get("record-index")
            .map(|ready| &ready.value)
    }

    fn warm_record_index(&self) -> Result<(), RetrievalPortError> {
        self.record_index
            .get_or_try_init(|| self.build_record_index())
            .map(|_| ())
    }

    fn build_record_index(
        &self,
    ) -> Result<ResidentReadyV1<GenerationRecordIndexV1>, RetrievalPortError> {
        #[cfg(test)]
        self.lane_checkpoint(ServingLaneV1::RecordIndex)?;
        self.control.checkpoint()?;
        let record_entries = self
            .generation
            .chunks()
            .chunks()
            .len()
            .checked_mul(3)
            .and_then(|entries| entries.checked_add(self.generation.snapshot().files.len()))
            .and_then(|entries| entries.checked_add(self.generation.symbols().symbols.len()))
            .and_then(|entries| {
                self.generation
                    .edges()
                    .len()
                    .checked_mul(2)
                    .and_then(|edges| entries.checked_add(edges))
            })
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "record-index resident entry count exceeds usize".to_owned(),
                )
            })?;
        let record_bytes = conservative_lane_reservation(
            self.generation.sealed_bytes,
            record_entries,
            512,
            8 * 1024 * 1024,
        )?;
        let reservation =
            self.reserve_serving_component("code_index.serving_record_index.v1", record_bytes)?;
        let value = GenerationRecordIndexV1::build(self.generation.as_ref(), &self.control)?;
        self.control.checkpoint()?;
        Ok(ResidentReadyV1 {
            value,
            _reservation: reservation,
        })
    }

    fn exact_lexical_owners(&self) -> Result<&ExactLexicalOwnersV1, RetrievalPortError> {
        self.exact_lexical
            .get_or_try_init(|| self.build_exact_lexical_owners())
            .map(|ready| &ready.value)
    }

    fn build_exact_lexical_owners(
        &self,
    ) -> Result<ResidentReadyV1<ExactLexicalOwnersV1>, RetrievalPortError> {
        self.control.checkpoint()?;
        let requested_bytes = conservative_exact_lexical_reservation(
            self.generation.sealed_bytes,
            self.generation.chunks().chunks().len(),
        )?;
        let reservation =
            self.reserve_serving_component("code_index.serving_exact_lexical.v1", requested_bytes)?;
        let generation_id = self.generation.manifest().generation_id.clone();
        let freshness = tracedecay_query::retrieval::graph::production_code_index_freshness(
            self.generation.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let metadata = CodeLexicalProjectionMetadataV1 {
            generation: generation_id,
            repository_id: Some(self.generation.snapshot().repository.clone()),
            logical_paths: self
                .generation
                .snapshot()
                .files
                .iter()
                .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
                .collect(),
            freshness: freshness.clone(),
            exact_retriever_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RETRIEVER_REVISION_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            lexical_retriever_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_RETRIEVER_REVISION_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
            exact_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        };
        let admitted = self
            .generation
            .admitted_shared_chunks()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        let projection = CodeLexicalProjectionAdapterV1::new_admitted(metadata, admitted)?;
        let authority = CentralExactAdmissionAuthorityV1::new(
            ExactAdmissionRuleRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        );
        let exact = ExactLane::new(authority.clone(), projection.exact_adapter(authority));
        let lexical = LexicalLane::new(projection);
        self.control.checkpoint()?;
        Ok(ResidentReadyV1 {
            value: ExactLexicalOwnersV1 { exact, lexical },
            _reservation: reservation,
        })
    }

    fn build_graph_owner(
        &self,
    ) -> Result<ResidentReadyV1<GraphLane<CodeGraphEvidenceAdapterV1>>, RetrievalPortError> {
        #[cfg(test)]
        self.lane_checkpoint(ServingLaneV1::Graph)?;
        self.control.checkpoint()?;
        let graph_entries = self
            .generation
            .edges()
            .len()
            .checked_add(self.generation.symbols().symbols.len())
            .ok_or_else(|| {
                RetrievalPortError::Contract("graph resident entry count exceeds usize".to_owned())
            })?;
        let requested_bytes = conservative_lane_reservation(
            self.generation.sealed_bytes,
            graph_entries,
            512,
            8 * 1024 * 1024,
        )?;
        let reservation =
            self.reserve_serving_component("code_index.serving_graph.v1", requested_bytes)?;
        let freshness = tracedecay_query::retrieval::graph::production_code_index_freshness(
            self.generation.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let value = GraphLane::new(CodeGraphEvidenceAdapterV1::new_shared(
            self.generation.manifest().generation_id.clone(),
            Some(self.generation.snapshot().repository.clone()),
            freshness,
            self.generation.shared_edges(),
            self.generation.chunks().shared_chunks(),
        )?);
        self.control.checkpoint()?;
        Ok(ResidentReadyV1 {
            value,
            _reservation: reservation,
        })
    }

    fn reserve_serving_component(
        &self,
        component: &'static str,
        requested_bytes: u64,
    ) -> Result<ResidentMemoryReservationV1, RetrievalPortError> {
        self.control.checkpoint()?;
        let requested_bytes = NonZeroU64::new(requested_bytes).ok_or_else(|| {
            RetrievalPortError::Contract("serving resident reservation is zero".to_owned())
        })?;
        let component = ResidentMemoryComponentIdV1::new(component)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        self.generation
            .resident_memory
            .reserve(
                ResidentMemoryKeyV1 {
                    project_id: self.generation.project_id.clone(),
                    worktree_id: self.generation.worktree_id.clone(),
                    generation_id: self.generation.manifest().generation_id.clone(),
                    component,
                },
                requested_bytes,
            )
            .map_err(|error| RetrievalPortError::AuthorityUnavailable(error.to_string()))
    }
}

pub(super) fn conservative_lane_reservation(
    payload_bytes: u64,
    entries: usize,
    bytes_per_entry: u64,
    fixed_bytes: u64,
) -> Result<u64, RetrievalPortError> {
    let entries = u64::try_from(entries)
        .map_err(|_| RetrievalPortError::Contract("serving entry count exceeds u64".to_owned()))?;
    payload_bytes
        .checked_add(entries.checked_mul(bytes_per_entry).ok_or_else(|| {
            RetrievalPortError::Contract("serving entry reservation exceeds u64".to_owned())
        })?)
        .and_then(|bytes| bytes.checked_add(fixed_bytes))
        .and_then(NonZeroU64::new)
        .map(NonZeroU64::get)
        .ok_or_else(|| {
            RetrievalPortError::Contract("serving resident reservation exceeds u64".to_owned())
        })
}

fn conservative_exact_lexical_reservation(
    sealed_bytes: u64,
    chunks: usize,
) -> Result<u64, RetrievalPortError> {
    let ngram_bytes = code_lexical_ngram_resident_upper_bound_v1(sealed_bytes);
    let retained_payload = sealed_bytes
        .checked_mul(8)
        .and_then(|bytes| bytes.checked_add(ngram_bytes))
        .ok_or_else(|| {
            RetrievalPortError::Contract(
                "exact/lexical resident reservation exceeds u64".to_owned(),
            )
        })?;
    conservative_lane_reservation(retained_payload, chunks, 4_096, 8 * 1024 * 1024)
}

impl LatestCompleteCodeIndexV1 {
    pub(super) fn record_index(&self) -> Result<&GenerationRecordIndexV1, RetrievalPortError> {
        let _ = self.production_query_owners()?;
        self.serving
            .get()
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(
                    "serving registry was not published".to_owned(),
                )
            })?
            .record_index()
    }

    pub(in crate::daemon) fn warm_serving_caches(&self) -> Result<(), RetrievalPortError> {
        let owners = self.production_query_owners()?;
        std::thread::scope(|scope| {
            let record = scope.spawn(|| owners.warm_record_index());
            let exact_lexical = scope.spawn(|| {
                let exact = owners.warm_exact();
                let lexical = owners.warm_lexical();
                exact.and(lexical)
            });
            let graph = scope.spawn(|| owners.warm_graph());
            let join = |result: std::thread::Result<Result<(), RetrievalPortError>>| {
                result.map_err(|_| {
                    RetrievalPortError::AuthorityUnavailable(
                        "serving lane warm worker terminated".to_owned(),
                    )
                })?
            };
            let record = join(record.join());
            let exact_lexical = join(exact_lexical.join());
            let graph = join(graph.join());
            record.and(exact_lexical).and(graph)
        })
    }

    #[cfg(test)]
    pub(super) fn record_index_is_warm(&self) -> bool {
        self.serving
            .get()
            .is_some_and(|owners| owners.record_index.is_ready())
    }

    #[cfg(test)]
    pub(super) fn query_owners_are_warm(&self) -> bool {
        self.serving_lanes_are_ready()
    }

    pub(super) fn serving_lanes_are_ready(&self) -> bool {
        self.serving.get().is_some_and(|owners| {
            owners.record_index.is_ready()
                && owners.exact.is_ready()
                && owners.lexical.is_ready()
                && owners.graph.is_ready()
        })
    }

    pub(super) fn production_query_owners(
        &self,
    ) -> Result<Arc<ProductionCodeIndexQueryOwnersV1>, RetrievalPortError> {
        if let Some(owners) = self.serving.get() {
            return Ok(Arc::clone(owners));
        }
        let _publish = self
            .query_owners_build_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owners) = self.serving.get() {
            return Ok(Arc::clone(owners));
        }
        self.warm_control.checkpoint()?;
        let owners = Arc::new(ProductionCodeIndexQueryOwnersV1 {
            generation: Arc::clone(&self.generation),
            control: Arc::clone(&self.warm_control),
            record_index: Arc::new(ServingLaneCellV1::new()),
            exact_lexical: Arc::new(ServingLaneCellV1::new()),
            exact: Arc::new(ServingLaneCellV1::new()),
            lexical: Arc::new(ServingLaneCellV1::new()),
            graph: Arc::new(ServingLaneCellV1::new()),
            #[cfg(test)]
            faulted_lanes: Arc::new(AtomicU8::new(0)),
        });
        let _ = self.serving.set(Arc::clone(&owners));
        Ok(self.serving.get().map(Arc::clone).unwrap_or(owners))
    }
}
