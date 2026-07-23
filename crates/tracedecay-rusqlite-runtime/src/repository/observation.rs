use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::ObservationSourceCursorV1;
use tracedecay_store::{
    ObservationReadOperationV1, ObservationReadResultV1, ObservationWrite, StoredObservationRowV1,
};

use super::support::{decode, encode, invalid};

#[derive(Clone, Default)]
pub struct ObservationExecutor;

impl ObservationExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        write: &ObservationWrite,
    ) -> rusqlite::Result<()> {
        let observation = write.observation();
        let source_json = encode(observation.source())?;
        let scope_json = encode(observation.scope())?;
        let observation_json = encode(observation)?;
        let committed_cursor_json = encode(write.next_cursor())?;
        let receipt = observation.receipt();
        let receipt_json = encode(receipt)?;
        let receipt_id = receipt.receipt().receipt_id().as_str();
        let payload_digest = observation.payload_reference().digest().as_str();
        let existing = savepoint
            .query_row(
                "SELECT payload_digest, receipt_id, observation_json
                 FROM observations WHERE observation_id = ?1",
                [observation.observation_id().as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_digest, stored_receipt_id, stored_observation)) = existing {
            if stored_digest != payload_digest
                || stored_receipt_id != receipt_id
                || stored_observation != observation_json
            {
                return Err(invalid("observation identity collision"));
            }
            let stored_receipt: String = savepoint.query_row(
                "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
                [receipt_id],
                |row| row.get(0),
            )?;
            if stored_receipt != receipt_json {
                return Err(invalid("sanitization receipt identity collision"));
            }
            return Ok(());
        }

        let actual_cursor = read_cursor(savepoint, &source_json, &scope_json)?;
        if actual_cursor.as_ref() != write.expected_cursor() {
            return Err(invalid("observation source cursor conflict"));
        }

        savepoint.execute(
            "INSERT INTO sanitization_receipts (
                receipt_id, sanitizer_version, payload_digest, receipt_json
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(receipt_id) DO NOTHING",
            params![
                receipt_id,
                receipt.receipt().sanitizer_version().as_str(),
                receipt
                    .payload()
                    .map_or("", |payload| payload.digest().as_str()),
                receipt_json,
            ],
        )?;
        let stored_receipt: String = savepoint.query_row(
            "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
            [receipt_id],
            |row| row.get(0),
        )?;
        if stored_receipt != receipt_json {
            return Err(invalid("sanitization receipt identity collision"));
        }

        savepoint.execute(
            "INSERT INTO observations (
                observation_id, payload_digest, receipt_id,
                observation_json, committed_cursor_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                observation.observation_id().as_str(),
                payload_digest,
                receipt_id,
                observation_json,
                committed_cursor_json,
            ],
        )?;
        let sequence = savepoint.last_insert_rowid();
        savepoint.execute(
            "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_json, scope_json) DO UPDATE SET
                cursor_json = excluded.cursor_json",
            params![source_json, scope_json, committed_cursor_json],
        )?;
        savepoint.execute(
            "INSERT INTO projection_queue (observation_id, observation_sequence)
             VALUES (?1, ?2)",
            params![observation.observation_id().as_str(), sequence],
        )?;
        Ok(())
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &ObservationReadOperationV1,
    ) -> rusqlite::Result<ObservationReadResultV1> {
        match operation {
            ObservationReadOperationV1::SourceCursor { source, scope } => {
                let cursor = read_cursor(snapshot, &encode(source)?, &encode(scope)?)?;
                Ok(ObservationReadResultV1::SourceCursor(cursor))
            }
            ObservationReadOperationV1::Observation { observation_id } => {
                let row = snapshot
                    .query_row(
                        "SELECT sequence, observation_json, committed_cursor_json
                         FROM observations WHERE observation_id = ?1",
                        [observation_id.as_str()],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                let value = row
                    .map(|(sequence, observation, cursor)| -> rusqlite::Result<_> {
                        Ok(StoredObservationRowV1 {
                            sequence: u64::try_from(sequence)
                                .map_err(|_| invalid("negative observation sequence"))?,
                            observation: decode(observation)?,
                            committed_cursor: decode(cursor)?,
                        })
                    })
                    .transpose()?;
                if value
                    .as_ref()
                    .is_some_and(|row| row.observation.observation_id() != observation_id)
                {
                    return Err(invalid("observation row identity mismatch"));
                }
                Ok(ObservationReadResultV1::Observation(Box::new(value)))
            }
        }
    }
}

fn read_cursor(
    connection: &rusqlite::Connection,
    source_json: &str,
    scope_json: &str,
) -> rusqlite::Result<Option<ObservationSourceCursorV1>> {
    connection
        .query_row(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(decode)
        .transpose()
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;
    use tracedecay_domain::{
        ComponentVersion, ObservationId, ObservationIdentityMaterialV1,
        ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, ProjectId, ProviderId, RetentionClass, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
        SessionId,
    };
    use tracedecay_store::ObservationWrite;

    use super::ObservationExecutor;

    fn observation_write(body: &str, receipt_id: &str) -> ObservationWrite {
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("provider.fixture").unwrap(),
            SessionId::new("session.fixture").unwrap(),
        )
        .unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: ProjectId::new("project.fixture").unwrap(),
        };
        let generation = ObservationSourceGenerationV1::new(1).unwrap();
        let range = ObservationSourceRangeV1::new(0, 1).unwrap();
        let payload = json!({"kind": "assistant_message", "body": body});
        let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt_id).unwrap(),
                ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(payload_reference),
        )
        .unwrap();
        let observation = tracedecay_domain::DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source.clone(),
                scope.clone(),
                generation,
                range,
                ObservationOrderingDomainV1::SqliteRowId,
                ObservationId::new("record.fixture").unwrap(),
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.fixture").unwrap(),
            payload,
        )
        .unwrap();
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            range.end(),
        )
        .unwrap();
        ObservationWrite::new(observation, None, next_cursor).unwrap()
    }

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sanitization_receipts (
                    receipt_id TEXT PRIMARY KEY,
                    sanitizer_version TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    receipt_json TEXT NOT NULL
                 );
                 CREATE TABLE observations (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    observation_id TEXT NOT NULL UNIQUE,
                    payload_digest TEXT NOT NULL,
                    receipt_id TEXT NOT NULL,
                    observation_json TEXT NOT NULL,
                    committed_cursor_json TEXT NOT NULL
                 );
                 CREATE TABLE source_cursors (
                    source_json TEXT NOT NULL,
                    scope_json TEXT NOT NULL,
                    cursor_json TEXT NOT NULL,
                    PRIMARY KEY (source_json, scope_json)
                 );
                 CREATE TABLE projection_queue (
                    observation_id TEXT PRIMARY KEY,
                    observation_sequence INTEGER NOT NULL UNIQUE
                 );",
            )
            .unwrap();
        connection
    }

    fn execute(connection: &mut Connection, write: &ObservationWrite) -> rusqlite::Result<()> {
        let mut transaction = connection.transaction()?;
        let savepoint = transaction.savepoint()?;
        ObservationExecutor.execute_write(&savepoint, write)?;
        savepoint.commit()?;
        transaction.commit()
    }

    #[test]
    fn exact_replay_is_a_no_op_after_the_source_cursor_advanced() {
        let mut connection = connection();
        let write = observation_write("fixture", "receipt.fixture");
        let replay = ObservationWrite::new(
            write.observation().clone(),
            None,
            write.next_cursor().clone().with_resume_checkpoint(7, 11),
        )
        .unwrap();

        execute(&mut connection, &write).unwrap();
        execute(&mut connection, &replay).unwrap();

        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM projection_queue", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn identity_collision_fails_without_advancing_the_source_cursor() {
        let mut connection = connection();
        let write = observation_write("fixture", "receipt.fixture");
        execute(&mut connection, &write).unwrap();
        let cursor_before: String = connection
            .query_row("SELECT cursor_json FROM source_cursors", [], |row| {
                row.get(0)
            })
            .unwrap();

        let error = execute(
            &mut connection,
            &observation_write("conflicting", "receipt.conflicting"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("observation identity collision"));
        assert_eq!(
            connection
                .query_row("SELECT cursor_json FROM source_cursors", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            cursor_before
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM projection_queue", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
