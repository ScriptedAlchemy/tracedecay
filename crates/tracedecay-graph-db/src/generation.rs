use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, Write};

use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::{encode_lowercase_hex, encode_tagged_lowercase_hex};
use tracedecay_store::runtime::{
    GraphDependencyGenerationClosureDigestV1, GraphDependencyGenerationIdentityV1,
    GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationReplayV1, GraphRecoveredGenerationDigestV1, GraphVerifiedHeadV1,
    MAX_GRAPH_REPLAY_SOURCE_BYTES_V1, StoreShardIdV1,
};

use crate::limits::{MAX_VERIFIED_GENERATION_ENTITIES, MAX_VERIFIED_GENERATION_RELATIONS};
use crate::schema::{NAMESPACE_PROPERTY, required_string};
use crate::state::latest_projection;
use crate::{
    GraphBudgetKind, GraphDbError, GraphEntity, GraphEntityId, GraphGenerationId,
    GraphIdempotencyKey, GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark, SourceGeneration,
};

const DIGEST_CHECK_INTERVAL_BYTES: u64 = 64 * 1024;
const CHECKED_VEC_INITIAL_CAPACITY_BYTES: usize = 1_024;

#[path = "generation/identity.rs"]
mod identity;
#[path = "generation/recovered.rs"]
mod recovered;
#[path = "generation/replay.rs"]
mod replay;
pub use identity::{
    GraphEntityRef, GraphGenerationDependency, GraphProjectionIdentity, GraphRelationRef,
};
pub(crate) use recovered::recovered_generation_digest_from_database;
pub(crate) use replay::InlineOnlyGraphGenerationManifestProvider;
use replay::validate_sealed_replay;
pub use replay::{
    GraphGenerationManifestProvider, GraphGenerationReplayMetadata, GraphGenerationReplaySource,
    GraphProjectorRevision, SealedCodeGenerationReplay, SealedGraphStateDigest,
    SemanticVectorGenerationReplay,
};
pub(crate) use replay::{
    checked_decode_replay_source, metadata_manifest_from_replay, validate_metadata_binding,
    validate_supplied_manifest_binding,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphGenerationRelation {
    pub identity: GraphRelationId,
    pub from: GraphEntityRef,
    pub to: GraphEntityRef,
    pub kind: GraphRelationKind,
    pub properties: BTreeMap<GraphPropertyName, GraphProperty>,
}

impl GraphGenerationRelation {
    pub fn new(
        identity: GraphRelationId,
        from: GraphEntityRef,
        to: GraphEntityRef,
        kind: GraphRelationKind,
        properties: BTreeMap<GraphPropertyName, GraphProperty>,
    ) -> Result<Self, GraphDbError> {
        let relation = Self {
            identity,
            from,
            to,
            kind,
            properties,
        };
        relation.validate()?;
        Ok(relation)
    }

    fn validate(&self) -> Result<(), GraphDbError> {
        GraphRelation::new(
            self.identity.clone(),
            self.from.identity.clone(),
            self.to.identity.clone(),
            self.kind.clone(),
            self.properties.clone(),
        )
        .map(|_| ())
    }

    pub(crate) fn storage_relation(&self) -> Result<GraphRelation, GraphDbError> {
        GraphRelation::new(
            self.identity.clone(),
            self.from.identity.clone(),
            self.to.identity.clone(),
            self.kind.clone(),
            self.properties.clone(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphGenerationManifest {
    pub projection: GraphProjectionIdentity,
    pub generation: GraphGenerationId,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub dependencies: Vec<GraphGenerationDependency>,
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphGenerationRelation>,
}

/// The small, cheaply cloned metadata half of a generation manifest: exactly
/// the fields that name a generation and bind it to its dependency closure.
///
/// Every stage after the staged rows are durable — the close/reopen
/// recovered-digest proof, quarantine, lease seating, and the finalization
/// receipt — reads only these fields. Carrying them separately lets the bulk
/// `entities`/`relations` vectors (multiple gigabytes on a first index) be
/// released the moment the last staging page commits, instead of staying live
/// through reopen and verification alongside the rebuilt in-RAM store.
///
/// This type is deliberately not serialized: the canonical replay payload and
/// every pinned digest are still produced from the flat
/// [`GraphGenerationManifest`] shape, so its byte layout is unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphGenerationManifestIdentity {
    pub projection: GraphProjectionIdentity,
    pub generation: GraphGenerationId,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub dependencies: Vec<GraphGenerationDependency>,
}

impl GraphGenerationManifestIdentity {
    pub fn dependency_closure_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
        dependency_closure_digest(&self.dependencies, check)
    }

    pub(crate) fn physical_namespace(&self) -> Result<GraphNamespace, GraphDbError> {
        physical_namespace(
            &self.projection.namespace,
            &self.projection.projection,
            &self.generation,
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GraphReplayCollectionOutcome {
    Retired(GraphGenerationReplaySource),
    Retained,
    Absent,
}

impl GraphGenerationManifest {
    pub fn new(
        projection: GraphProjectionIdentity,
        generation: GraphGenerationId,
        source_generation: SourceGeneration,
        watermark: GraphWatermark,
        dependencies: Vec<GraphGenerationDependency>,
        entities: Vec<GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
    ) -> Result<Self, GraphDbError> {
        Self::new_checked(
            projection,
            generation,
            source_generation,
            watermark,
            dependencies,
            entities,
            relations,
            &|| Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_checked(
        projection: GraphProjectionIdentity,
        generation: GraphGenerationId,
        source_generation: SourceGeneration,
        watermark: GraphWatermark,
        dependencies: Vec<GraphGenerationDependency>,
        entities: Vec<GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        check()?;
        if entities.len() > MAX_VERIFIED_GENERATION_ENTITIES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_ENTITIES,
            ));
        }
        if relations.len() > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_RELATIONS,
            ));
        }
        let dependencies = checked_sorted_dependencies(dependencies, check)?;
        let entities = checked_sorted_entities(entities, check)?;
        let relations = checked_sorted_relations(relations, check)?;
        let manifest = Self {
            projection,
            generation,
            source_generation,
            watermark,
            dependencies,
            entities,
            relations,
        };
        manifest.validate_checked(check)?;
        Ok(manifest)
    }

    #[hotpath::measure(
        label = "graph_db.generation.replay.hydrate",
        impl_type = "GraphGenerationManifest"
    )]
    pub fn from_replay(
        publication: &GraphPublicationReplayV1,
        provider: &dyn GraphGenerationManifestProvider,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        check()?;
        publication
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let source = checked_decode_replay_source(&publication.canonical_replay_source, check)?;
        let manifest = match source {
            GraphGenerationReplaySource::InlineManifest(manifest) => manifest,
            GraphGenerationReplaySource::MetadataOnlyManifest(_)
            | GraphGenerationReplaySource::SemanticVectorGeneration(_) => {
                return Err(GraphDbError::unavailable(
                    "metadata-only replay requires verified native generation rows",
                ));
            }
            GraphGenerationReplaySource::SealedCodeGeneration(source) => {
                validate_sealed_replay(&source)?;
                provider.hydrate_sealed_code_generation(
                    &publication.key.projection,
                    &source,
                    check,
                )?
            }
        };
        manifest.validate_checked(check)?;
        let projection = &publication.key.projection;
        if projection.namespace.as_str() != manifest.projection.namespace.as_str()
            || projection.projection.as_str() != manifest.projection.projection.as_str()
            || publication.key.generation.as_str() != manifest.generation.as_str()
        {
            return Err(GraphDbError::invalid(
                "canonical graph replay identity does not match its relational key",
            ));
        }
        if publication.direct_dependency_generations
            != manifest.relational_dependency_generations(&projection.shard_id)?
        {
            return Err(GraphDbError::Conflict);
        }
        if publication.dependency_generation_closure_digest.as_str()
            != manifest.dependency_closure_digest(check)?.as_str()
            || publication.expected_recovered_digest.as_str()
                != manifest.expected_recovered_digest(check)?.as_str()
        {
            return Err(GraphDbError::Conflict);
        }
        check()?;
        crate::hotpath_observe::record_counts(
            manifest.entities.len(),
            manifest.relations.len(),
            1,
            0,
        );
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Replay,
        );
        Ok(manifest)
    }

    pub fn from_inline_replay(
        publication: &GraphPublicationReplayV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        Self::from_replay(
            publication,
            &InlineOnlyGraphGenerationManifestProvider,
            check,
        )
    }

    pub fn canonical_replay_source(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<u8>, GraphDbError> {
        self.validate_checked(check)?;
        // Serialize the same externally-tagged `inline_manifest` shape as
        // `GraphGenerationReplaySource::InlineManifest` without cloning the
        // entity/relation payload into an owned enum just to write it.
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        enum InlineReplaySourceView<'a> {
            InlineManifest(&'a GraphGenerationManifest),
        }
        checked_canonical_bytes(
            &InlineReplaySourceView::InlineManifest(self),
            check,
            "canonical graph generation replay",
            MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        )
    }

    pub fn sealed_replay_payload(
        &self,
        source: SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<u8>, GraphDbError> {
        self.validate_checked(check)?;
        validate_sealed_replay(&source)?;
        self.replay_source_payload(
            GraphGenerationReplaySource::SealedCodeGeneration(source),
            check,
        )
    }

    fn replay_source_payload(
        &self,
        source: GraphGenerationReplaySource,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<u8>, GraphDbError> {
        checked_canonical_bytes(
            &source,
            check,
            "canonical graph generation replay",
            MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        )
    }

    pub fn dependency_closure_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
        dependency_closure_digest(&self.dependencies, check)
    }

    /// The metadata half of this manifest, cloned away from its bulk rows.
    #[must_use]
    pub fn identity(&self) -> GraphGenerationManifestIdentity {
        GraphGenerationManifestIdentity {
            projection: self.projection.clone(),
            generation: self.generation.clone(),
            source_generation: self.source_generation.clone(),
            watermark: self.watermark.clone(),
            dependencies: self.dependencies.clone(),
        }
    }

    /// `(entities, relations)` row counts, for observability that must not
    /// keep the rows themselves alive.
    #[must_use]
    pub fn row_counts(&self) -> (usize, usize) {
        (self.entities.len(), self.relations.len())
    }

    pub fn expected_recovered_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        let digest = recovered_generation_digest(self, check)?;
        GraphRecoveredGenerationDigestV1::new(format!("sha256:{digest}"))
            .map_err(|error| GraphDbError::invalid(error.to_string()))
    }

    pub fn relational_replay(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        let payload = self.canonical_replay_source(check)?;
        self.relational_replay_with_payload(
            shard_id,
            idempotency_key,
            input_digest,
            expected_prior_head,
            payload,
            check,
        )
    }

    pub fn relational_sealed_replay(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        source: SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        let payload = self.sealed_replay_payload(source, check)?;
        self.relational_replay_with_payload(
            shard_id,
            idempotency_key,
            input_digest,
            expected_prior_head,
            payload,
            check,
        )
    }

    fn relational_replay_with_payload(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        payload: Vec<u8>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        check()?;
        let projection = GraphProjectionIdentityV1 {
            shard_id,
            namespace: GraphNamespaceV1::new(self.projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(self.projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let direct_dependencies = self.relational_dependency_generations(&projection.shard_id)?;
        let key = GraphPublicationKeyV1::new(
            projection,
            GraphGenerationIdV1::new(self.generation.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            GraphPublicationIdempotencyKeyV1::new(idempotency_key.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        );
        GraphPublicationReplayV1::new(
            key,
            input_digest,
            self.dependency_closure_digest(check)?,
            direct_dependencies,
            expected_prior_head,
            self.expected_recovered_digest(check)?,
            payload,
        )
        .map_err(|error| GraphDbError::invalid(error.to_string()))
    }

    fn relational_dependency_generations(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Result<Vec<GraphDependencyGenerationIdentityV1>, GraphDbError> {
        relational_dependency_generations(&self.dependencies, shard_id)
    }

    pub(crate) fn validate_checked(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        check()?;
        if self.entities.len() > MAX_VERIFIED_GENERATION_ENTITIES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_ENTITIES,
            ));
        }
        if self.relations.len() > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_RELATIONS,
            ));
        }
        let mut dependency_projections = BTreeSet::new();
        for dependency in &self.dependencies {
            check()?;
            if dependency.projection == self.projection {
                return Err(GraphDbError::invalid(
                    "a graph generation cannot depend on its own projection",
                ));
            }
            if !dependency_projections.insert(dependency.projection.clone()) {
                return Err(GraphDbError::invalid(
                    "a graph generation repeats a dependency projection",
                ));
            }
        }
        let mut entity_ids = HashSet::with_capacity(self.entities.len());
        for entity in &self.entities {
            check()?;
            entity.validate()?;
            if !entity_ids.insert(entity.identity.clone()) {
                return Err(GraphDbError::invalid(
                    "a graph generation repeats an entity identity",
                ));
            }
        }
        let allowed_projections = self
            .dependencies
            .iter()
            .map(|dependency| dependency.projection.clone())
            .chain(std::iter::once(self.projection.clone()))
            .collect::<BTreeSet<_>>();
        let mut relation_ids = HashSet::with_capacity(self.relations.len());
        for relation in &self.relations {
            check()?;
            relation.validate()?;
            if !relation_ids.insert(relation.identity.clone()) {
                return Err(GraphDbError::invalid(
                    "a graph generation repeats a relation identity",
                ));
            }
            for endpoint in [&relation.from, &relation.to] {
                if !allowed_projections.contains(&endpoint.projection) {
                    return Err(GraphDbError::invalid(format!(
                        "relation endpoint projection `{}` is not the candidate or an exact dependency",
                        endpoint.projection
                    )));
                }
                if endpoint.projection == self.projection
                    && !entity_ids.contains(&endpoint.identity)
                {
                    return Err(GraphDbError::invalid(format!(
                        "local relation endpoint `{}` is absent from the candidate generation",
                        endpoint.identity
                    )));
                }
            }
        }
        check()?;
        Ok(())
    }
}

fn checked_sorted_dependencies(
    dependencies: Vec<GraphGenerationDependency>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphGenerationDependency>, GraphDbError> {
    let mut sorted = BTreeSet::new();
    for dependency in dependencies {
        check()?;
        if !sorted.insert(dependency) {
            return Err(GraphDbError::invalid(
                "a graph generation repeats a dependency",
            ));
        }
    }
    check()?;
    collect_checked(sorted, check)
}

fn checked_sorted_entities(
    entities: Vec<GraphEntity>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphEntity>, GraphDbError> {
    let mut sorted = BTreeMap::new();
    for entity in entities {
        check()?;
        let identity = entity.identity.clone();
        if sorted.insert(identity, entity).is_some() {
            return Err(GraphDbError::invalid(
                "a graph generation repeats an entity identity",
            ));
        }
    }
    check()?;
    collect_checked(sorted.into_values(), check)
}

fn checked_sorted_relations(
    relations: Vec<GraphGenerationRelation>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphGenerationRelation>, GraphDbError> {
    let mut sorted = BTreeMap::new();
    for relation in relations {
        check()?;
        let identity = relation.identity.clone();
        if sorted.insert(identity, relation).is_some() {
            return Err(GraphDbError::invalid(
                "a graph generation repeats a relation identity",
            ));
        }
    }
    check()?;
    collect_checked(sorted.into_values(), check)
}

fn collect_checked<T>(
    values: impl IntoIterator<Item = T>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<T>, GraphDbError> {
    let mut collected = Vec::new();
    for value in values {
        check()?;
        collected.push(value);
    }
    check()?;
    Ok(collected)
}

/// The dependency-closure digest, shared by the full manifest and its
/// identity so both hash the exact same `dependencies` encoding.
fn dependency_closure_digest(
    dependencies: &[GraphGenerationDependency],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let encoded = serde_json::to_writer(&mut writer, dependencies);
    writer.finish()?;
    encoded.map_err(|error| {
        GraphDbError::invalid(format!(
            "failed to encode graph dependency generation closure: {error}"
        ))
    })?;
    GraphDependencyGenerationClosureDigestV1::new(encode_tagged_lowercase_hex(
        "sha256:",
        &digest.finalize(),
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))
}

fn relational_dependency_generations(
    dependencies: &[GraphGenerationDependency],
    shard_id: &StoreShardIdV1,
) -> Result<Vec<GraphDependencyGenerationIdentityV1>, GraphDbError> {
    dependencies
        .iter()
        .map(|dependency| {
            Ok(GraphDependencyGenerationIdentityV1::new(
                GraphProjectionIdentityV1 {
                    shard_id: shard_id.clone(),
                    namespace: GraphNamespaceV1::new(dependency.projection.namespace.as_str())
                        .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                    projection: GraphProjectionIdV1::new(dependency.projection.projection.as_str())
                        .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                },
                GraphGenerationIdV1::new(dependency.generation.as_str())
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            ))
        })
        .collect()
}

pub(crate) fn physical_namespace(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    generation: &GraphGenerationId,
) -> Result<GraphNamespace, GraphDbError> {
    let encoded = serde_json::to_vec(&(namespace, projection, generation)).map_err(|error| {
        GraphDbError::invalid(format!(
            "failed to encode physical graph generation identity: {error}"
        ))
    })?;
    GraphNamespace::new(format!(
        "generation:{}",
        hex::encode(Sha256::digest(encoded))
    ))
}

pub(crate) fn is_physical_generation_namespace(namespace: &GraphNamespace) -> bool {
    let Some(digest) = namespace.as_str().strip_prefix("generation:") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Streams the stored rows of `identity`'s generation through the
/// recovered-digest proof and compares against `expected`, the digest bound
/// to this exact manifest (a journaled publication's
/// `expected_recovered_digest`, a verified head's digest, or a digest the
/// caller computed from the manifest once). Verification never
/// re-canonicalizes the manifest itself, and reads no manifest row: the
/// expected rows come from the database, so the caller may already have
/// released the bulk `entities`/`relations` vectors.
#[hotpath::measure(label = "graph_db.generation.recover.verify")]
pub(crate) fn verify_recovered_generation(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
    #[cfg(test)]
    RECOVERED_GENERATION_ENUMERATIONS.with(|count| count.set(count.get() + 1));
    check()?;
    let physical_namespace = identity.physical_namespace()?;
    let recovered_commit = latest_projection(
        database,
        &physical_namespace,
        &identity.projection.projection,
    )?
    .ok_or_else(|| GraphDbError::GenerationMismatch {
        namespace: identity.projection.namespace.to_string(),
        projection: identity.projection.projection.to_string(),
        generation: identity.generation.to_string(),
        message: "recovered generation is missing".to_owned(),
    })?
    .commit;
    let expected_dependency_digest = identity.dependency_closure_digest(check)?;
    if recovered_commit.source_generation != identity.source_generation
        || recovered_commit.watermark != identity.watermark
        || recovered_commit.generation_dependency_digest.as_ref()
            != Some(&expected_dependency_digest)
    {
        return Err(GraphDbError::GenerationMismatch {
            namespace: identity.projection.namespace.to_string(),
            projection: identity.projection.projection.to_string(),
            generation: identity.generation.to_string(),
            message:
                "persisted generation source, watermark, or dependency metadata does not match its manifest"
                    .to_owned(),
        });
    }
    let digest = recovered_generation_digest_from_database(database, identity, check)?;
    let actual =
        GraphRecoveredGenerationDigestV1::new(format!("sha256:{digest}")).map_err(|error| {
            GraphDbError::Corrupt {
                message: error.to_string(),
            }
        })?;
    if &actual != expected {
        return Err(GraphDbError::GenerationMismatch {
            namespace: identity.projection.namespace.to_string(),
            projection: identity.projection.projection.to_string(),
            generation: identity.generation.to_string(),
            message: format!(
                "expected recovered digest `{}`, observed `{}`",
                expected.as_str(),
                actual.as_str()
            ),
        });
    }
    Ok(actual)
}

#[cfg(test)]
thread_local! {
    static RECOVERED_GENERATION_ENUMERATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static MANIFEST_CANONICALIZATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static CANONICAL_BUFFER_ALLOCATION_GROWTHS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_recovered_generation_enumerations() {
    RECOVERED_GENERATION_ENUMERATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn recovered_generation_enumerations() -> usize {
    RECOVERED_GENERATION_ENUMERATIONS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_manifest_canonicalizations() {
    MANIFEST_CANONICALIZATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn manifest_canonicalizations() -> usize {
    MANIFEST_CANONICALIZATIONS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_canonical_buffer_allocation_growths() {
    CANONICAL_BUFFER_ALLOCATION_GROWTHS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn canonical_buffer_allocation_growths() -> usize {
    CANONICAL_BUFFER_ALLOCATION_GROWTHS.with(std::cell::Cell::get)
}

pub(crate) fn physical_namespace_projection_map(
    identity: &GraphGenerationManifestIdentity,
) -> Result<BTreeMap<GraphNamespace, GraphProjectionIdentity>, GraphDbError> {
    let mut map = BTreeMap::from([(identity.physical_namespace()?, identity.projection.clone())]);
    for dependency in &identity.dependencies {
        map.insert(
            physical_namespace(
                &dependency.projection.namespace,
                &dependency.projection.projection,
                &dependency.generation,
            )?,
            dependency.projection.clone(),
        );
    }
    Ok(map)
}

pub(crate) fn recovered_entity_ref(
    store: &dyn grafeo_core::graph::GraphStore,
    node: grafeo_common::types::NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
) -> Result<GraphEntityRef, GraphDbError> {
    let entity = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
        message: "recovered generation relation endpoint is missing".to_owned(),
    })?;
    let namespace = GraphNamespace::new(required_string(
        entity.get_property(NAMESPACE_PROPERTY),
        "recovered generation endpoint namespace",
    )?)
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("recovered generation endpoint namespace is invalid: {error}"),
    })?;
    let projection = namespace_projection
        .get(&namespace)
        .cloned()
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "recovered generation relation escapes its dependency closure".to_owned(),
        })?;
    let identity = GraphEntityId::new(required_string(
        entity.get_property(crate::schema::ENTITY_ID_PROPERTY),
        "recovered generation endpoint identity",
    )?)
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("recovered generation endpoint identity is invalid: {error}"),
    })?;
    Ok(GraphEntityRef::new(projection, identity))
}

fn recovered_generation_digest(
    manifest: &GraphGenerationManifest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<String, GraphDbError> {
    #[cfg(test)]
    MANIFEST_CANONICALIZATIONS.with(|count| count.set(count.get() + 1));
    let GraphGenerationManifest {
        projection,
        generation,
        source_generation,
        watermark,
        dependencies,
        entities,
        relations,
    } = manifest;
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    write_generation_identity_frames(
        &mut writer,
        &mut canonical,
        projection,
        generation,
        source_generation,
        watermark,
        dependencies,
    )?;
    for entity in entities {
        write_canonical_frame(
            &mut writer,
            &mut canonical,
            "entity",
            entity,
            "recovered generation entity",
        )?;
    }
    for relation in relations {
        write_canonical_frame(
            &mut writer,
            &mut canonical,
            "relation",
            relation,
            "recovered generation relation",
        )?;
    }
    writer.finish()?;
    Ok(encode_lowercase_hex(&digest.finalize()))
}

/// Writes the leading identity frames of the recovered-generation digest.
///
/// The in-memory manifest proof and the streamed database proof must agree
/// byte for byte, so both go through this one writer: the frame order
/// (`format`, `projection`, `generation`, `source_generation`, `watermark`,
/// `dependencies`) and each frame's canonical encoding live here only.
fn write_generation_identity_frames(
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    projection: &GraphProjectionIdentity,
    generation: &GraphGenerationId,
    source_generation: &SourceGeneration,
    watermark: &GraphWatermark,
    dependencies: &[GraphGenerationDependency],
) -> Result<(), GraphDbError> {
    write_canonical_frame(
        writer,
        canonical,
        "format",
        "tracedecay.graph-generation.v1",
        "recovered generation format",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "projection",
        projection,
        "recovered generation projection",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "generation",
        generation,
        "recovered generation identity",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "source_generation",
        source_generation,
        "recovered source generation",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "watermark",
        watermark,
        "recovered generation watermark",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "dependencies",
        dependencies,
        "recovered generation dependencies",
    )
}

fn write_frame(
    writer: &mut CheckedDigestWriter<'_>,
    tag: &str,
    bytes: &[u8],
) -> Result<(), GraphDbError> {
    let tag_len =
        u64::try_from(tag.len()).map_err(|_| GraphDbError::invalid("digest tag is too large"))?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| GraphDbError::invalid("digest frame is too large"))?;
    write_digest_bytes(writer, &tag_len.to_be_bytes())?;
    write_digest_bytes(writer, tag.as_bytes())?;
    write_digest_bytes(writer, &byte_len.to_be_bytes())?;
    write_digest_bytes(writer, bytes)
}

fn write_canonical_frame<T: Serialize + ?Sized>(
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    tag: &str,
    value: &T,
    subject: &str,
) -> Result<(), GraphDbError> {
    let bytes = canonical.encode(value, subject)?;
    write_frame(writer, tag, bytes)
}

fn write_digest_bytes(
    writer: &mut CheckedDigestWriter<'_>,
    bytes: &[u8],
) -> Result<(), GraphDbError> {
    match writer.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) => Err(writer.take_failure().unwrap_or_else(|| {
            GraphDbError::unavailable(format!("failed to hash recovered generation: {error}"))
        })),
    }
}

struct CheckedDigestWriter<'a> {
    digest: &'a mut Sha256,
    bytes_since_check: u64,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
}

impl<'a> CheckedDigestWriter<'a> {
    fn new(digest: &'a mut Sha256, check: &'a dyn Fn() -> Result<(), GraphDbError>) -> Self {
        Self {
            digest,
            bytes_since_check: 0,
            check,
            failure: None,
        }
    }

    fn finish(mut self) -> Result<(), GraphDbError> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()
    }

    fn take_failure(&mut self) -> Option<GraphDbError> {
        self.failure.take()
    }
}

impl Write for CheckedDigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("digest frame is too large"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(length)
            .ok_or_else(|| io::Error::other("digest check interval overflow"))?;
        if self.bytes_since_check >= DIGEST_CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "generation digest interrupted",
                ));
            }
        }
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CheckedVecWriter<'a> {
    bytes: Vec<u8>,
    bytes_since_check: u64,
    max_bytes: usize,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
    #[cfg(test)]
    allocation_growths: usize,
}

impl<'a> CheckedVecWriter<'a> {
    fn new(
        check: &'a dyn Fn() -> Result<(), GraphDbError>,
        max_bytes: usize,
    ) -> Result<Self, GraphDbError> {
        check()?;
        Ok(Self {
            bytes: Vec::new(),
            bytes_since_check: 0,
            max_bytes,
            check,
            failure: None,
            #[cfg(test)]
            allocation_growths: 0,
        })
    }

    fn finish(mut self) -> Result<Vec<u8>, GraphDbError> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()?;
        Ok(self.bytes)
    }

    fn encode<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
        subject: &str,
    ) -> Result<&[u8], GraphDbError> {
        self.bytes.clear();
        self.bytes_since_check = 0;
        self.failure = None;
        (self.check)()?;
        let encoded = serde_json::to_writer(&mut *self, value);
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()?;
        encoded.map_err(|error| {
            GraphDbError::invalid(format!("failed to encode {subject}: {error}"))
        })?;
        Ok(&self.bytes)
    }

    #[cfg(test)]
    fn allocation_growths(&self) -> usize {
        self.allocation_growths
    }
}

impl Write for CheckedVecWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next_len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("canonical graph replay size overflow"))?;
        if next_len > self.max_bytes {
            return Err(io::Error::other(
                "canonical graph replay exceeds its payload bound",
            ));
        }
        if next_len > self.bytes.capacity() {
            let growth_target = if self.bytes.capacity() == 0 {
                CHECKED_VEC_INITIAL_CAPACITY_BYTES
            } else {
                self.bytes
                    .capacity()
                    .checked_mul(2)
                    .unwrap_or(self.max_bytes)
            };
            let target_capacity = growth_target.max(next_len).min(self.max_bytes);
            let additional = target_capacity
                .checked_sub(self.bytes.len())
                .ok_or_else(|| {
                    io::Error::other("canonical graph replay capacity is below its encoded length")
                })?;
            #[cfg(test)]
            let capacity_before_reserve = self.bytes.capacity();
            if self.bytes.try_reserve_exact(additional).is_err()
                || self.bytes.capacity() > self.max_bytes
            {
                self.failure = Some(GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Write,
                    self.max_bytes,
                ));
                return Err(io::Error::other(
                    "canonical graph replay allocation exceeds its product budget",
                ));
            }
            #[cfg(test)]
            if self.bytes.capacity() != capacity_before_reserve {
                self.allocation_growths += 1;
                CANONICAL_BUFFER_ALLOCATION_GROWTHS.with(|count| {
                    count.set(count.get().saturating_add(1));
                });
            }
        }
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("canonical graph replay chunk is too large"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(length)
            .ok_or_else(|| io::Error::other("canonical replay check interval overflow"))?;
        if self.bytes_since_check >= DIGEST_CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "canonical graph replay interrupted",
                ));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checked_canonical_bytes<T: Serialize + ?Sized>(
    value: &T,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    subject: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, GraphDbError> {
    let mut writer = CheckedVecWriter::new(check, max_bytes)?;
    let encoded = serde_json::to_writer(&mut writer, value);
    let bytes = writer.finish()?;
    encoded
        .map_err(|error| GraphDbError::invalid(format!("failed to encode {subject}: {error}")))?;
    Ok(bytes)
}

#[cfg(test)]
mod checked_vec_writer_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use sha2::{Digest, Sha256};
    use tracedecay_domain::canonical_text::encode_lowercase_hex;

    use super::{
        CheckedVecWriter, GraphDbError, GraphGenerationManifest,
        canonical_buffer_allocation_growths, checked_canonical_bytes, recovered_generation_digest,
        reset_canonical_buffer_allocation_growths,
    };
    use crate::{
        GraphEntity, GraphEntityId, GraphGenerationId, GraphNamespace, GraphProjectionId,
        GraphProjectionIdentity, GraphWatermark, SourceGeneration,
    };

    #[test]
    fn many_tiny_serde_writes_use_bounded_amortized_growth() {
        let value = vec![0_u8; 4_096];
        let mut writer =
            CheckedVecWriter::new(&|| Ok(()), 16 * 1_024).expect("bounded writer initializes");

        serde_json::to_writer(&mut writer, &value).expect("fixture fits the writer bound");
        let allocation_growths = writer.allocation_growths();
        let actual = writer.finish().expect("bounded writer finishes");

        assert_eq!(
            actual,
            serde_json::to_vec(&value).expect("fixture serializes")
        );
        assert_eq!(actual.len(), 8_193);
        assert_eq!(
            encode_lowercase_hex(&Sha256::digest(&actual)),
            "cb113f74dc19a08fcacd246b84ca69e1dff17209792ea3bd8d1b34397f5eca92"
        );
        assert!(
            allocation_growths <= 16,
            "4,096 tiny values caused {allocation_growths} allocation growths"
        );
    }

    #[test]
    fn canonical_bytes_refuse_the_first_byte_past_the_bound() {
        let value = vec![0_u8; 4_096];

        let error = checked_canonical_bytes(&value, &|| Ok(()), "bounded fixture", 8_192)
            .expect_err("8,193 encoded bytes must exceed the bound");

        assert!(matches!(
            error,
            GraphDbError::InvalidRequest { message }
                if message.contains("canonical graph replay exceeds its payload bound")
        ));
    }

    #[test]
    fn writer_capacity_never_exceeds_its_payload_bound() {
        let value = vec![0_u8; 4_096];
        let max_bytes = 8_192;
        let mut writer =
            CheckedVecWriter::new(&|| Ok(()), max_bytes).expect("bounded writer initializes");

        let error = serde_json::to_writer(&mut writer, &value)
            .expect_err("the final encoded byte must be refused");

        assert!(error.to_string().contains("canonical graph replay exceeds"));
        assert!(writer.bytes.len() <= max_bytes);
        assert!(writer.bytes.capacity() <= max_bytes);
    }

    #[test]
    fn recovered_digest_reuses_canonical_buffer_across_manifest_rows() {
        let manifest = GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("allocation-probe").unwrap(),
                GraphProjectionId::new("manifest").unwrap(),
            ),
            GraphGenerationId::new("generation-allocation-probe").unwrap(),
            SourceGeneration::new("source-allocation-probe").unwrap(),
            GraphWatermark::new("watermark-allocation-probe").unwrap(),
            vec![],
            (0..4_096)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap();

        reset_canonical_buffer_allocation_growths();
        let digest = recovered_generation_digest(&manifest, &|| Ok(())).unwrap();
        let allocation_growths = canonical_buffer_allocation_growths();
        assert_eq!(
            digest,
            "786f46a4a0f263e5c67927f2a196ce95bd7071733478fb94560c5736dce44f9f"
        );

        assert!(
            allocation_growths <= 8,
            "4,096 manifest rows caused {allocation_growths} canonical-buffer allocation growths"
        );
    }
}
