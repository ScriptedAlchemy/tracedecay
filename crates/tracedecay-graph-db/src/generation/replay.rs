use serde::{Deserialize, Serialize};
use tracedecay_store::runtime::{
    GraphPublicationInputDigestV1, GraphPublicationReplayV1, GraphVerifiedHeadV1, StoreShardIdV1,
};

use super::{GraphDbError, GraphGenerationManifest, GraphIdempotencyKey};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphGenerationReplayMetadata {
    pub projection: super::GraphProjectionIdentity,
    pub generation: crate::GraphGenerationId,
    pub source_generation: super::SourceGeneration,
    pub watermark: super::GraphWatermark,
    pub dependencies: Vec<super::GraphGenerationDependency>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SealedCodeGenerationReplay {
    pub repository: tracedecay_domain::RepositoryId,
    pub generation: tracedecay_domain::CodeGenerationId,
    pub sealed_state_digest: SealedGraphStateDigest,
    pub projector_revision: GraphProjectorRevision,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorGenerationReplay {
    pub metadata: GraphGenerationReplayMetadata,
    pub semantic_generation_id: tracedecay_domain::VectorGenerationIdV1,
    pub base_generation: Option<tracedecay_domain::VectorGenerationIdV1>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphGenerationReplaySource {
    InlineManifest(Box<GraphGenerationManifest>),
    MetadataOnlyManifest(GraphGenerationReplayMetadata),
    SealedCodeGeneration(SealedCodeGenerationReplay),
    SemanticVectorGeneration(SemanticVectorGenerationReplay),
}

#[hotpath::measure_all]
impl GraphGenerationManifest {
    pub fn relational_metadata_replay(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        self.validate_checked(check)?;
        let payload = self.replay_source_payload(
            GraphGenerationReplaySource::MetadataOnlyManifest(GraphGenerationReplayMetadata {
                projection: self.projection.clone(),
                generation: self.generation.clone(),
                source_generation: self.source_generation.clone(),
                watermark: self.watermark.clone(),
                dependencies: self.dependencies.clone(),
            }),
            check,
        )?;
        self.relational_replay_with_payload(
            shard_id,
            idempotency_key,
            input_digest,
            expected_prior_head,
            payload,
            check,
        )
    }

    pub(crate) fn relational_semantic_vector_replay_with_recovered_digest(
        &self,
        plan: &tracedecay_store::SemanticVectorStagePlan,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_recovered_digest: tracedecay_store::GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        self.validate_checked(check)?;
        let payload = self.replay_source_payload(
            GraphGenerationReplaySource::SemanticVectorGeneration(SemanticVectorGenerationReplay {
                metadata: GraphGenerationReplayMetadata {
                    projection: self.projection.clone(),
                    generation: self.generation.clone(),
                    source_generation: self.source_generation.clone(),
                    watermark: self.watermark.clone(),
                    dependencies: self.dependencies.clone(),
                },
                semantic_generation_id: plan.semantic_generation_id.clone(),
                base_generation: plan.base_generation.clone(),
            }),
            check,
        )?;
        let mut replay = self.relational_replay_with_payload(
            plan.key.projection.shard_id.clone(),
            idempotency_key,
            input_digest,
            plan.expected_prior_verified_head.clone(),
            payload,
            check,
        )?;
        replay.expected_recovered_digest = expected_recovered_digest;
        replay
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        Ok(replay)
    }
}

pub(crate) fn metadata_manifest_from_replay(
    publication: &GraphPublicationReplayV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<GraphGenerationManifest>, GraphDbError> {
    let source = checked_decode_replay_source(&publication.canonical_replay_source, check)?;
    let metadata = match source {
        GraphGenerationReplaySource::MetadataOnlyManifest(metadata) => metadata,
        GraphGenerationReplaySource::SemanticVectorGeneration(vector) => vector.metadata,
        GraphGenerationReplaySource::InlineManifest(_)
        | GraphGenerationReplaySource::SealedCodeGeneration(_) => return Ok(None),
    };
    // The decoded metadata is kept for the binding comparison below instead
    // of decoding the canonical payload a second time; `new_checked` may
    // normalize (sort) dependencies, so the comparison still refuses a
    // journaled payload that was not already canonical.
    let manifest = GraphGenerationManifest::new_checked(
        metadata.projection.clone(),
        metadata.generation.clone(),
        metadata.source_generation.clone(),
        metadata.watermark.clone(),
        metadata.dependencies.clone(),
        Vec::new(),
        Vec::new(),
        check,
    )?;
    validate_metadata_publication(publication)?;
    validate_decoded_metadata_binding(publication, &manifest, &metadata, false, check)?;
    Ok(Some(manifest))
}

pub(crate) fn validate_metadata_binding(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    validate_metadata_publication(publication)?;
    manifest.validate_checked(check)?;
    let source = checked_decode_replay_source(&publication.canonical_replay_source, check)?;
    let metadata = match source {
        GraphGenerationReplaySource::MetadataOnlyManifest(metadata) => metadata,
        GraphGenerationReplaySource::SemanticVectorGeneration(vector) => vector.metadata,
        GraphGenerationReplaySource::InlineManifest(_)
        | GraphGenerationReplaySource::SealedCodeGeneration(_) => {
            return Err(GraphDbError::conflict("replay.validate_metadata_binding"));
        }
    };
    validate_decoded_metadata_binding(
        publication,
        manifest,
        &metadata,
        validate_expected_digest,
        check,
    )
}

#[hotpath::measure]
fn validate_metadata_publication(
    publication: &GraphPublicationReplayV1,
) -> Result<(), GraphDbError> {
    hotpath::measure_block!("graph_db.generation.replay.binding.validate", {
        publication
            .validate()
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("metadata-only graph replay is invalid: {error}"),
            })
    })
}

#[hotpath::measure]
fn validate_decoded_metadata_binding(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    metadata: &GraphGenerationReplayMetadata,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    if metadata.projection != manifest.projection
        || metadata.generation != manifest.generation
        || metadata.source_generation != manifest.source_generation
        || metadata.watermark != manifest.watermark
        || metadata.dependencies != manifest.dependencies
    {
        return Err(GraphDbError::conflict(
            "replay.validate_decoded_metadata_binding",
        ));
    }
    validate_publication_manifest_identity(publication, manifest, validate_expected_digest, check)
}

pub(crate) fn validate_supplied_manifest_binding(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    hotpath::measure_block!("graph_db.generation.replay.binding.validate", {
        publication
            .validate()
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("graph publication replay is invalid: {error}"),
            })
    })?;
    manifest.validate_checked(check)?;
    match checked_decode_replay_source(&publication.canonical_replay_source, check)? {
        GraphGenerationReplaySource::InlineManifest(replayed) if replayed.as_ref() == manifest => {
            validate_publication_manifest_identity(
                publication,
                manifest,
                validate_expected_digest,
                check,
            )
        }
        GraphGenerationReplaySource::MetadataOnlyManifest(metadata) => {
            validate_decoded_metadata_binding(
                publication,
                manifest,
                &metadata,
                validate_expected_digest,
                check,
            )
        }
        GraphGenerationReplaySource::SemanticVectorGeneration(vector) => {
            validate_decoded_metadata_binding(
                publication,
                manifest,
                &vector.metadata,
                validate_expected_digest,
                check,
            )
        }
        // A sealed code generation journals only its replay source, so the
        // supplied manifest cannot be compared field-by-field against a
        // journaled manifest. The identity check below still pins it exactly:
        // the journaled dependency-closure and expected-recovered digests were
        // derived from the manifest at append time, so only the manifest that
        // was journaled can pass. Refusing every supplied sealed manifest here
        // made each code-graph publication fail as `Conflict` immediately
        // after its own journal append, permanently wedging activation.
        GraphGenerationReplaySource::SealedCodeGeneration(source) => {
            validate_sealed_replay(&source)?;
            validate_publication_manifest_identity(
                publication,
                manifest,
                validate_expected_digest,
                check,
            )
        }
        GraphGenerationReplaySource::InlineManifest(_) => Err(GraphDbError::conflict(
            "replay.validate_supplied_manifest_binding",
        )),
    }
}

#[hotpath::measure]
fn validate_publication_manifest_identity(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    hotpath::measure_block!("graph_db.generation.replay.binding.identity", {
        let direct_dependencies =
            manifest.relational_dependency_generations(&publication.key.projection.shard_id)?;
        if publication.key.projection.namespace.as_str() != manifest.projection.namespace.as_str()
            || publication.key.projection.projection.as_str()
                != manifest.projection.projection.as_str()
            || publication.key.generation.as_str() != manifest.generation.as_str()
            || publication.direct_dependency_generations != direct_dependencies
            || publication.dependency_generation_closure_digest
                != manifest.dependency_closure_digest(check)?
            || (validate_expected_digest
                && publication.expected_recovered_digest
                    != manifest.expected_recovered_digest(check)?)
        {
            return Err(GraphDbError::conflict(
                "replay.validate_publication_manifest_identity",
            ));
        }
        Ok(())
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct SealedGraphStateDigest(String);

impl TryFrom<String> for SealedGraphStateDigest {
    type Error = GraphDbError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_sha256(&value, "sealed graph state digest")?;
        Ok(Self(value))
    }
}

impl From<SealedGraphStateDigest> for String {
    fn from(value: SealedGraphStateDigest) -> Self {
        value.0
    }
}

#[hotpath::measure_all]
impl SealedGraphStateDigest {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct GraphProjectorRevision(String);

impl TryFrom<String> for GraphProjectorRevision {
    type Error = GraphDbError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > 1024
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".:_-".contains(&byte))
        {
            return Err(GraphDbError::invalid(
                "sealed graph projector revision is invalid",
            ));
        }
        Ok(Self(value))
    }
}

impl From<GraphProjectorRevision> for String {
    fn from(value: GraphProjectorRevision) -> Self {
        value.0
    }
}

#[hotpath::measure_all]
impl GraphProjectorRevision {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_sha256(value: &str, subject: &str) -> Result<(), GraphDbError> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(GraphDbError::invalid(format!("{subject} must use sha256")));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(GraphDbError::invalid(format!("{subject} is invalid")));
    }
    Ok(())
}

pub trait GraphGenerationManifestProvider: Send + Sync {
    fn hydrate_sealed_code_generation(
        &self,
        owner: &tracedecay_store::GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, GraphDbError>;
}

pub(crate) struct InlineOnlyGraphGenerationManifestProvider;

impl GraphGenerationManifestProvider for InlineOnlyGraphGenerationManifestProvider {
    fn hydrate_sealed_code_generation(
        &self,
        _owner: &tracedecay_store::GraphProjectionIdentityV1,
        _source: &SealedCodeGenerationReplay,
        _check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, GraphDbError> {
        Err(GraphDbError::unavailable(
            "sealed code generation replay provider is not mounted",
        ))
    }
}

#[hotpath::measure(label = "graph_db.generation.replay.decode")]
pub(crate) fn checked_decode_replay_source(
    payload: &[u8],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationReplaySource, GraphDbError> {
    check()?;
    // Parse straight from the in-memory slice. Reader-mode serde_json pulls
    // exactly one byte per `Read::read` call through `io::Bytes` (measured:
    // 2.9M one-byte reads and ~21x the slice-mode wall time for a 2.9MB
    // payload), and `io::Bytes` retries `ErrorKind::Interrupted`, so the old
    // checked reader's per-interval cancellation never aborted the parse —
    // its failure only surfaced after the full slow parse finished. Checks at
    // the decode boundaries bound the work between two cancellation checks by
    // one slice parse of a payload capped at
    // `MAX_GRAPH_REPLAY_SOURCE_BYTES_V1`, a tighter worst-case cancellation
    // latency than the reader ever delivered.
    let mut deserializer = serde_json::Deserializer::from_slice(payload);
    let decoded = GraphGenerationReplaySource::deserialize(&mut deserializer)
        .and_then(|source| deserializer.end().map(|()| source));
    check()?;
    let source = decoded.map_err(|error| {
        GraphDbError::invalid(format!(
            "canonical graph generation replay is invalid: {error}"
        ))
    })?;
    crate::hotpath_observe::record_counts(0, 0, 1, payload.len());
    crate::hotpath_observe::record_hydration_source(hydration_source(&source));
    Ok(source)
}

#[hotpath::measure]
fn hydration_source(
    source: &GraphGenerationReplaySource,
) -> crate::hotpath_observe::HydrationSource {
    match source {
        GraphGenerationReplaySource::InlineManifest(_) => {
            crate::hotpath_observe::HydrationSource::Inline
        }
        GraphGenerationReplaySource::MetadataOnlyManifest(_) => {
            crate::hotpath_observe::HydrationSource::Metadata
        }
        GraphGenerationReplaySource::SealedCodeGeneration(_) => {
            crate::hotpath_observe::HydrationSource::Sealed
        }
        GraphGenerationReplaySource::SemanticVectorGeneration(_) => {
            crate::hotpath_observe::HydrationSource::SemanticVector
        }
    }
}

pub(super) fn validate_sealed_replay(
    source: &SealedCodeGenerationReplay,
) -> Result<(), GraphDbError> {
    source
        .repository
        .validate()
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    source
        .generation
        .validate()
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        GraphEntity, GraphEntityId, GraphGenerationDependency, GraphGenerationId, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphWatermark, SourceGeneration,
    };

    fn projection(namespace: &str, projection: &str) -> GraphProjectionIdentity {
        GraphProjectionIdentity::new(
            GraphNamespace::new(namespace).unwrap(),
            GraphProjectionId::new(projection).unwrap(),
        )
    }

    fn shard() -> StoreShardIdV1 {
        StoreShardIdV1::project(
            tracedecay_store::BrainId::new("brain.replay").unwrap(),
            tracedecay_store::UserProfileId::new("profile.replay").unwrap(),
            tracedecay_store::ProjectId::new("project.replay").unwrap(),
        )
    }

    fn input_digest() -> GraphPublicationInputDigestV1 {
        GraphPublicationInputDigestV1::new(format!("sha256:{}", "d".repeat(64))).unwrap()
    }

    fn corpus_manifest() -> GraphGenerationManifest {
        let identity = projection("replay-decode", "corpus");
        let entities = (0..1_000)
            .map(|index| {
                GraphEntity::new(
                    GraphEntityId::new(format!("entity:{index:04}")).unwrap(),
                    BTreeSet::new(),
                    BTreeMap::from([(
                        GraphPropertyName::new("marker").unwrap(),
                        GraphProperty::String("m".repeat(200)),
                    )]),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        GraphGenerationManifest::new(
            identity,
            GraphGenerationId::new("corpus-generation").unwrap(),
            SourceGeneration::new("corpus-source").unwrap(),
            GraphWatermark::new("corpus-watermark").unwrap(),
            vec![GraphGenerationDependency::new(
                projection("replay-decode", "dependency"),
                GraphGenerationId::new("dependency-g1").unwrap(),
                GraphIdempotencyKey::new("publish:dependency-g1").unwrap(),
            )],
            entities,
            vec![],
        )
        .unwrap()
    }

    fn metadata_manifest() -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            projection("replay-decode", "metadata"),
            GraphGenerationId::new("metadata-generation").unwrap(),
            SourceGeneration::new("metadata-source").unwrap(),
            GraphWatermark::new("metadata-watermark").unwrap(),
            vec![GraphGenerationDependency::new(
                projection("replay-decode", "dependency"),
                GraphGenerationId::new("dependency-g1").unwrap(),
                GraphIdempotencyKey::new("publish:dependency-g1").unwrap(),
            )],
            vec![],
            vec![],
        )
        .unwrap()
    }

    fn assert_invalid(result: Result<GraphGenerationReplaySource, GraphDbError>) {
        match result {
            Err(GraphDbError::InvalidRequest { message }) => assert!(
                message.starts_with("canonical graph generation replay is invalid"),
                "unexpected rejection message: {message}"
            ),
            other => panic!("malformed payload must be rejected as invalid: {other:?}"),
        }
    }

    #[test]
    fn canonical_inline_source_decodes_the_identical_manifest() {
        let manifest = corpus_manifest();
        let payload = manifest.canonical_replay_source(&|| Ok(())).unwrap();
        let decoded = checked_decode_replay_source(&payload, &|| Ok(())).unwrap();
        assert_eq!(
            decoded,
            GraphGenerationReplaySource::InlineManifest(Box::new(manifest))
        );
    }

    #[test]
    fn canonical_sealed_source_decodes_the_identical_replay() {
        let sealed = SealedCodeGenerationReplay {
            repository: tracedecay_domain::RepositoryId::new("repository.replay").unwrap(),
            generation: tracedecay_domain::CodeGenerationId::new("code-generation.replay").unwrap(),
            sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                "sha256:{}",
                "5".repeat(64)
            ))
            .unwrap(),
            projector_revision: GraphProjectorRevision::try_from("projector.replay".to_owned())
                .unwrap(),
        };
        let payload = metadata_manifest()
            .sealed_replay_payload(sealed.clone(), &|| Ok(()))
            .unwrap();
        let decoded = checked_decode_replay_source(&payload, &|| Ok(())).unwrap();
        assert_eq!(
            decoded,
            GraphGenerationReplaySource::SealedCodeGeneration(sealed)
        );
    }

    #[test]
    fn malformed_payloads_are_rejected_as_invalid() {
        let payload = corpus_manifest()
            .canonical_replay_source(&|| Ok(()))
            .unwrap();

        let truncated = &payload[..payload.len() / 2];
        assert_invalid(checked_decode_replay_source(truncated, &|| Ok(())));

        let mut trailing = payload.clone();
        trailing.extend_from_slice(b"{}");
        assert_invalid(checked_decode_replay_source(&trailing, &|| Ok(())));

        assert_invalid(checked_decode_replay_source(
            br#"{"mystery_source":{}}"#,
            &|| Ok(()),
        ));

        let mut tampered: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        tampered["inline_manifest"]["mystery"] = serde_json::Value::Bool(true);
        let tampered = serde_json::to_vec(&tampered).unwrap();
        assert_invalid(checked_decode_replay_source(&tampered, &|| Ok(())));
    }

    #[test]
    fn cancellation_surfaces_as_the_typed_error_at_both_decode_boundaries() {
        let payload = corpus_manifest()
            .canonical_replay_source(&|| Ok(()))
            .unwrap();

        // Cancelled before any parse work: exactly the entry check runs.
        let polls = AtomicUsize::new(0);
        let result = checked_decode_replay_source(&payload, &|| {
            polls.fetch_add(1, Ordering::SeqCst);
            Err(GraphDbError::Cancelled)
        });
        assert_eq!(result, Err(GraphDbError::Cancelled));
        assert_eq!(polls.load(Ordering::SeqCst), 1);

        // Cancelled while the payload decodes: the boundary check after the
        // parse surfaces the typed error instead of returning the decoded
        // source.
        let polls = AtomicUsize::new(0);
        let result = checked_decode_replay_source(&payload, &|| {
            if polls.fetch_add(1, Ordering::SeqCst) >= 1 {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(GraphDbError::Cancelled));
        assert_eq!(polls.load(Ordering::SeqCst), 2);

        // A cancellation observed during decode outranks a malformed payload,
        // matching the old checked reader's failure precedence.
        let truncated = &payload[..payload.len() / 2];
        let polls = AtomicUsize::new(0);
        let result = checked_decode_replay_source(truncated, &|| {
            if polls.fetch_add(1, Ordering::SeqCst) >= 1 {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(GraphDbError::Cancelled));
    }

    /// `metadata_manifest_from_replay` must decode the canonical payload
    /// exactly once; the binding validation reuses the decoded metadata. The
    /// hydration counters record one replay row per decode, so a second parse
    /// pass fails this test.
    #[test]
    fn metadata_manifest_from_replay_decodes_the_payload_once() {
        let manifest = metadata_manifest();
        let publication = manifest
            .relational_metadata_replay(
                shard(),
                GraphIdempotencyKey::new("publish:metadata-generation").unwrap(),
                input_digest(),
                None,
                &|| Ok(()),
            )
            .unwrap();
        let _ = crate::hotpath_observe::take_hydration_counters();
        let hydrated = metadata_manifest_from_replay(&publication, &|| Ok(()))
            .unwrap()
            .expect("a metadata-only replay must hydrate a metadata manifest");
        let counters = crate::hotpath_observe::take_hydration_counters();
        assert_eq!(
            counters.replay_rows, 1,
            "metadata hydration must decode the canonical payload exactly once: {counters:?}"
        );
        assert_eq!(
            counters.generation_bytes,
            publication.canonical_replay_source.len() as u64,
            "decoded payload bytes must match one decode pass: {counters:?}"
        );
        assert_eq!(hydrated.projection, manifest.projection);
        assert_eq!(hydrated.generation, manifest.generation);
        assert_eq!(hydrated.source_generation, manifest.source_generation);
        assert_eq!(hydrated.watermark, manifest.watermark);
        assert_eq!(hydrated.dependencies, manifest.dependencies);
        assert!(hydrated.entities.is_empty());
        assert!(hydrated.relations.is_empty());
    }
}
