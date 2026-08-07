use std::io::{self, Read};

use serde::{Deserialize, Serialize};
use tracedecay_store::runtime::{
    GraphPublicationInputDigestV1, GraphPublicationReplayV1, GraphVerifiedHeadV1, StoreShardIdV1,
};

use super::{
    DIGEST_CHECK_INTERVAL_BYTES, GraphDbError, GraphGenerationManifest, GraphIdempotencyKey,
};

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
    InlineManifest(GraphGenerationManifest),
    MetadataOnlyManifest(GraphGenerationReplayMetadata),
    SealedCodeGeneration(SealedCodeGenerationReplay),
    SemanticVectorGeneration(SemanticVectorGenerationReplay),
}

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
    let manifest = GraphGenerationManifest::new_checked(
        metadata.projection,
        metadata.generation,
        metadata.source_generation,
        metadata.watermark,
        metadata.dependencies,
        Vec::new(),
        Vec::new(),
        check,
    )?;
    validate_metadata_binding(publication, &manifest, false, check)?;
    Ok(Some(manifest))
}

pub(crate) fn validate_metadata_binding(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    publication
        .validate()
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("metadata-only graph replay is invalid: {error}"),
        })?;
    manifest.validate_checked(check)?;
    let source = checked_decode_replay_source(&publication.canonical_replay_source, check)?;
    let metadata = match source {
        GraphGenerationReplaySource::MetadataOnlyManifest(metadata) => metadata,
        GraphGenerationReplaySource::SemanticVectorGeneration(vector) => vector.metadata,
        GraphGenerationReplaySource::InlineManifest(_)
        | GraphGenerationReplaySource::SealedCodeGeneration(_) => {
            return Err(GraphDbError::Conflict);
        }
    };
    if metadata.projection != manifest.projection
        || metadata.generation != manifest.generation
        || metadata.source_generation != manifest.source_generation
        || metadata.watermark != manifest.watermark
        || metadata.dependencies != manifest.dependencies
    {
        return Err(GraphDbError::Conflict);
    }
    validate_publication_manifest_identity(publication, manifest, validate_expected_digest, check)
}

pub(crate) fn validate_supplied_manifest_binding(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    publication
        .validate()
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("graph publication replay is invalid: {error}"),
        })?;
    manifest.validate_checked(check)?;
    match checked_decode_replay_source(&publication.canonical_replay_source, check)? {
        GraphGenerationReplaySource::InlineManifest(replayed) if replayed == *manifest => {
            validate_publication_manifest_identity(
                publication,
                manifest,
                validate_expected_digest,
                check,
            )
        }
        GraphGenerationReplaySource::MetadataOnlyManifest(_)
        | GraphGenerationReplaySource::SemanticVectorGeneration(_) => {
            validate_metadata_binding(publication, manifest, validate_expected_digest, check)
        }
        GraphGenerationReplaySource::InlineManifest(_)
        | GraphGenerationReplaySource::SealedCodeGeneration(_) => Err(GraphDbError::Conflict),
    }
}

fn validate_publication_manifest_identity(
    publication: &GraphPublicationReplayV1,
    manifest: &GraphGenerationManifest,
    validate_expected_digest: bool,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    let direct_dependencies =
        manifest.relational_dependency_generations(&publication.key.projection.shard_id)?;
    if publication.key.projection.namespace.as_str() != manifest.projection.namespace.as_str()
        || publication.key.projection.projection.as_str() != manifest.projection.projection.as_str()
        || publication.key.generation.as_str() != manifest.generation.as_str()
        || publication.direct_dependency_generations != direct_dependencies
        || publication.dependency_generation_closure_digest
            != manifest.dependency_closure_digest(check)?
        || (validate_expected_digest
            && publication.expected_recovered_digest
                != manifest.expected_recovered_digest(check)?)
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
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

pub(crate) fn checked_decode_replay_source(
    payload: &[u8],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationReplaySource, GraphDbError> {
    check()?;
    let mut reader = CheckedSliceReader::new(payload, check);
    let decoded = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
        match GraphGenerationReplaySource::deserialize(&mut deserializer) {
            Ok(source) => deserializer.end().map(|()| source),
            Err(error) => Err(error),
        }
    };
    if let Some(error) = reader.take_failure() {
        return Err(error);
    }
    check()?;
    decoded.map_err(|error| {
        GraphDbError::invalid(format!(
            "canonical graph generation replay is invalid: {error}"
        ))
    })
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

struct CheckedSliceReader<'a> {
    payload: &'a [u8],
    offset: usize,
    bytes_since_check: u64,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
}

impl<'a> CheckedSliceReader<'a> {
    fn new(payload: &'a [u8], check: &'a dyn Fn() -> Result<(), GraphDbError>) -> Self {
        Self {
            payload,
            offset: 0,
            bytes_since_check: 0,
            check,
            failure: None,
        }
    }

    fn take_failure(&mut self) -> Option<GraphDbError> {
        self.failure.take()
    }
}

impl Read for CheckedSliceReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.offset >= self.payload.len() {
            return Ok(0);
        }
        let remaining = &self.payload[self.offset..];
        let length = remaining.len().min(output.len());
        let length_u64 = u64::try_from(length)
            .map_err(|_| io::Error::other("canonical replay read length is too large"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(length_u64)
            .ok_or_else(|| io::Error::other("canonical replay check interval overflow"))?;
        if self.bytes_since_check >= DIGEST_CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "canonical graph replay decode interrupted",
                ));
            }
        }
        output[..length].copy_from_slice(&remaining[..length]);
        self.offset += length;
        Ok(length)
    }
}
