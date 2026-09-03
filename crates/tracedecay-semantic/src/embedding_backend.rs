//! Production embedding-backend selection.
//!
//! The catalog entry a model was selected from declares its backend; that
//! declaration becomes `runtime_backend` in the admitted projection identity
//! and nothing else decides which runtime serves an authority. The dispatcher
//! here reads that admitted identity and routes every port operation to the
//! matching backend, so the session pool, projector, and query runtime stay
//! generic over one runtime type while two backends exist. Vectors from the
//! two backends can never mix: their projection keys differ in
//! `runtime_backend`, `runtime_build_revision`, dimensions, precision, and
//! member digests, so they live in different vector generations.
use std::sync::Arc;

use super::artifact_store::{FASTEMBED_RUNTIME_BUILD_REVISION_V1, FASTEMBED_RUNTIME_FAMILY_V1};
use super::fastembed_adapter::{
    AdmittedProjectionArtifactV1, BoundedSanitizedTextBatchV1, EmbedError, EmbeddingRuntime,
    EmbeddingSession, EmbeddingVectorV1, FastEmbedEmbeddingRuntime, SemanticExecutionAuthority,
};
use super::model2vec_adapter::{
    MODEL2VEC_RUNTIME_BUILD_REVISION_V1, MODEL2VEC_RUNTIME_FAMILY_V1, Model2VecEmbeddingRuntime,
};
use super::runtime_service::SharedEmbeddingRuntimeFactory;

/// Runtime family recorded as `EmbeddingProjectionKeyV1::runtime_backend`.
///
/// Every backend that can produce vectors has exactly one variant here; the
/// string forms are persisted in projection keys and artifact manifests and
/// must never change for an existing variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmbeddingRuntimeFamilyV1 {
    FastEmbedOrt,
    Model2VecStatic,
}

impl EmbeddingRuntimeFamilyV1 {
    pub const fn runtime_family(self) -> &'static str {
        match self {
            Self::FastEmbedOrt => FASTEMBED_RUNTIME_FAMILY_V1,
            Self::Model2VecStatic => MODEL2VEC_RUNTIME_FAMILY_V1,
        }
    }

    /// Exact implementation revision this binary links for the family.
    pub const fn build_revision(self) -> &'static str {
        match self {
            Self::FastEmbedOrt => FASTEMBED_RUNTIME_BUILD_REVISION_V1,
            Self::Model2VecStatic => MODEL2VEC_RUNTIME_BUILD_REVISION_V1,
        }
    }

    /// Typed parse of a persisted `runtime_backend`; `None` names a backend
    /// this binary has no runtime for.
    pub fn from_runtime_family(name: &str) -> Option<Self> {
        [Self::FastEmbedOrt, Self::Model2VecStatic]
            .into_iter()
            .find(|family| family.runtime_family() == name)
    }
}

/// The production embedding runtime: one value that serves every cataloged
/// backend by dispatching on the authority's admitted runtime family.
#[derive(Default)]
pub struct ProductionEmbeddingRuntime {
    fastembed: FastEmbedEmbeddingRuntime,
    model2vec: Model2VecEmbeddingRuntime,
}

/// One warmed session of whichever backend the authority selected.
///
/// Both sessions are boxed so the enum stays pointer-sized: the ORT session
/// and the Model2Vec table handle are each large, and leaving either inline
/// made the discriminant dispatch copy a large unused payload on every call
/// through the other backend.
pub enum ProductionEmbeddingSession {
    FastEmbed(Box<<FastEmbedEmbeddingRuntime as EmbeddingRuntime>::Session>),
    Model2Vec(Box<<Model2VecEmbeddingRuntime as EmbeddingRuntime>::Session>),
}

impl EmbeddingSession for ProductionEmbeddingSession {
    fn authority(&self) -> &AdmittedProjectionArtifactV1 {
        match self {
            Self::FastEmbed(session) => session.authority(),
            Self::Model2Vec(session) => session.authority(),
        }
    }

    fn resident_bytes_estimate(&self) -> u64 {
        match self {
            Self::FastEmbed(session) => session.resident_bytes_estimate(),
            Self::Model2Vec(session) => session.resident_bytes_estimate(),
        }
    }

    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        match self {
            Self::FastEmbed(session) => session.embed_batch(batch, authority),
            Self::Model2Vec(session) => session.embed_batch(batch, authority),
        }
    }
}

impl EmbeddingRuntime for ProductionEmbeddingRuntime {
    type Session = ProductionEmbeddingSession;

    fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64 {
        match authority.runtime_family() {
            EmbeddingRuntimeFamilyV1::FastEmbedOrt => {
                self.fastembed.resident_bytes_reservation(authority)
            }
            EmbeddingRuntimeFamilyV1::Model2VecStatic => {
                self.model2vec.resident_bytes_reservation(authority)
            }
        }
    }

    fn verify_artifact_compatibility(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError> {
        match authority.runtime_family() {
            EmbeddingRuntimeFamilyV1::FastEmbedOrt => {
                self.fastembed.verify_artifact_compatibility(authority)
            }
            EmbeddingRuntimeFamilyV1::Model2VecStatic => {
                self.model2vec.verify_artifact_compatibility(authority)
            }
        }
    }

    fn open_session(
        &self,
        authority: &AdmittedProjectionArtifactV1,
        interruption: &dyn SemanticExecutionAuthority,
    ) -> Result<Self::Session, EmbedError> {
        match authority.runtime_family() {
            EmbeddingRuntimeFamilyV1::FastEmbedOrt => self
                .fastembed
                .open_session(authority, interruption)
                .map(|session| ProductionEmbeddingSession::FastEmbed(Box::new(session))),
            EmbeddingRuntimeFamilyV1::Model2VecStatic => self
                .model2vec
                .open_session(authority, interruption)
                .map(|session| ProductionEmbeddingSession::Model2Vec(Box::new(session))),
        }
    }
}

/// Owned factory for the production runtime. Backends whose dependency
/// feature is compiled out are present as stand-ins whose operations fail
/// with a typed runtime failure, so a selection the build cannot serve
/// degrades to the documented fallback states instead of failing to build.
pub fn production_embedding_runtime_factory()
-> SharedEmbeddingRuntimeFactory<ProductionEmbeddingRuntime> {
    Arc::new(|| Ok(ProductionEmbeddingRuntime::default()))
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{ChunkerRevision, EmbeddingPrecisionV1, PrivacyDomainId};
    use tracedecay_semantic_contracts::{
        DEFAULT_FASTEMBED_MODEL_ID, MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID, SemanticResourceCeilings,
    };

    use super::super::artifact_store::AdmittedArtifactV1;
    #[cfg(not(all(feature = "semantic-fastembed", feature = "semantic-model2vec")))]
    use super::super::fastembed_adapter::ManualCancellation;
    use super::super::fastembed_adapter::ProjectionArtifactPinV1;
    use super::super::fastembed_adapter::lifecycle_test_support::{
        lifecycle_authority_from, lifecycle_install_fixture, model2vec_lifecycle_install_fixture,
    };
    use super::super::model_catalog::FastEmbedModelCatalogV1;
    use super::super::session_pool::test_support::{admitted_artifact, projection_for};
    use super::*;

    // Only a compiled-out backend surfaces a runtime failure here.
    #[cfg(not(all(feature = "semantic-fastembed", feature = "semantic-model2vec")))]
    fn runtime_failure_detail(error: EmbedError) -> String {
        match error {
            EmbedError::Runtime(failure) => failure.detail,
            other => panic!("expected a runtime failure, got {other:?}"),
        }
    }

    /// The production selection path: settings name a catalog id, the
    /// lifecycle projects it, and the projection alone decides the backend.
    #[test]
    fn production_catalog_projections_select_distinct_backends() {
        let catalog = FastEmbedModelCatalogV1::production();
        let resources = SemanticResourceCeilings::default();
        let projection = |model_id: &str| {
            AdmittedProjectionArtifactV1::lifecycle_projection(
                catalog.get(model_id).expect("cataloged"),
                ChunkerRevision::new("chunker.v1").expect("chunker"),
                PrivacyDomainId::new("privacy.project-a".to_owned()).expect("privacy"),
                7,
                resources,
            )
            .expect("production projection")
        };
        let fastembed = projection(DEFAULT_FASTEMBED_MODEL_ID);
        let model2vec = projection(MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID);

        let fastembed_key = fastembed.embedding_key();
        assert_eq!(
            EmbeddingRuntimeFamilyV1::from_runtime_family(&fastembed_key.runtime_backend),
            Some(EmbeddingRuntimeFamilyV1::FastEmbedOrt)
        );
        assert_eq!(
            fastembed_key.runtime_build_revision,
            FASTEMBED_RUNTIME_BUILD_REVISION_V1
        );
        assert_eq!(fastembed_key.dimensions, 768);
        assert_eq!(fastembed_key.precision, EmbeddingPrecisionV1::Fp32);

        let model2vec_key = model2vec.embedding_key();
        assert_eq!(
            EmbeddingRuntimeFamilyV1::from_runtime_family(&model2vec_key.runtime_backend),
            Some(EmbeddingRuntimeFamilyV1::Model2VecStatic)
        );
        assert_eq!(
            model2vec_key.runtime_build_revision,
            MODEL2VEC_RUNTIME_BUILD_REVISION_V1
        );
        assert_eq!(model2vec_key.dimensions, 256);
        assert_eq!(model2vec_key.precision, EmbeddingPrecisionV1::Fp16);
        assert_eq!(
            model2vec_key.truncation_length,
            1024.min(resources.max_sequence_length)
        );

        // Distinct identities on every axis that would let vectors mix.
        assert_ne!(fastembed.projection_key(), model2vec.projection_key());
        assert_ne!(
            fastembed_key.model_artifact_digest,
            model2vec_key.model_artifact_digest
        );
        assert_ne!(
            fastembed_key.tokenizer_digest,
            model2vec_key.tokenizer_digest
        );
        assert_ne!(fastembed_key.config_digest, model2vec_key.config_digest);
    }

    #[test]
    fn dispatcher_routes_each_lifecycle_authority_to_its_declared_backend() {
        let model2vec_fixture = model2vec_lifecycle_install_fixture(
            EmbeddingPrecisionV1::Fp16,
            r#"{"normalize": true}"#,
            8,
        );
        let model2vec = lifecycle_authority_from(&model2vec_fixture, 4096).expect("model2vec");
        let fastembed_fixture = lifecycle_install_fixture(b"model");
        let fastembed = lifecycle_authority_from(&fastembed_fixture, 1024).expect("fastembed");
        assert_eq!(
            model2vec.runtime_family(),
            EmbeddingRuntimeFamilyV1::Model2VecStatic
        );
        assert_eq!(
            fastembed.runtime_family(),
            EmbeddingRuntimeFamilyV1::FastEmbedOrt
        );

        let runtime = ProductionEmbeddingRuntime::default();
        assert_eq!(
            runtime.resident_bytes_reservation(&model2vec),
            model2vec.resident_bytes_estimate()
        );
        assert_eq!(
            runtime.resident_bytes_reservation(&fastembed),
            fastembed.resident_bytes_estimate()
        );

        // Each authority receives its own backend's verdict: a compiled-in
        // backend admits the fixture descriptor, a compiled-out backend
        // names its own feature — never the other backend's.
        #[cfg(feature = "semantic-model2vec")]
        runtime
            .verify_artifact_compatibility(&model2vec)
            .expect("Model2Vec backend admits its descriptor");
        #[cfg(not(feature = "semantic-model2vec"))]
        {
            let detail = runtime_failure_detail(
                runtime
                    .verify_artifact_compatibility(&model2vec)
                    .expect_err("compiled-out Model2Vec"),
            );
            assert!(detail.contains("semantic-model2vec"), "{detail}");
            let detail = runtime_failure_detail(
                runtime
                    .open_session(&model2vec, &ManualCancellation::new())
                    .err()
                    .expect("compiled-out Model2Vec session"),
            );
            assert!(detail.contains("semantic-model2vec"), "{detail}");
        }
        #[cfg(feature = "semantic-fastembed")]
        runtime
            .verify_artifact_compatibility(&fastembed)
            .expect("FastEmbed backend admits its descriptor");
        #[cfg(not(feature = "semantic-fastembed"))]
        {
            let detail = runtime_failure_detail(
                runtime
                    .verify_artifact_compatibility(&fastembed)
                    .expect_err("compiled-out FastEmbed"),
            );
            assert!(detail.contains("semantic-fastembed"), "{detail}");
            let detail = runtime_failure_detail(
                runtime
                    .open_session(&fastembed, &ManualCancellation::new())
                    .err()
                    .expect("compiled-out FastEmbed session"),
            );
            assert!(detail.contains("semantic-fastembed"), "{detail}");
        }
    }

    #[test]
    fn persisted_manifests_naming_an_unknown_runtime_fail_the_backend_pin() {
        let mut manifest = admitted_artifact().manifest().clone();
        manifest.payload.runtime.runtime = "sentence-transformers".to_owned();
        let artifact = AdmittedArtifactV1::test_fixture(manifest);
        let mut key = projection_for(&artifact);
        key.runtime_backend = "sentence-transformers".to_owned();
        let projection = key.admit().expect("structurally valid key");
        assert_eq!(
            AdmittedProjectionArtifactV1::admit(&artifact, &projection),
            Err(ProjectionArtifactPinV1::RuntimeBackend)
        );
    }

    #[test]
    fn runtime_families_round_trip_their_persisted_names() {
        for family in [
            EmbeddingRuntimeFamilyV1::FastEmbedOrt,
            EmbeddingRuntimeFamilyV1::Model2VecStatic,
        ] {
            assert_eq!(
                EmbeddingRuntimeFamilyV1::from_runtime_family(family.runtime_family()),
                Some(family)
            );
            assert!(!family.build_revision().is_empty());
        }
        assert_eq!(
            EmbeddingRuntimeFamilyV1::from_runtime_family("fastembed-ort"),
            Some(EmbeddingRuntimeFamilyV1::FastEmbedOrt)
        );
        assert_eq!(
            EmbeddingRuntimeFamilyV1::from_runtime_family("model2vec-static"),
            Some(EmbeddingRuntimeFamilyV1::Model2VecStatic)
        );
        assert_eq!(
            EmbeddingRuntimeFamilyV1::from_runtime_family("sentence-transformers"),
            None
        );
        assert_ne!(
            EmbeddingRuntimeFamilyV1::FastEmbedOrt.build_revision(),
            EmbeddingRuntimeFamilyV1::Model2VecStatic.build_revision()
        );
    }
}
