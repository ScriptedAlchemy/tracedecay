use std::num::NonZeroU64;
use std::ops::Deref;
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

use super::{LatestCompleteCodeIndexV1, queries};

pub(super) struct ResidentReadyV1<T> {
    pub(super) value: T,
    pub(super) _reservation: ResidentMemoryReservationV1,
}

impl<T> Deref for ResidentReadyV1<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
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

pub(super) struct ExactLexicalOwnersV1 {
    pub(super) exact: ExactLane<
        CentralExactAdmissionAuthorityV1,
        CodeExactProjectionAdapterV1<CentralExactAdmissionAuthorityV1>,
    >,
    pub(super) lexical: LexicalLane<CodeLexicalProjectionAdapterV1>,
}

pub(super) struct GenerationServingCachesV1 {
    pub(super) query_owners: Arc<OnceLock<Arc<ProductionCodeIndexQueryOwnersV1>>>,
    pub(super) exact_lexical: Arc<OnceLock<Arc<ResidentReadyV1<ExactLexicalOwnersV1>>>>,
    pub(super) graph: Arc<OnceLock<Arc<ResidentReadyV1<GraphLane<CodeGraphEvidenceAdapterV1>>>>>,
    pub(super) record_index: Arc<OnceLock<Arc<ResidentReadyV1<queries::GenerationRecordIndexV1>>>>,
    pub(super) build_gate: Arc<Mutex<()>>,
    pub(super) control: Arc<ServingWarmControlV1>,
}

impl GenerationServingCachesV1 {
    pub(super) fn new() -> Self {
        Self {
            query_owners: Arc::new(OnceLock::new()),
            exact_lexical: Arc::new(OnceLock::new()),
            graph: Arc::new(OnceLock::new()),
            record_index: Arc::new(OnceLock::new()),
            build_gate: Arc::new(Mutex::new(())),
            control: Arc::new(ServingWarmControlV1::default()),
        }
    }

    pub(super) fn cancel(&self) {
        self.control.cancel();
    }
}

#[derive(Clone)]
pub(super) struct ProductionCodeIndexQueryOwnersV1 {
    pub(super) exact_lexical: Arc<ResidentReadyV1<ExactLexicalOwnersV1>>,
    pub(super) graph: Arc<ResidentReadyV1<GraphLane<CodeGraphEvidenceAdapterV1>>>,
}

impl ProductionCodeIndexQueryOwnersV1 {
    pub(super) fn exact(
        &self,
    ) -> &ExactLane<
        CentralExactAdmissionAuthorityV1,
        CodeExactProjectionAdapterV1<CentralExactAdmissionAuthorityV1>,
    > {
        &self.exact_lexical.exact
    }

    pub(super) fn lexical(&self) -> &LexicalLane<CodeLexicalProjectionAdapterV1> {
        &self.exact_lexical.lexical
    }

    pub(super) fn graph(&self) -> &GraphLane<CodeGraphEvidenceAdapterV1> {
        &self.graph
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
    pub(in crate::daemon) fn record_index(
        &self,
    ) -> Result<&queries::GenerationRecordIndexV1, RetrievalPortError> {
        self.ensure_serving_ready()?;
        self.record_index
            .get()
            .map(|ready| &ready.value)
            .ok_or_else(|| {
                RetrievalPortError::AuthorityUnavailable(
                    "record-index warm completed without a ready value".to_owned(),
                )
            })
    }

    pub(in crate::daemon) fn warm_serving_caches(&self) -> Result<(), RetrievalPortError> {
        self.warm_control.checkpoint()?;
        let _ = self.generation.admitted_chunks();
        let _ = self.generation.test_attribution_authority();
        self.ensure_serving_ready()?;
        self.warm_control.checkpoint()
    }

    #[cfg(test)]
    pub(super) fn record_index_is_warm(&self) -> bool {
        self.record_index.get().is_some()
    }

    #[cfg(test)]
    pub(super) fn query_owners_are_warm(&self) -> bool {
        self.query_owners.get().is_some()
    }

    pub(super) fn production_query_owners(
        &self,
    ) -> Result<Arc<ProductionCodeIndexQueryOwnersV1>, RetrievalPortError> {
        self.ensure_serving_ready()?;
        self.query_owners.get().map(Arc::clone).ok_or_else(|| {
            RetrievalPortError::AuthorityUnavailable(
                "serving warm completed without ready query owners".to_owned(),
            )
        })
    }

    fn ensure_serving_ready(&self) -> Result<(), RetrievalPortError> {
        if self.serving_ready() {
            return Ok(());
        }
        let _build = self
            .query_owners_build_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.serving_ready() {
            return Ok(());
        }
        self.warm_control.checkpoint()?;

        let record_entries = self
            .generation
            .snapshot()
            .files
            .len()
            .saturating_add(self.generation.chunks().chunks().len().saturating_mul(3))
            .saturating_add(self.generation.symbols().symbols.len())
            .saturating_add(self.generation.edges().len().saturating_mul(2));
        let record_reservation = self.reserve_serving_component(
            "code_index.record_index.v1",
            conservative_lane_reservation(
                self.generation.sealed_bytes,
                record_entries,
                512,
                8 * 1024 * 1024,
            )?,
        )?;
        let record_index =
            queries::GenerationRecordIndexV1::build(self.generation.as_ref(), &self.warm_control)?;

        let exact_lexical_reservation = self.reserve_serving_component(
            "code_index.exact_lexical.v1",
            conservative_exact_lexical_reservation(
                self.generation.sealed_bytes,
                self.generation.chunks().chunks().len(),
            )?,
        )?;
        let exact_lexical_value = self.build_exact_lexical_owners()?;

        let graph_entries = self
            .generation
            .edges()
            .len()
            .saturating_add(self.generation.symbols().symbols.len());
        let graph_reservation = self.reserve_serving_component(
            "code_index.graph.v1",
            conservative_lane_reservation(
                self.generation.sealed_bytes,
                graph_entries,
                512,
                8 * 1024 * 1024,
            )?,
        )?;
        let graph_value = self.build_graph_owner()?;
        self.warm_control.checkpoint()?;

        let record_index = Arc::new(ResidentReadyV1 {
            value: record_index,
            _reservation: record_reservation,
        });
        let exact_lexical = Arc::new(ResidentReadyV1 {
            value: exact_lexical_value,
            _reservation: exact_lexical_reservation,
        });
        let graph = Arc::new(ResidentReadyV1 {
            value: graph_value,
            _reservation: graph_reservation,
        });
        let owners = Arc::new(ProductionCodeIndexQueryOwnersV1 {
            exact_lexical: Arc::clone(&exact_lexical),
            graph: Arc::clone(&graph),
        });
        self.record_index.get_or_init(|| record_index);
        self.exact_lexical.get_or_init(|| exact_lexical);
        self.graph.get_or_init(|| graph);
        self.query_owners.get_or_init(|| owners);
        if !self.serving_ready() {
            return Err(RetrievalPortError::AuthorityUnavailable(
                "serving warm failed to publish its complete resident set".to_owned(),
            ));
        }
        Ok(())
    }

    fn serving_ready(&self) -> bool {
        self.record_index.get().is_some()
            && self.exact_lexical.get().is_some()
            && self.graph.get().is_some()
            && self.query_owners.get().is_some()
    }

    fn build_exact_lexical_owners(&self) -> Result<ExactLexicalOwnersV1, RetrievalPortError> {
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
        let lexical_projection = CodeLexicalProjectionAdapterV1::new_admitted(metadata, admitted)?;
        let authority = CentralExactAdmissionAuthorityV1::new(
            ExactAdmissionRuleRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        );
        let exact = ExactLane::new(
            authority.clone(),
            lexical_projection.exact_adapter(authority),
        );
        let lexical = LexicalLane::new(lexical_projection);
        Ok(ExactLexicalOwnersV1 { exact, lexical })
    }

    fn build_graph_owner(
        &self,
    ) -> Result<GraphLane<CodeGraphEvidenceAdapterV1>, RetrievalPortError> {
        let freshness = tracedecay_query::retrieval::graph::production_code_index_freshness(
            self.generation.manifest().seal.sealed_at,
            ComponentRevision::new("policy.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        Ok(GraphLane::new(CodeGraphEvidenceAdapterV1::new_shared(
            self.generation.manifest().generation_id.clone(),
            Some(self.generation.snapshot().repository.clone()),
            freshness,
            self.generation.shared_edges(),
            self.generation.chunks().shared_chunks(),
        )?))
    }

    fn reserve_serving_component(
        &self,
        component: &'static str,
        requested_bytes: u64,
    ) -> Result<ResidentMemoryReservationV1, RetrievalPortError> {
        self.warm_control.checkpoint()?;
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
