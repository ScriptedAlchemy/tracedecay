use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, DurableObservationV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1,
};
use tracedecay_store::ObservationWrite;

use super::support::{decode, encode, invalid};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationReadOperationV1 {
    SourceCursor {
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
    },
    Observation {
        observation_id: CanonicalObservationIdV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredObservationRowV1 {
    pub sequence: u64,
    pub observation: DurableObservationV1,
    pub committed_cursor: ObservationSourceCursorV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationReadResultV1 {
    SourceCursor(Option<ObservationSourceCursorV1>),
    Observation(Box<Option<StoredObservationRowV1>>),
}

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
        let actual_cursor = read_cursor(savepoint, &source_json, &scope_json)?;
        if actual_cursor.as_ref() != write.expected_cursor() {
            return Err(invalid("observation source cursor conflict"));
        }

        let observation_json = encode(observation)?;
        let committed_cursor_json = encode(write.next_cursor())?;
        let receipt = observation.receipt();
        let receipt_json = encode(receipt)?;
        let receipt_id = receipt.receipt().receipt_id().as_str();
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
                observation.payload_reference().digest().as_str(),
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
