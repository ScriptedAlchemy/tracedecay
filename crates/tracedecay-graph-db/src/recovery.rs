use std::collections::BTreeSet;
use std::io;

use grafeo_common::types::Value;
use grafeo_common::utils::error::ErrorCode;
use grafeo_engine::GrafeoDB;

use crate::error::rollback_failure;
use crate::location::ValidatedOpen;
use crate::schema::{
    FINAL_SCHEMA, FORMAT_LABEL, FORMAT_VERSION_PROPERTY, INDEXED_PROPERTIES, NAMESPACE_PROPERTY,
    PROJECTION_PROPERTY, SCHEMA_PROPERTY, SEQUENCE_PROPERTY, required_string,
};
use crate::state::FormatState;
use crate::{GraphCommit, GraphDbError, GraphDurability, GraphNamespace, GraphProjectionId};

const QUARANTINE_LABEL: &str = "__tracedecay_graph_db_recovery_quarantine";
const QUARANTINE_KEY_LABEL_PREFIX: &str = "__tracedecay_graph_db_recovery_quarantine_key_";

/// A freshly reopened graph database together with its loaded format state
/// and the set of quarantined projections read from the store.
pub(crate) type RecoveredDatabase = (
    GrafeoDB,
    FormatState,
    BTreeSet<(GraphNamespace, GraphProjectionId)>,
);

#[derive(Clone, Debug)]
pub struct VerifiedGraphCommit {
    pub commit: GraphCommit,
    pub head: tracedecay_store::runtime::GraphVerifiedHeadV1,
    pub recovered_digest: tracedecay_store::runtime::GraphRecoveredGenerationDigestV1,
    pub snapshot: crate::VerifiedGraphSnapshot,
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
) -> Result<RecoveredDatabase, GraphDbError> {
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
) -> Result<RecoveredDatabase, GraphDbError> {
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
) -> Result<RecoveredDatabase, GraphDbError> {
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
