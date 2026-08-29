use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use grafeo_common::types::Value;
use grafeo_common::utils::error::ErrorCode;
use grafeo_engine::GrafeoDB;

use crate::error::rollback_failure;
use crate::location::ValidatedOpen;
use crate::schema::{
    FINAL_SCHEMA, FORMAT_LABEL, FORMAT_VERSION_PROPERTY, INDEXED_PROPERTIES, NAMESPACE_PROPERTY,
    PROJECTION_PROPERTY, QUARANTINE_KEY_PROPERTY, SCHEMA_PROPERTY, SEQUENCE_PROPERTY,
    nodes_with_label, required_string,
};
use crate::state::FormatState;
use crate::{GraphCommit, GraphDbError, GraphDurability, GraphNamespace, GraphProjectionId};

const QUARANTINE_LABEL: &str = "__tracedecay_graph_db_recovery_quarantine";

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

#[hotpath::measure(label = "graph_db.generation.recover.open")]
pub(crate) fn open_recovered_database(
    reopen: &ValidatedOpen,
) -> Result<RecoveredDatabase, GraphDbError> {
    let recovered = hotpath::measure_block!(
        "graph_db.generation.recover.open.engine",
        GrafeoDB::with_config(reopen.config.clone()).map_err(|error| {
            GraphDbError::DurabilityUncertain {
                message: format!(
                    "Grafeo reopen failed during recovered projection verification: {error}"
                ),
            }
        })
    )?;
    record_open_corpus_gauges(&recovered);
    if let Err(error) = validate_or_initialize_format(&recovered, reopen) {
        return close_recovered_after_error("validate recovered graph format", recovered, error);
    }
    let state = match hotpath::measure_block!(
        "graph_db.generation.open.state",
        FormatState::load(&recovered)
    ) {
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
    collapse_replayed_wal(&recovered);
    Ok((recovered, state, quarantined))
}

pub(crate) fn validate_or_initialize_format(
    database: &GrafeoDB,
    validated: &ValidatedOpen,
) -> Result<(), GraphDbError> {
    hotpath::measure_block!(
        "graph_db.generation.open.format",
        validate_or_initialize_format_marker(database, validated)
    )?;
    // The pinned grafeo fork persists the store's property-index keys in the
    // catalog section and `load_from_sections` rebuilds each one with a full
    // node scan inside `GrafeoDB::with_config`, so on a store checkpointed by
    // this engine these calls find the index present and return without
    // scanning. This loop stays as the authority that *defines* the index
    // set: a fresh store and a store whose last checkpoint predates catalog
    // index persistence register here, and without the indexes the unique-key
    // lookups in `state.rs` degrade from a hash hit to a full node scan —
    // measured at 500k entities, 64 point reads took 23.7s instead of 1.4ms,
    // and the bounded traversal 773ms instead of 2.7ms. The span shows which
    // side of the engine boundary the rebuild cost actually lands on.
    hotpath::measure_block!("graph_db.generation.open.property_indexes", {
        for property in INDEXED_PROPERTIES {
            database.create_property_index(property);
        }
    });
    Ok(())
}

fn validate_or_initialize_format_marker(
    database: &GrafeoDB,
    validated: &ValidatedOpen,
) -> Result<(), GraphDbError> {
    let store = database.graph_store();
    let markers = nodes_with_label(store.as_ref(), FORMAT_LABEL);
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

/// Records how much corpus the engine open just hydrated, so the phase spans
/// around it can be correlated with store size in a Hotpath report.
#[inline(always)]
pub(crate) fn record_open_corpus_gauges(database: &GrafeoDB) {
    #[cfg(feature = "hotpath")]
    {
        let store = database.graph_store();
        hotpath::gauge!("graph_db.generation.open.nodes").set(store.node_count());
        hotpath::gauge!("graph_db.generation.open.edges").set(store.edge_count());
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = database;
}

/// Checkpoints sidecar-WAL history that a successful open replayed into the
/// live store, then removes only segments that the checkpoint makes
/// unreachable. Open remains available if this optimization fails: the
/// durable WAL still contains the same history and a later open can retry.
pub(crate) fn collapse_replayed_wal(database: &GrafeoDB) {
    let status = database.wal_status();
    if !status.enabled {
        return;
    }
    let Some(file_manager) = database.file_manager() else {
        return;
    };
    let sidecar = file_manager.sidecar_wal_path();
    let Some(newest_segment) = newest_wal_segment_sequence(&sidecar) else {
        return;
    };
    let checkpointed_sequence = grafeo_storage::wal::WalRecovery::new(&sidecar)
        .checkpoint()
        .map(|metadata| metadata.log_sequence);
    if let Some(sequence) = checkpointed_sequence
        && sequence >= newest_segment
    {
        let removed_segments = remove_checkpoint_covered_segments(&sidecar, sequence);
        if removed_segments > 0 {
            tracing::info!(
                event = "graph_wal_segments_removed",
                phase = "open",
                removed_segments,
                "removed checkpoint-covered WAL segments left by an earlier collapse"
            );
        }
        return;
    }

    match hotpath::measure_block!(
        "graph_db.generation.open.wal_checkpoint",
        database.wal_checkpoint()
    ) {
        Ok(()) => {
            let removed_segments = grafeo_storage::wal::WalRecovery::new(&sidecar)
                .checkpoint()
                .map_or(0, |metadata| {
                    remove_checkpoint_covered_segments(&sidecar, metadata.log_sequence)
                });
            tracing::info!(
                event = "graph_wal_checkpoint",
                phase = "open",
                wal_bytes = status.size_bytes,
                removed_segments,
                "collapsed replayed WAL history into the graph container"
            );
        }
        Err(error) => tracing::warn!(
            event = "graph_wal_checkpoint_failed",
            phase = "open",
            wal_bytes = status.size_bytes,
            %error,
            "replayed WAL history could not be checkpointed; the next open will retry"
        ),
    }
}

/// Delete segments that recovery skips because their sequence precedes the
/// durable checkpoint. Keep the checkpoint segment and two preceding files,
/// matching Grafeo's own rotation safety margin.
fn remove_checkpoint_covered_segments(sidecar: &Path, covered_below: u64) -> u64 {
    let Ok(entries) = std::fs::read_dir(sidecar) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(sequence) = name
            .to_str()
            .and_then(|name| name.strip_prefix("wal_"))
            .and_then(|name| name.strip_suffix(".log"))
            .and_then(|name| name.parse::<u64>().ok())
        else {
            continue;
        };
        if sequence + 2 < covered_below && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn newest_wal_segment_sequence(sidecar: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(sidecar).ok()?;
    entries
        .flatten()
        .filter(|entry| entry.metadata().is_ok_and(|metadata| metadata.len() > 0))
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.strip_prefix("wal_")?
                .strip_suffix(".log")?
                .parse::<u64>()
                .ok()
        })
        .max()
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

#[hotpath::measure(label = "graph_db.generation.recover.checkpoint")]
pub(crate) fn checkpoint_recovered_database(
    recovered: GrafeoDB,
    reopen: &ValidatedOpen,
) -> Result<RecoveredDatabase, GraphDbError> {
    hotpath::measure_block!(
        "graph_db.generation.recover.checkpoint.close",
        recovered.close()
    )
    .map_err(|error| GraphDbError::DurabilityUncertain {
        message: format!("Grafeo close failed while checkpointing projection quarantine: {error}"),
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

#[hotpath::measure(label = "graph_db.generation.open.quarantine")]
pub(crate) fn load_quarantined_projections(
    database: &GrafeoDB,
) -> Result<BTreeSet<(GraphNamespace, GraphProjectionId)>, GraphDbError> {
    let store = database.graph_store();
    let mut quarantined = BTreeSet::new();
    for node_id in nodes_with_label(store.as_ref(), QUARANTINE_LABEL) {
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
        if record.get_property(QUARANTINE_KEY_PROPERTY)
            != Some(&quarantine_key_value(&namespace, &projection))
        {
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
    let key_value = quarantine_key_value(namespace, projection);
    let store = database.graph_store();
    let mut markers = store
        .find_nodes_by_property(QUARANTINE_KEY_PROPERTY, &key_value)
        .into_iter()
        .filter(|node| {
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
                &[QUARANTINE_LABEL],
                [
                    (NAMESPACE_PROPERTY, Value::from(namespace.as_str())),
                    (PROJECTION_PROPERTY, Value::from(projection.as_str())),
                    (QUARANTINE_KEY_PROPERTY, key_value.clone()),
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

/// The indexed unique-key value for one projection quarantine marker.
///
/// A property rather than a label for the same reason entity identity is: one
/// native label per record becomes one columnar node table per record.
fn quarantine_key_value(namespace: &GraphNamespace, projection: &GraphProjectionId) -> Value {
    Value::from(format!(
        "{}_{}",
        hex::encode(namespace.as_str().as_bytes()),
        hex::encode(projection.as_str().as_bytes())
    ))
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

#[cfg(test)]
mod tests {
    use super::remove_checkpoint_covered_segments;

    #[test]
    fn removes_only_segments_the_checkpoint_covers() {
        let sidecar = tempfile::TempDir::new().unwrap();
        for sequence in 0..=5u64 {
            std::fs::write(sidecar.path().join(format!("wal_{sequence:08}.log")), b"x").unwrap();
        }
        std::fs::write(sidecar.path().join("checkpoint.meta"), b"meta").unwrap();

        let removed = remove_checkpoint_covered_segments(sidecar.path(), 5);

        assert_eq!(removed, 3, "segments 0, 1, and 2 are checkpoint-covered");
        let mut remaining: Vec<String> = std::fs::read_dir(sidecar.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            [
                "checkpoint.meta",
                "wal_00000003.log",
                "wal_00000004.log",
                "wal_00000005.log",
            ]
        );
    }
}
