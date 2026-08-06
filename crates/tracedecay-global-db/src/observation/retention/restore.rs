use tracedecay_runtime_core::errors::Result;

use super::{
    ANCHOR_RELEASED_MARKER, CREATE_ANCHOR_UPDATE_TRIGGER, CREATE_OBSERVATION_UPDATE_TRIGGER,
    CREATE_PROVENANCE_UPDATE_TRIGGER, DROP_ANCHOR_UPDATE_TRIGGER, DROP_OBSERVATION_UPDATE_TRIGGER,
    DROP_PROVENANCE_UPDATE_TRIGGER, OBSERVATION_RELEASED_MARKER, PROVENANCE_RELEASED_MARKER,
    db_error,
};

/// Reapplies already-authorized release markers while publishing an older
/// physical restore. The caller must attach the quiesced current database as
/// `current_authority` and hold the staging write transaction.
pub fn replay_current_release_state_for_restore(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<()> {
    transaction
        .execute_batch(&format!(
            "{DROP_ANCHOR_UPDATE_TRIGGER};
             {DROP_OBSERVATION_UPDATE_TRIGGER};
             {DROP_PROVENANCE_UPDATE_TRIGGER};"
        ))
        .map_err(db_error)?;
    transaction
        .execute_batch(&format!(
            "UPDATE main.retrieval_anchors AS staging
             SET anchor_json = (
                 SELECT current.anchor_json
                 FROM current_authority.retrieval_anchors AS current
                 WHERE current.anchor_id = staging.anchor_id
                   AND current.owner_json = staging.owner_json
             )
             WHERE EXISTS(
                 SELECT 1
                 FROM current_authority.retrieval_anchors AS current
                 WHERE current.anchor_id = staging.anchor_id
                   AND current.owner_json = staging.owner_json
                   AND current.anchor_json = '{ANCHOR_RELEASED_MARKER}'
             );
             UPDATE main.observations AS staging
             SET observation_json = (
                 SELECT current.observation_json
                 FROM current_authority.observations AS current
                 WHERE current.observation_id = staging.observation_id
             )
             WHERE EXISTS(
                 SELECT 1
                 FROM current_authority.observations AS current
                 WHERE current.observation_id = staging.observation_id
                   AND current.observation_json = '{OBSERVATION_RELEASED_MARKER}'
             );
             UPDATE main.observation_repository_provenance AS staging
             SET
                 availability_json = (
                     SELECT current.availability_json
                     FROM current_authority.observation_repository_provenance AS current
                     WHERE current.observation_id = staging.observation_id
                 ),
                 capture_json = (
                     SELECT current.capture_json
                     FROM current_authority.observation_repository_provenance AS current
                     WHERE current.observation_id = staging.observation_id
                 )
             WHERE EXISTS(
                 SELECT 1
                 FROM current_authority.observation_repository_provenance AS current
                 WHERE current.observation_id = staging.observation_id
                   AND current.availability_json = '{PROVENANCE_RELEASED_MARKER}'
             );"
        ))
        .map_err(db_error)?;
    transaction
        .execute_batch(&format!(
            "{CREATE_ANCHOR_UPDATE_TRIGGER};
             {CREATE_OBSERVATION_UPDATE_TRIGGER};
             {CREATE_PROVENANCE_UPDATE_TRIGGER};"
        ))
        .map_err(db_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMMUTABLE_SCHEMA: &str = "
        CREATE TABLE retrieval_anchors (
            anchor_id TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            anchor_json TEXT NOT NULL,
            PRIMARY KEY (anchor_id, owner_json)
        );
        CREATE TABLE observations (
            observation_id TEXT PRIMARY KEY,
            observation_json TEXT NOT NULL
        );
        CREATE TABLE observation_repository_provenance (
            observation_id TEXT PRIMARY KEY,
            availability_json TEXT NOT NULL,
            capture_json TEXT
        );";

    fn seed(path: &std::path::Path, released: bool) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch(IMMUTABLE_SCHEMA).unwrap();
        let (anchor, observation, provenance) = if released {
            (
                ANCHOR_RELEASED_MARKER,
                OBSERVATION_RELEASED_MARKER,
                PROVENANCE_RELEASED_MARKER,
            )
        } else {
            (
                "{\"live\":\"anchor\"}",
                "{\"live\":\"observation\"}",
                "{\"live\":\"provenance\"}",
            )
        };
        connection
            .execute(
                "INSERT INTO retrieval_anchors VALUES ('anchor.1', '{}', ?1)",
                [anchor],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO observations VALUES ('observation.1', ?1)",
                [observation],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO observation_repository_provenance
                 VALUES ('observation.1', ?1, ?1)",
                [provenance],
            )
            .unwrap();
        connection
            .execute_batch(&format!(
                "{CREATE_ANCHOR_UPDATE_TRIGGER};
                 {CREATE_OBSERVATION_UPDATE_TRIGGER};
                 {CREATE_PROVENANCE_UPDATE_TRIGGER};"
            ))
            .unwrap();
    }

    #[test]
    fn restore_replays_all_release_markers_through_canonical_immutable_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let current = temporary.path().join("current.db");
        let staging = temporary.path().join("staging.db");
        seed(&current, true);
        seed(&staging, false);

        let mut connection = rusqlite::Connection::open(&staging).unwrap();
        connection
            .execute(
                "ATTACH DATABASE ?1 AS current_authority",
                [current.to_str().unwrap()],
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        replay_current_release_state_for_restore(&transaction).unwrap();
        transaction.commit().unwrap();

        let values = connection
            .query_row(
                "SELECT
                    (SELECT anchor_json FROM retrieval_anchors),
                    (SELECT observation_json FROM observations),
                    (SELECT availability_json FROM observation_repository_provenance),
                    (SELECT capture_json FROM observation_repository_provenance)",
                (),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            values,
            (
                ANCHOR_RELEASED_MARKER.to_owned(),
                OBSERVATION_RELEASED_MARKER.to_owned(),
                PROVENANCE_RELEASED_MARKER.to_owned(),
                PROVENANCE_RELEASED_MARKER.to_owned(),
            )
        );
        for mutation in [
            "UPDATE retrieval_anchors SET anchor_json = '{}' WHERE anchor_id = 'anchor.1'",
            "UPDATE observations SET observation_json = '{}' WHERE observation_id = 'observation.1'",
            "UPDATE observation_repository_provenance SET availability_json = '{}' WHERE observation_id = 'observation.1'",
        ] {
            assert!(connection.execute(mutation, ()).is_err());
        }
    }
}
