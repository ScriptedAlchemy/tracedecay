use std::collections::BTreeSet;
use std::io::{self, Write};

use serde::Serialize;
use sha2::{Digest, Sha256};

use grafeo_common::types::Value;
use grafeo_common::utils::error::ErrorCode;
use grafeo_engine::GrafeoDB;

use crate::error::rollback_failure;
use crate::location::ValidatedOpen;
use crate::schema::{
    FINAL_SCHEMA, FORMAT_LABEL, FORMAT_VERSION_PROPERTY, INDEXED_PROPERTIES, NAMESPACE_PROPERTY,
    PROJECTION_PROPERTY, SCHEMA_PROPERTY, SEQUENCE_PROPERTY, required_string,
};
use crate::state::{
    FormatState, latest_projection, load_entity_by_node, projection_entities_checked,
    projection_relations_checked,
};
use crate::{
    GraphCommit, GraphDbError, GraphDurability, GraphEntity, GraphNamespace, GraphProjectionId,
    GraphRelation, GraphWatermark, SourceGeneration,
};

const QUARANTINE_LABEL: &str = "__tracedecay_graph_db_recovery_quarantine";
const QUARANTINE_KEY_LABEL_PREFIX: &str = "__tracedecay_graph_db_recovery_quarantine_key_";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredProjectionDigest(String);

impl RecoveredProjectionDigest {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredProjectionManifest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub entity_count: u64,
    pub relation_count: u64,
    pub digest: RecoveredProjectionDigest,
}

impl RecoveredProjectionManifest {
    pub fn new(
        namespace: GraphNamespace,
        projection: GraphProjectionId,
        source_generation: SourceGeneration,
        watermark: GraphWatermark,
        entities: &[GraphEntity],
        relations: &[GraphRelation],
    ) -> Result<Self, GraphDbError> {
        validate_manifest_identities(entities, relations)?;
        let entity_count = u64::try_from(entities.len())
            .map_err(|_| GraphDbError::invalid("projection entity count exceeds u64"))?;
        let relation_count = u64::try_from(relations.len())
            .map_err(|_| GraphDbError::invalid("projection relation count exceeds u64"))?;
        Ok(Self {
            digest: recovered_projection_digest(
                &namespace,
                &projection,
                &source_generation,
                &watermark,
                entities,
                relations,
                &|| Ok(()),
            )?,
            namespace,
            projection,
            source_generation,
            watermark,
            entity_count,
            relation_count,
        })
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedGraphCommit {
    pub commit: GraphCommit,
    pub head: tracedecay_store::runtime::GraphVerifiedHeadV1,
    pub recovered_digest: tracedecay_store::runtime::GraphRecoveredGenerationDigestV1,
    pub snapshot: crate::VerifiedGraphSnapshot,
}

pub(crate) fn recovered_projection_digest(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    source_generation: &SourceGeneration,
    watermark: &GraphWatermark,
    entities: &[GraphEntity],
    relations: &[GraphRelation],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<RecoveredProjectionDigest, GraphDbError> {
    check()?;
    let mut entities = entities.iter().collect::<Vec<_>>();
    let mut relations = relations.iter().collect::<Vec<_>>();
    entities.sort_by(|left, right| left.identity.cmp(&right.identity));
    relations.sort_by(|left, right| left.identity.cmp(&right.identity));
    let mut digest = Sha256::new();
    hash_bytes_frame(
        &mut digest,
        0,
        b"tracedecay.graph-db.recovered-projection.v2",
    );
    hash_json_frame(&mut digest, 1, namespace, check)?;
    hash_json_frame(&mut digest, 2, projection, check)?;
    hash_json_frame(&mut digest, 3, source_generation, check)?;
    hash_json_frame(&mut digest, 4, watermark, check)?;
    hash_json_frame(
        &mut digest,
        5,
        &u64::try_from(entities.len())
            .map_err(|_| GraphDbError::invalid("projection entity count exceeds u64"))?,
        check,
    )?;
    for entity in entities {
        hash_json_frame(&mut digest, 6, entity, check)?;
    }
    hash_json_frame(
        &mut digest,
        7,
        &u64::try_from(relations.len())
            .map_err(|_| GraphDbError::invalid("projection relation count exceeds u64"))?,
        check,
    )?;
    for relation in relations {
        hash_json_frame(&mut digest, 8, relation, check)?;
    }
    check()?;
    Ok(RecoveredProjectionDigest(hex::encode(digest.finalize())))
}

fn validate_manifest_identities(
    entities: &[GraphEntity],
    relations: &[GraphRelation],
) -> Result<(), GraphDbError> {
    let mut entity_ids = BTreeSet::new();
    for entity in entities {
        entity.validate()?;
        if !entity_ids.insert(&entity.identity) {
            return Err(GraphDbError::invalid(
                "recovered projection manifest repeats an entity identity",
            ));
        }
    }
    let mut relation_ids = BTreeSet::new();
    for relation in relations {
        relation.validate()?;
        if !relation_ids.insert(&relation.identity) {
            return Err(GraphDbError::invalid(
                "recovered projection manifest repeats a relation identity",
            ));
        }
    }
    Ok(())
}

fn hash_json_frame(
    digest: &mut Sha256,
    tag: u8,
    value: &impl Serialize,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    check()?;
    let mut counter = CheckedCounter::new(check);
    let counted = serde_json::to_writer(&mut counter, value);
    if let Some(error) = counter.failure {
        return Err(error);
    }
    counted.map_err(canonicalization_error)?;
    digest.update([tag]);
    digest.update(counter.bytes.to_be_bytes());
    let mut writer = CheckedDigestWriter::new(digest, check);
    let written = serde_json::to_writer(&mut writer, value);
    if let Some(error) = writer.failure {
        return Err(error);
    }
    written.map_err(canonicalization_error)?;
    check()
}

fn hash_bytes_frame(digest: &mut Sha256, tag: u8, bytes: &[u8]) {
    digest.update([tag]);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn canonicalization_error(error: serde_json::Error) -> GraphDbError {
    GraphDbError::invalid(format!(
        "failed to canonicalize recovered projection: {error}"
    ))
}

const CHECK_INTERVAL_BYTES: u64 = 64 * 1024;

struct CheckedCounter<'a> {
    bytes: u64,
    bytes_since_check: u64,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
}

impl<'a> CheckedCounter<'a> {
    fn new(check: &'a dyn Fn() -> Result<(), GraphDbError>) -> Self {
        Self {
            bytes: 0,
            bytes_since_check: 0,
            check,
            failure: None,
        }
    }
}

impl Write for CheckedCounter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("canonical recovery frame exceeds u64"))?;
        self.bytes = self
            .bytes
            .checked_add(written)
            .ok_or_else(|| io::Error::other("canonical recovery frame exceeds u64"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(written)
            .ok_or_else(|| io::Error::other("canonical recovery check interval exceeds u64"))?;
        if self.bytes_since_check >= CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "recovered projection digest interrupted",
                ));
            }
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
}

impl Write for CheckedDigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("canonical recovery frame exceeds u64"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(written)
            .ok_or_else(|| io::Error::other("canonical recovery check interval exceeds u64"))?;
        if self.bytes_since_check >= CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "recovered projection digest interrupted",
                ));
            }
        }
        self.digest.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn verify_recovered_projection(
    database: &GrafeoDB,
    manifest: &RecoveredProjectionManifest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphCommit, RecoveredProjectionDigest), GraphDbError> {
    check()?;
    let projection = latest_projection(database, &manifest.namespace, &manifest.projection)?
        .ok_or_else(|| {
            projection_mismatch(
                &manifest.namespace,
                &manifest.projection,
                "recovered projection is missing",
            )
        })?;
    check()?;
    if projection.commit.source_generation != manifest.source_generation
        || projection.commit.watermark != manifest.watermark
    {
        return Err(projection_mismatch(
            &manifest.namespace,
            &manifest.projection,
            "recovered projection generation or watermark does not match its manifest",
        ));
    }
    let stored_entities =
        projection_entities_checked(database, &manifest.namespace, &manifest.projection, check)?;
    let mut entity_ids = BTreeSet::new();
    let mut entities = Vec::with_capacity(stored_entities.len());
    for stored in stored_entities {
        check()?;
        if stored.namespace != manifest.namespace || stored.projection != manifest.projection {
            return Err(GraphDbError::Corrupt {
                message: "recovered entity projection index does not match scalar ownership"
                    .to_owned(),
            });
        }
        if !entity_ids.insert(stored.entity.identity.clone()) {
            return Err(GraphDbError::Corrupt {
                message: "recovered projection repeats an entity identity".to_owned(),
            });
        }
        entities.push(stored.entity);
    }
    check()?;
    let stored_relations =
        projection_relations_checked(database, &manifest.namespace, &manifest.projection, check)?;
    let mut relation_ids = BTreeSet::new();
    let mut relations = Vec::with_capacity(stored_relations.len());
    for stored in stored_relations {
        check()?;
        let edge = database
            .graph_store()
            .get_edge(stored.edge)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "recovered relation edge is unreadable".to_owned(),
            })?;
        let source = load_entity_by_node(database, edge.src)?;
        if source.namespace != manifest.namespace || stored.projection != manifest.projection {
            return Err(GraphDbError::Corrupt {
                message: "recovered relation projection index does not match scalar ownership"
                    .to_owned(),
            });
        }
        if !relation_ids.insert(stored.relation.identity.clone()) {
            return Err(GraphDbError::Corrupt {
                message: "recovered projection repeats a relation identity".to_owned(),
            });
        }
        relations.push(stored.relation);
    }
    check()?;
    if u64::try_from(entities.len()).ok() != Some(manifest.entity_count)
        || u64::try_from(relations.len()).ok() != Some(manifest.relation_count)
    {
        return Err(projection_mismatch(
            &manifest.namespace,
            &manifest.projection,
            "recovered projection counts do not match its manifest",
        ));
    }
    let digest = recovered_projection_digest(
        &manifest.namespace,
        &manifest.projection,
        &manifest.source_generation,
        &manifest.watermark,
        &entities,
        &relations,
        check,
    )?;
    check()?;
    if digest != manifest.digest {
        return Err(projection_mismatch(
            &manifest.namespace,
            &manifest.projection,
            "recovered projection digest does not match its manifest",
        ));
    }
    Ok((projection.commit, digest))
}

pub(crate) fn projection_mismatch(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    message: impl Into<String>,
) -> GraphDbError {
    GraphDbError::ProjectionMismatch {
        namespace: namespace.as_str().to_owned(),
        projection: projection.as_str().to_owned(),
        message: message.into(),
    }
}

pub(crate) fn open_recovered_database(
    reopen: &ValidatedOpen,
) -> Result<
    (
        GrafeoDB,
        FormatState,
        BTreeSet<(GraphNamespace, GraphProjectionId)>,
    ),
    GraphDbError,
> {
    let recovered = GrafeoDB::with_config(reopen.config.clone()).map_err(|error| {
        GraphDbError::DurabilityUncertain {
            message: format!(
                "Grafeo reopen failed during recovered projection verification: {error}"
            ),
        }
    })?;
    if let Err(error) = validate_or_initialize_format(&recovered, reopen) {
        return close_recovered_after_error("validate recovered graph format", recovered, error);
    }
    let state = match FormatState::load(&recovered) {
        Ok(state) => state,
        Err(error) => {
            return close_recovered_after_error("load recovered graph state", recovered, error);
        }
    };
    let quarantined = match load_quarantined_projections(&recovered) {
        Ok(quarantined) => quarantined,
        Err(error) => {
            return close_recovered_after_error(
                "load recovered graph quarantines",
                recovered,
                error,
            );
        }
    };
    Ok((recovered, state, quarantined))
}

pub(crate) fn validate_or_initialize_format(
    database: &GrafeoDB,
    validated: &ValidatedOpen,
) -> Result<(), GraphDbError> {
    let store = database.graph_store();
    let markers = store.nodes_by_label(FORMAT_LABEL);
    if markers.is_empty() {
        if store.node_count() != 0 || validated.preexisting_store {
            return Err(GraphDbError::ResetRequired {
                message: "existing Grafeo store has no TraceDecay format marker".to_owned(),
            });
        }
        let mut session = database.session();
        session
            .begin_transaction()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let version = i64::from(validated.expected_format.get());
        if let Err(error) = session.create_node_with_props(
            &[FORMAT_LABEL],
            [
                (FORMAT_VERSION_PROPERTY, Value::from(version)),
                (SCHEMA_PROPERTY, Value::from(FINAL_SCHEMA)),
                (SEQUENCE_PROPERTY, Value::from(0_i64)),
            ],
        ) {
            return match session.rollback() {
                Ok(()) => Err(GraphDbError::unavailable(error.to_string())),
                Err(rollback_error) => Err(rollback_failure(
                    "format initialization",
                    error,
                    rollback_error,
                )),
            };
        }
        session
            .commit()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        for property in INDEXED_PROPERTIES {
            database.create_property_index(property);
        }
        if validated.durability == GraphDurability::WalSync
            && let Err(error) = crate::runtime::sync_wal(database)
        {
            return Err(error);
        }
        return Ok(());
    }
    if markers.len() != 1 {
        return Err(GraphDbError::ResetRequired {
            message: "TraceDecay format marker count is not exactly one".to_owned(),
        });
    }
    let marker = store
        .get_node(markers[0])
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "TraceDecay format marker is unreadable".to_owned(),
        })?;
    let actual = marker
        .get_property(FORMAT_VERSION_PROPERTY)
        .and_then(Value::as_int64);
    if actual != Some(i64::from(validated.expected_format.get())) {
        return Err(GraphDbError::ResetRequired {
            message: format!(
                "TraceDecay graph format mismatch: expected {}, found {actual:?}",
                validated.expected_format.get()
            ),
        });
    }
    if marker.get_property(SCHEMA_PROPERTY).and_then(Value::as_str) != Some(FINAL_SCHEMA) {
        return Err(GraphDbError::ResetRequired {
            message: "TraceDecay graph schema is not the final native scalar schema".to_owned(),
        });
    }
    Ok(())
}

fn close_recovered_after_error<T>(
    context: &str,
    recovered: GrafeoDB,
    primary: GraphDbError,
) -> Result<T, GraphDbError> {
    match recovered.close() {
        Ok(()) => Err(primary),
        Err(close_error) => Err(rollback_failure(context, primary, close_error)),
    }
}

pub(crate) fn checkpoint_recovered_database(
    recovered: GrafeoDB,
    reopen: &ValidatedOpen,
) -> Result<
    (
        GrafeoDB,
        FormatState,
        BTreeSet<(GraphNamespace, GraphProjectionId)>,
    ),
    GraphDbError,
> {
    recovered
        .close()
        .map_err(|error| GraphDbError::DurabilityUncertain {
            message: format!(
                "Grafeo close failed while checkpointing projection quarantine: {error}"
            ),
        })?;
    open_recovered_database(reopen)
}

pub(crate) fn requarantine_after_failed_checkpoint_verification(
    recovered: GrafeoDB,
    reopen: &ValidatedOpen,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    verification_error: &GraphDbError,
) -> Result<
    (
        GrafeoDB,
        FormatState,
        BTreeSet<(GraphNamespace, GraphProjectionId)>,
    ),
    GraphDbError,
> {
    set_projection_quarantine(&recovered, namespace, projection, true)
        .and_then(|()| crate::runtime::sync_wal(&recovered))
        .map_err(|error| {
            rollback_failure(
                "restore recovered projection quarantine",
                verification_error,
                quarantine_transition_failure(
                    "persist recovered projection quarantine after failed verification",
                    error,
                ),
            )
        })?;
    let (recovered, state, quarantined) = checkpoint_recovered_database(recovered, reopen)
        .map_err(|error| {
            rollback_failure(
                "restore recovered projection quarantine",
                verification_error,
                error,
            )
        })?;
    if !quarantined.contains(&(namespace.clone(), projection.clone())) {
        return Err(rollback_failure(
            "restore recovered projection quarantine",
            verification_error,
            GraphDbError::DurabilityUncertain {
                message: "recovered projection quarantine disappeared after checkpoint".to_owned(),
            },
        ));
    }
    Ok((recovered, state, quarantined))
}

pub(crate) fn load_quarantined_projections(
    database: &GrafeoDB,
) -> Result<BTreeSet<(GraphNamespace, GraphProjectionId)>, GraphDbError> {
    let store = database.graph_store();
    let mut quarantined = BTreeSet::new();
    for node_id in store.nodes_by_label(QUARANTINE_LABEL) {
        let record = store
            .get_node(node_id)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "projection quarantine marker is unreadable".to_owned(),
            })?;
        let namespace = GraphNamespace::new(required_string(
            record.get_property(NAMESPACE_PROPERTY),
            "projection quarantine namespace",
        )?)
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("invalid projection quarantine namespace: {error}"),
        })?;
        let projection = GraphProjectionId::new(required_string(
            record.get_property(PROJECTION_PROPERTY),
            "projection quarantine identity",
        )?)
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("invalid projection quarantine identity: {error}"),
        })?;
        let key_label = quarantine_key_label(&namespace, &projection);
        if !record.has_label(&key_label) {
            return Err(GraphDbError::Corrupt {
                message: "projection quarantine marker has no exact native key".to_owned(),
            });
        }
        if !quarantined.insert((namespace, projection)) {
            return Err(GraphDbError::Corrupt {
                message: "projection quarantine marker is duplicated".to_owned(),
            });
        }
    }
    Ok(quarantined)
}

pub(crate) fn set_projection_quarantine(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    quarantined: bool,
) -> Result<(), GraphDbError> {
    let key_label = quarantine_key_label(namespace, projection);
    let store = database.graph_store();
    let mut markers = store.nodes_by_label(&key_label).into_iter().filter(|node| {
        store
            .get_node(*node)
            .is_some_and(|record| record.has_label(QUARANTINE_LABEL))
    });
    let existing = markers.next();
    if markers.next().is_some() {
        return Err(GraphDbError::Corrupt {
            message: "projection quarantine marker is duplicated".to_owned(),
        });
    }
    if quarantined == existing.is_some() {
        return Ok(());
    }

    let mut session = database.session();
    session
        .begin_transaction()
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    let mutation = if quarantined {
        session
            .create_node_with_props(
                &[QUARANTINE_LABEL, &key_label],
                [
                    (NAMESPACE_PROPERTY, Value::from(namespace.as_str())),
                    (PROJECTION_PROPERTY, Value::from(projection.as_str())),
                ],
            )
            .map(|_| ())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))
    } else {
        session
            .execute(&format!(
                "MATCH (n:{QUARANTINE_LABEL}) WHERE id(n) = {} DELETE n",
                existing
                    .ok_or_else(|| GraphDbError::Corrupt {
                        message: "projection quarantine marker disappeared".to_owned(),
                    })?
                    .as_u64()
            ))
            .map(|_| ())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))
    };
    if let Err(error) = mutation {
        return match session.rollback() {
            Ok(()) => Err(error),
            Err(rollback) => Err(rollback_failure(
                "change projection quarantine",
                error,
                rollback,
            )),
        };
    }
    session
        .commit()
        .map_err(|error| GraphDbError::DurabilityUncertain {
            message: format!(
                "projection quarantine commit failed; durable outcome cannot be established: {error}"
            ),
        })
}

fn quarantine_key_label(namespace: &GraphNamespace, projection: &GraphProjectionId) -> String {
    format!(
        "{QUARANTINE_KEY_LABEL_PREFIX}{}_{}",
        hex::encode(namespace.as_str().as_bytes()),
        hex::encode(projection.as_str().as_bytes())
    )
}

pub(crate) fn quarantine_transition_failure(context: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: format!("{context} failed: {error}"),
    }
}

pub(crate) fn is_database_fault(error: &GraphDbError) -> bool {
    matches!(
        error,
        GraphDbError::ResetRequired { .. }
            | GraphDbError::Corrupt { .. }
            | GraphDbError::DurabilityUncertain { .. }
    )
}

pub(crate) fn map_open_error(
    error: grafeo_common::utils::error::Error,
    preexisting_store: bool,
) -> GraphDbError {
    let malformed_io = matches!(
        &error,
        grafeo_common::utils::error::Error::Io(io)
            if preexisting_store
                && matches!(
                    io.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                )
    );
    let message = error.to_string();
    if malformed_io {
        return GraphDbError::Corrupt { message };
    }
    match error.error_code() {
        ErrorCode::StorageCorrupted
        | ErrorCode::StorageRecoveryFailed
        | ErrorCode::SerializationError
            if preexisting_store =>
        {
            GraphDbError::Corrupt { message }
        }
        _ => GraphDbError::unavailable(message),
    }
}
