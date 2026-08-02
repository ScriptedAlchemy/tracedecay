//! Production query-embedding adapter over the reloadable session service.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::{CodeGenerationId, ProjectionKeyV1, VectorGenerationIdV1};
use tracedecay_query::retrieval::ports::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    EphemeralQueryEmbeddingV1, SemanticQueryEmbeddingPort, SemanticQueryEmbeddingRequestV1,
};

use super::fastembed_adapter::{
    BoundedSanitizedTextBatchV1, CancellationSignal, EmbedError, EmbeddingRuntime, EmbeddingSession,
};
use super::runtime_service::{SemanticGenerationPointerV1, SemanticRuntimeService};
use super::session_pool::SessionAcquireError;

/// Owned factory for request-scoped query embedders.
pub struct PooledSemanticQueryEmbedderFactory<R: EmbeddingRuntime> {
    runtime: Arc<SemanticRuntimeService<R>>,
    query_in_flight: Arc<AtomicBool>,
}

impl<R> PooledSemanticQueryEmbedderFactory<R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    pub fn new(runtime: Arc<SemanticRuntimeService<R>>) -> Arc<Self> {
        Self::new_with_admission(runtime, Arc::new(AtomicBool::new(false)))
    }

    pub fn new_with_admission(
        runtime: Arc<SemanticRuntimeService<R>>,
        query_in_flight: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            query_in_flight,
        })
    }

    pub fn runtime(&self) -> &Arc<SemanticRuntimeService<R>> {
        &self.runtime
    }

    pub fn create<'a>(
        self: &Arc<Self>,
        cancellation: Arc<dyn CancellationSignal + 'a>,
    ) -> PooledSemanticQueryEmbedder<'a, R> {
        PooledSemanticQueryEmbedder {
            factory: Arc::clone(self),
            cancellation,
        }
    }

    fn try_query_permit(&self) -> Option<SemanticQueryPermitV1<'_>> {
        self.query_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| SemanticQueryPermitV1 {
                in_flight: &self.query_in_flight,
            })
    }
}

struct SemanticQueryPermitV1<'a> {
    in_flight: &'a AtomicBool,
}

impl Drop for SemanticQueryPermitV1<'_> {
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

/// Query runtime paired with the exact atomically published semantic pointer.
///
/// A warmed runtime prepared for a future generation is never returned until
/// all three request identities match the scheduler's current pointer.
pub struct CurrentSemanticQueryRuntimeV1<R: EmbeddingRuntime> {
    pointer: SemanticGenerationPointerV1,
    factory: Arc<PooledSemanticQueryEmbedderFactory<R>>,
}

impl<R> CurrentSemanticQueryRuntimeV1<R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    #[cfg(test)]
    pub fn new(
        pointer: SemanticGenerationPointerV1,
        runtime: Arc<SemanticRuntimeService<R>>,
    ) -> Self {
        Self::new_with_admission(pointer, runtime, Arc::new(AtomicBool::new(false)))
    }

    pub fn new_with_admission(
        pointer: SemanticGenerationPointerV1,
        runtime: Arc<SemanticRuntimeService<R>>,
        query_in_flight: Arc<AtomicBool>,
    ) -> Self {
        Self {
            pointer,
            factory: PooledSemanticQueryEmbedderFactory::new_with_admission(
                runtime,
                query_in_flight,
            ),
        }
    }

    pub fn factory_for(
        &self,
        source_generation: &CodeGenerationId,
        vector_generation: &VectorGenerationIdV1,
        projection_key: &ProjectionKeyV1,
    ) -> Option<Arc<PooledSemanticQueryEmbedderFactory<R>>> {
        let (_, authority, _) = self.factory.runtime().active_snapshot();
        (self.pointer.source_generation == *source_generation
            && self.pointer.generation == *vector_generation
            && self.pointer.projection_key == *projection_key
            && authority.projection().projection_key() == projection_key)
            .then(|| Arc::clone(&self.factory))
    }
}

/// Request-scoped adapter that obtains one bounded warmed session and emits
/// exactly one ephemeral query vector.
pub struct PooledSemanticQueryEmbedder<'a, R: EmbeddingRuntime> {
    factory: Arc<PooledSemanticQueryEmbedderFactory<R>>,
    cancellation: Arc<dyn CancellationSignal + 'a>,
}

impl<R> SemanticQueryEmbeddingPort for PooledSemanticQueryEmbedder<'_, R>
where
    R: EmbeddingRuntime + Send + Sync + 'static,
{
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError> {
        let (_generation, authority, pool) = self.factory.runtime.active_snapshot();
        if authority.projection() != request.projection {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        if request.query_digest.privacy_domain != *request.projection.privacy_domain()
            || request.query_digest.key_epoch != request.projection.privacy_key_epoch()
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        if self.cancellation.cancelled() {
            return Err(RetrievalPortError::Cancelled);
        }

        let max_query_bytes = authority.max_batch_bytes() as usize;
        if request.query_view.as_bytes().len() > max_query_bytes {
            return Err(RetrievalPortError::BudgetExceeded);
        }
        let text = request.query_view.as_str().to_owned();
        let batch = BoundedSanitizedTextBatchV1::try_new(vec![text], 1, max_query_bytes)
            .map_err(map_embed_error)?;
        // ORT inference cannot be preempted once entered. Admit only one
        // request per published runtime so a timed-out caller cannot cause
        // concurrent cold session opens and multiply resident model memory.
        let _query_permit = self.factory.try_query_permit().ok_or_else(|| {
            RetrievalPortError::AuthorityUnavailable("semantic query already in flight".to_owned())
        })?;
        let mut session = pool.acquire(&authority).map_err(map_acquire_error)?;
        let mut vectors = session
            .embed_batch(&batch, self.cancellation.as_ref())
            .map_err(map_embed_error)?;
        if vectors.len() != 1 {
            return Err(RetrievalPortError::Contract(
                "query embedding runtime returned a non-unit batch".to_owned(),
            ));
        }
        let vector = vectors
            .pop()
            .unwrap_or_else(|| panic!("unit query embedding batch"));
        vector.validate().map_err(map_embed_error)?;
        EphemeralQueryEmbeddingV1::new(
            request.query_digest.clone(),
            request.projection.clone(),
            vector.values,
        )
    }
}

fn map_acquire_error(error: SessionAcquireError) -> RetrievalPortError {
    match error {
        SessionAcquireError::Cancelled => RetrievalPortError::Cancelled,
        SessionAcquireError::DeadlineExceeded { .. } => RetrievalPortError::BudgetExceeded,
        SessionAcquireError::Open(error) => map_embed_error(error),
        SessionAcquireError::Exhausted { .. }
        | SessionAcquireError::QueueFull { .. }
        | SessionAcquireError::MemoryCeilingExceeded { .. }
        | SessionAcquireError::Closed => {
            RetrievalPortError::AuthorityUnavailable("semantic runtime unavailable".to_owned())
        }
    }
}

fn map_embed_error(error: EmbedError) -> RetrievalPortError {
    match error {
        EmbedError::Cancelled => RetrievalPortError::Cancelled,
        EmbedError::DimensionMismatch { .. } | EmbedError::NonFiniteVectorValue => {
            RetrievalPortError::IncompatibleProjection
        }
        EmbedError::BatchBytesExceeded { .. } => RetrievalPortError::BudgetExceeded,
        EmbedError::EmptyBatch | EmbedError::TooManyTexts { .. } => {
            RetrievalPortError::Contract("bounded query embedding was rejected".to_owned())
        }
        EmbedError::Runtime(_) => {
            RetrievalPortError::AuthorityUnavailable("semantic runtime unavailable".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_domain::{
        CodeGenerationId, EphemeralSanitizedQueryViewV1, ManifestDigest, QueryDigest, QueryMac,
        QueryNormalizationRevision, SanitizerRevision, VectorGenerationIdV1,
    };

    use super::super::fastembed_adapter::{FakeEmbeddingRuntime, ManualCancellation};
    use super::super::runtime_service::{SemanticRuntimeService, SharedEmbeddingRuntimeFactory};
    use super::super::session_pool::test_support::{authority, config};
    use super::*;
    use tracedecay_query::retrieval::semantic::{
        SemanticQueryEmbeddingPort, SemanticQueryEmbeddingRequestV1,
    };

    fn domain_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical domain fixture identity")
    }

    #[test]
    fn query_factory_requires_the_atomically_current_compatible_generation() {
        let authority = Arc::new(authority());
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            config(1, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        let source_generation =
            CodeGenerationId::new("code-generation.current".to_owned()).expect("source generation");
        let vector_generation = VectorGenerationIdV1::new(
            ManifestDigest::new(format!("sha256:{}", "ab".repeat(32)))
                .expect("vector generation digest"),
        );
        let current = CurrentSemanticQueryRuntimeV1::new(
            SemanticGenerationPointerV1 {
                generation: vector_generation.clone(),
                source_generation: source_generation.clone(),
                projection_key: authority.projection().projection_key().clone(),
            },
            service,
        );

        assert!(
            current
                .factory_for(
                    &source_generation,
                    &vector_generation,
                    authority.projection().projection_key(),
                )
                .is_some()
        );
        assert!(
            current
                .factory_for(
                    &CodeGenerationId::new("code-generation.stale".to_owned())
                        .expect("stale source generation"),
                    &vector_generation,
                    authority.projection().projection_key(),
                )
                .is_none()
        );
    }

    #[test]
    fn owned_query_embedder_uses_the_pooled_runtime_interface() {
        let authority = Arc::new(authority());
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            config(1, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        let embedder_factory = PooledSemanticQueryEmbedderFactory::new(service);
        let embedder = embedder_factory.create(Arc::new(ManualCancellation::new()));
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "find session acquisition",
            domain_id::<SanitizerRevision>("sanitizer.v1"),
            domain_id::<QueryNormalizationRevision>("normalizer.v1"),
        )
        .expect("bounded query");
        let query_digest = QueryDigest::new(
            authority.projection().privacy_domain().clone(),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "11".repeat(32))).expect("query MAC"),
        );
        let request = SemanticQueryEmbeddingRequestV1 {
            query_digest: &query_digest,
            query_view: &query_view,
            projection: authority.projection(),
        };

        let _first = embedder.embed_query(request).expect("first embedding");
        let _second = embedder.embed_query(request).expect("second embedding");
        assert_eq!(
            embedder_factory.runtime().stats().sessions_opened,
            1,
            "the production adapter path reuses one warmed session"
        );
    }

    #[test]
    fn concurrent_query_abstains_across_runtime_factory_rotation() {
        let authority = Arc::new(authority());
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let first_service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            Arc::clone(&factory),
            config(2, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        let second_service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            config(2, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("replacement runtime service");
        let admission = Arc::new(AtomicBool::new(false));
        let first_factory = PooledSemanticQueryEmbedderFactory::new_with_admission(
            first_service,
            Arc::clone(&admission),
        );
        let second_factory = PooledSemanticQueryEmbedderFactory::new_with_admission(
            Arc::clone(&second_service),
            admission,
        );
        let held = first_factory
            .try_query_permit()
            .expect("first query permit");
        let embedder = second_factory.create(Arc::new(ManualCancellation::new()));
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "do not multiply model sessions",
            domain_id::<SanitizerRevision>("sanitizer.v1"),
            domain_id::<QueryNormalizationRevision>("normalizer.v1"),
        )
        .expect("bounded query");
        let query_digest = QueryDigest::new(
            authority.projection().privacy_domain().clone(),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "12".repeat(32))).expect("query MAC"),
        );
        let request = SemanticQueryEmbeddingRequestV1 {
            query_digest: &query_digest,
            query_view: &query_view,
            projection: authority.projection(),
        };

        assert!(matches!(
            embedder.embed_query(request),
            Err(RetrievalPortError::AuthorityUnavailable(message))
                if message == "semantic query already in flight"
        ));
        assert_eq!(
            second_service.stats().sessions_opened,
            0,
            "a rotated factory cannot open another model session"
        );

        drop(held);
        embedder
            .embed_query(request)
            .expect("the permit is released after the active query");
        assert_eq!(second_service.stats().sessions_opened, 1);
    }

    #[test]
    fn query_identity_mismatch_is_rejected_before_session_admission() {
        let authority = Arc::new(authority());
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            config(1, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        let embedder_factory = PooledSemanticQueryEmbedderFactory::new(Arc::clone(&service));
        let embedder = embedder_factory.create(Arc::new(ManualCancellation::new()));
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "must not reach inference",
            domain_id::<SanitizerRevision>("sanitizer.v1"),
            domain_id::<QueryNormalizationRevision>("normalizer.v1"),
        )
        .expect("bounded query");
        let wrong_digest = QueryDigest::new(
            domain_id("privacy.other"),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "33".repeat(32))).expect("query MAC"),
        );

        assert_eq!(
            embedder
                .embed_query(SemanticQueryEmbeddingRequestV1 {
                    query_digest: &wrong_digest,
                    query_view: &query_view,
                    projection: authority.projection(),
                })
                .err(),
            Some(RetrievalPortError::IncompatibleProjection)
        );
        assert_eq!(
            service.stats().sessions_opened,
            0,
            "invalid privacy identity cannot load or invoke the model"
        );
    }

    #[test]
    fn oversized_query_is_rejected_before_session_admission() {
        let authority = Arc::new(authority().with_test_max_batch_bytes(8));
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            config(1, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        let embedder_factory = PooledSemanticQueryEmbedderFactory::new(Arc::clone(&service));
        let embedder = embedder_factory.create(Arc::new(ManualCancellation::new()));
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "longer than eight bytes",
            domain_id::<SanitizerRevision>("sanitizer.v1"),
            domain_id::<QueryNormalizationRevision>("normalizer.v1"),
        )
        .expect("globally bounded query");
        let query_digest = QueryDigest::new(
            authority.projection().privacy_domain().clone(),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "44".repeat(32))).expect("query MAC"),
        );

        assert_eq!(
            embedder
                .embed_query(SemanticQueryEmbeddingRequestV1 {
                    query_digest: &query_digest,
                    query_view: &query_view,
                    projection: authority.projection(),
                })
                .err(),
            Some(RetrievalPortError::BudgetExceeded)
        );
        assert_eq!(
            service.stats().sessions_opened,
            0,
            "query byte admission runs before model loading"
        );
    }

    #[test]
    fn saturated_runtime_omits_semantics_without_entering_the_waiter_queue() {
        let authority = Arc::new(authority());
        let factory: SharedEmbeddingRuntimeFactory<FakeEmbeddingRuntime> =
            Arc::new(|| Ok(FakeEmbeddingRuntime::new().with_resident_bytes_per_session(1024)));
        let service = SemanticRuntimeService::new_owned(
            Arc::clone(&authority),
            factory,
            config(1, std::time::Duration::from_mins(1), 1 << 20),
        )
        .expect("runtime service");
        let held = service.acquire().expect("occupy the only session");
        let embedder_factory = PooledSemanticQueryEmbedderFactory::new(Arc::clone(&service));
        let embedder = embedder_factory.create(Arc::new(ManualCancellation::new()));
        let query_view = EphemeralSanitizedQueryViewV1::sanitize(
            "do not wait for semantic indexing",
            domain_id::<SanitizerRevision>("sanitizer.v1"),
            domain_id::<QueryNormalizationRevision>("normalizer.v1"),
        )
        .expect("bounded query");
        let query_digest = QueryDigest::new(
            authority.projection().privacy_domain().clone(),
            authority.projection().privacy_key_epoch(),
            QueryMac::new(format!("hmac-sha256:{}", "22".repeat(32))).expect("query MAC"),
        );

        let error = embedder
            .embed_query(SemanticQueryEmbeddingRequestV1 {
                query_digest: &query_digest,
                query_view: &query_view,
                projection: authority.projection(),
            })
            .err()
            .expect("saturated semantic runtime must abstain immediately");

        assert!(matches!(error, RetrievalPortError::AuthorityUnavailable(_)));
        assert_eq!(
            service.stats().queued_waiters,
            0,
            "retrieval never enters the background projection waiter queue"
        );
        drop(held);
    }
}
