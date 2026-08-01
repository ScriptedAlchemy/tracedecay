use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::RetrievalAnchorId;
use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionStateV1, RetrievalAnchorDerivativeV1,
    RetrievalAnchorDispositionRecordV1, RetrievalAnchorOwnerV1, RetrievalAnchorReadOperationV1,
    RetrievalAnchorReadResultV1, RetrievalAnchorTombstoneV1, StoredRetrievalAnchorRecordV1,
};

use super::support::{decode, encode, invalid};

#[derive(Clone, Default)]
pub struct RetrievalAnchorExecutor;

impl RetrievalAnchorExecutor {
    pub fn execute_disposition_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        record: &RetrievalAnchorDispositionRecordV1,
    ) -> rusqlite::Result<()> {
        record.validate().map_err(invalid)?;
        let owner = encode(record.owner())?;
        let record_json = encode(record)?;
        if let Some(existing) = savepoint
            .query_row(
                "SELECT record_json FROM retrieval_anchor_dispositions
                 WHERE disposition_id = ?1",
                [record.disposition_id()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            return if existing == record_json {
                Ok(())
            } else {
                Err(invalid("retrieval anchor disposition replay conflict"))
            };
        }
        let current = current_state(savepoint, record.anchor_id(), &owner)?;
        if !transition_allowed(current, record.state()) {
            return Err(invalid("invalid retrieval anchor disposition transition"));
        }
        savepoint.execute(
            "INSERT INTO retrieval_anchor_dispositions (
                disposition_id, anchor_id, owner_json, state, superseded_by,
                reason_class, effective_at, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.disposition_id(),
                record.anchor_id().as_str(),
                owner,
                record.state().as_str(),
                record.superseded_by().map(RetrievalAnchorId::as_str),
                record.reason_class().as_str(),
                record.effective_at().0,
                record_json,
            ],
        )?;
        if suppresses_derivatives(record.state()) {
            savepoint.execute(
                "INSERT INTO retrieval_anchor_derivative_tombstones (
                    source_anchor_id, owner_json, derivative_kind, derivative_id,
                    disposition_id, effective_at
                 )
                 SELECT source_anchor_id, owner_json, derivative_kind, derivative_id, ?3, ?4
                 FROM retrieval_anchor_reverse_lineage
                 WHERE source_anchor_id = ?1 AND owner_json = ?2",
                params![
                    record.anchor_id().as_str(),
                    encode(record.owner())?,
                    record.disposition_id(),
                    record.effective_at().0,
                ],
            )?;
        }
        Ok(())
    }

    pub fn execute_derivative_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        derivative: &RetrievalAnchorDerivativeV1,
    ) -> rusqlite::Result<()> {
        derivative.validate().map_err(invalid)?;
        let owner = encode(derivative.owner())?;
        if !AnchorDispositionStateV1::serves_derivatives(current_state(
            savepoint,
            derivative.source_anchor_id(),
            &owner,
        )?) {
            return Err(invalid(
                "cannot publish lineage from an unavailable retrieval anchor",
            ));
        }
        let changed = savepoint.execute(
            "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
                source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                derivative.source_anchor_id().as_str(),
                owner,
                derivative.kind().as_str(),
                derivative.derivative_id(),
                i64::from(derivative.is_direct_evidence()),
            ],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let replayed = savepoint
            .query_row(
                "SELECT direct_evidence FROM retrieval_anchor_reverse_lineage
                 WHERE source_anchor_id = ?1 AND owner_json = ?2
                   AND derivative_kind = ?3 AND derivative_id = ?4",
                params![
                    derivative.source_anchor_id().as_str(),
                    encode(derivative.owner())?,
                    derivative.kind().as_str(),
                    derivative.derivative_id(),
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            == Some(i64::from(derivative.is_direct_evidence()));
        if replayed {
            Ok(())
        } else {
            Err(invalid("retrieval anchor derivative replay conflict"))
        }
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &RetrievalAnchorReadOperationV1,
    ) -> rusqlite::Result<RetrievalAnchorReadResultV1> {
        match operation {
            RetrievalAnchorReadOperationV1::AnchorById { anchor_id, owner } => {
                read_anchor(snapshot, anchor_id, owner).map(RetrievalAnchorReadResultV1::Anchor)
            }
            RetrievalAnchorReadOperationV1::CurrentDisposition { anchor_id, owner } => {
                current_record(snapshot, anchor_id, owner)
                    .map(RetrievalAnchorReadResultV1::CurrentDisposition)
            }
            RetrievalAnchorReadOperationV1::Derivatives { anchor_id, owner } => {
                read_derivatives(snapshot, anchor_id, owner)
                    .map(RetrievalAnchorReadResultV1::Derivatives)
            }
            RetrievalAnchorReadOperationV1::Tombstone { anchor_id, owner } => {
                let tombstone = current_record(snapshot, anchor_id, owner)?
                    .filter(|record| {
                        matches!(
                            record.state(),
                            AnchorDispositionStateV1::Redacted
                                | AnchorDispositionStateV1::Expired
                                | AnchorDispositionStateV1::Quarantined
                                | AnchorDispositionStateV1::Deleted
                                | AnchorDispositionStateV1::Unavailable
                        )
                    })
                    .map(|record| {
                        RetrievalAnchorTombstoneV1::new(
                            record.anchor_id().clone(),
                            record.owner().clone(),
                            record.state(),
                            record.reason_class(),
                            record.effective_at(),
                        )
                        .map_err(invalid)
                    })
                    .transpose()?;
                Ok(RetrievalAnchorReadResultV1::Tombstone(tombstone))
            }
        }
    }
}

fn read_anchor(
    connection: &rusqlite::Connection,
    anchor_id: &RetrievalAnchorId,
    owner: &RetrievalAnchorOwnerV1,
) -> rusqlite::Result<Option<StoredRetrievalAnchorRecordV1>> {
    let owner_json = encode(owner)?;
    if !AnchorDispositionStateV1::serves_derivatives(current_state(
        connection,
        anchor_id,
        &owner_json,
    )?) {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT anchor_json, projection_generation FROM retrieval_anchors
             WHERE anchor_id = ?1 AND owner_json = ?2",
            params![anchor_id.as_str(), owner_json],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(record_json, projection_generation)| {
            let record: StoredRetrievalAnchorRecordV1 = decode(record_json)?;
            record.validate().map_err(invalid)?;
            if record.anchor_id() != anchor_id
                || record.owner() != owner.clone()
                || record.projection_generation().as_str() != projection_generation
            {
                return Err(invalid("retrieval anchor record identity mismatch"));
            }
            Ok(record)
        })
        .transpose()
}

fn current_state(
    connection: &rusqlite::Connection,
    anchor_id: &RetrievalAnchorId,
    owner_json: &str,
) -> rusqlite::Result<Option<AnchorDispositionStateV1>> {
    let owner: RetrievalAnchorOwnerV1 = decode(owner_json.to_owned())?;
    current_record(connection, anchor_id, &owner).map(|record| record.map(|record| record.state()))
}

fn current_record(
    connection: &rusqlite::Connection,
    anchor_id: &RetrievalAnchorId,
    owner: &RetrievalAnchorOwnerV1,
) -> rusqlite::Result<Option<RetrievalAnchorDispositionRecordV1>> {
    let owner_json = encode(owner)?;
    connection
        .query_row(
            "SELECT disposition_id, state, superseded_by, reason_class,
                    effective_at, record_json
             FROM retrieval_anchor_dispositions
             WHERE anchor_id = ?1 AND owner_json = ?2
             ORDER BY sequence DESC LIMIT 1",
            params![anchor_id.as_str(), owner_json],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .map(
            |(disposition_id, state, superseded_by, reason_class, effective_at, record_json)| {
                let record: RetrievalAnchorDispositionRecordV1 = decode(record_json)?;
                record.validate().map_err(invalid)?;
                if record.anchor_id() != anchor_id || record.owner() != owner {
                    return Err(invalid(
                        "retrieval anchor disposition read identity mismatch",
                    ));
                }
                if record.disposition_id() != disposition_id
                    || record.state().as_str() != state
                    || record.superseded_by().map(RetrievalAnchorId::as_str)
                        != superseded_by.as_deref()
                    || record.reason_class().as_str() != reason_class
                    || record.effective_at().0 != effective_at
                {
                    return Err(invalid(
                        "retrieval anchor disposition physical columns mismatch",
                    ));
                }
                Ok(record)
            },
        )
        .transpose()
}

fn read_derivatives(
    connection: &rusqlite::Connection,
    anchor_id: &RetrievalAnchorId,
    owner: &RetrievalAnchorOwnerV1,
) -> rusqlite::Result<Vec<RetrievalAnchorDerivativeV1>> {
    let owner_json = encode(owner)?;
    if !AnchorDispositionStateV1::serves_derivatives(current_state(
        connection,
        anchor_id,
        &owner_json,
    )?) {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT lineage.derivative_kind, lineage.derivative_id, lineage.direct_evidence
         FROM retrieval_anchor_reverse_lineage AS lineage
         WHERE lineage.source_anchor_id = ?1 AND lineage.owner_json = ?2
           AND NOT EXISTS (
               SELECT 1 FROM retrieval_anchor_derivative_tombstones AS tombstone
               WHERE tombstone.source_anchor_id = lineage.source_anchor_id
                 AND tombstone.owner_json = lineage.owner_json
                 AND tombstone.derivative_kind = lineage.derivative_kind
                 AND tombstone.derivative_id = lineage.derivative_id
           )
         ORDER BY lineage.derivative_kind, lineage.derivative_id",
    )?;
    statement
        .query_map(params![anchor_id.as_str(), owner_json], |row| {
            RetrievalAnchorDerivativeV1::new(
                anchor_id.clone(),
                owner.clone(),
                AnchorDerivativeKindV1::parse(&row.get::<_, String>(0)?).map_err(invalid)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            )
            .map_err(invalid)
        })?
        .collect()
}

// The disposition legality rules are owned by `AnchorDispositionStateV1` in
// `tracedecay-store`, because the root authority in
// `crates/tracedecay-runtime-core/src/db/retrieval_anchor_authority.rs` appends
// to the same tables and the two
// must never disagree about what an anchor's history permits. Only the refusal
// wording in this module is local, and it is observable, so it stays.

fn transition_allowed(
    current: Option<AnchorDispositionStateV1>,
    next: AnchorDispositionStateV1,
) -> bool {
    AnchorDispositionStateV1::transition_allowed(current, next)
}

fn suppresses_derivatives(state: AnchorDispositionStateV1) -> bool {
    state.suppresses_derivatives()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{FactOwnerV1, ProjectId, UtcMicros};
    use tracedecay_store::AnchorDispositionReasonClassV1;

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.fixture").unwrap(),
        }
    }

    fn anchor(value: &str) -> RetrievalAnchorId {
        RetrievalAnchorId::new(value).unwrap()
    }

    fn install(connection: &rusqlite::Connection) {
        // The anchors table comes from the canonical production DDL so this
        // executor is exercised against the constraints the live table has,
        // rather than a relaxed local restatement of its columns.
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection
            .execute_batch(tracedecay_store::RETRIEVAL_ANCHORS_SCHEMA_DDL)
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE retrieval_anchor_dispositions (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    disposition_id TEXT NOT NULL UNIQUE,
                    anchor_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    state TEXT NOT NULL,
                    superseded_by TEXT,
                    reason_class TEXT NOT NULL,
                    effective_at INTEGER NOT NULL,
                    record_json TEXT NOT NULL,
                    FOREIGN KEY(anchor_id, owner_json)
                      REFERENCES retrieval_anchors(anchor_id, owner_json)
                 );
                 CREATE TABLE retrieval_anchor_reverse_lineage (
                    source_anchor_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    derivative_kind TEXT NOT NULL,
                    derivative_id TEXT NOT NULL,
                    direct_evidence INTEGER NOT NULL,
                    PRIMARY KEY(source_anchor_id, owner_json, derivative_kind, derivative_id),
                    FOREIGN KEY(source_anchor_id, owner_json)
                      REFERENCES retrieval_anchors(anchor_id, owner_json)
                 );
                 CREATE TABLE retrieval_anchor_derivative_tombstones (
                    source_anchor_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    derivative_kind TEXT NOT NULL,
                    derivative_id TEXT NOT NULL,
                    disposition_id TEXT NOT NULL,
                    effective_at INTEGER NOT NULL,
                    PRIMARY KEY(
                      source_anchor_id, owner_json, derivative_kind, derivative_id,
                      disposition_id
                    ),
                    FOREIGN KEY(
                      source_anchor_id, owner_json, derivative_kind, derivative_id
                    ) REFERENCES retrieval_anchor_reverse_lineage(
                      source_anchor_id, owner_json, derivative_kind, derivative_id
                    )
                 );",
            )
            .unwrap();
    }

    fn insert_anchor(connection: &rusqlite::Connection, anchor_id: &RetrievalAnchorId) {
        connection
            .execute(
                "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'projection.fixture')",
                params![anchor_id.as_str(), encode(&owner()).unwrap()],
            )
            .unwrap();
    }

    #[test]
    fn deleted_disposition_atomically_suppresses_derivatives_and_returns_safe_tombstone() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let source = anchor("retrieval.source.fixture");
        insert_anchor(&connection, &source);
        let derivative = RetrievalAnchorDerivativeV1::new(
            source.clone(),
            owner(),
            AnchorDerivativeKindV1::Span,
            "span.fixture",
            true,
        )
        .unwrap();
        let deletion = RetrievalAnchorDispositionRecordV1::new(
            "disposition.fixture",
            source.clone(),
            owner(),
            AnchorDispositionStateV1::Deleted,
            None,
            AnchorDispositionReasonClassV1::UserRequest,
            UtcMicros(7),
        )
        .unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        let mut executor = RetrievalAnchorExecutor;
        executor
            .execute_derivative_write(&savepoint, &derivative)
            .unwrap();
        executor
            .execute_disposition_write(&savepoint, &deletion)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();

        let snapshot = connection.transaction().unwrap();
        let derivatives = executor
            .execute_read(
                &snapshot,
                &RetrievalAnchorReadOperationV1::Derivatives {
                    anchor_id: source.clone(),
                    owner: owner().into(),
                },
            )
            .unwrap();
        assert_eq!(
            derivatives,
            RetrievalAnchorReadResultV1::Derivatives(Vec::new())
        );
        let tombstone = executor
            .execute_read(
                &snapshot,
                &RetrievalAnchorReadOperationV1::Tombstone {
                    anchor_id: source,
                    owner: owner().into(),
                },
            )
            .unwrap();
        assert!(matches!(
            tombstone,
            RetrievalAnchorReadResultV1::Tombstone(Some(_))
        ));
        assert_eq!(
            executor
                .execute_read(
                    &snapshot,
                    &RetrievalAnchorReadOperationV1::AnchorById {
                        anchor_id: anchor("retrieval.source.fixture"),
                        owner: owner().into(),
                    },
                )
                .unwrap(),
            RetrievalAnchorReadResultV1::Anchor(None)
        );
    }

    #[test]
    fn disposition_replay_accepts_identical_material_and_rejects_conflict() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let source = anchor("retrieval.source.fixture");
        insert_anchor(&connection, &source);
        let deletion = RetrievalAnchorDispositionRecordV1::new(
            "disposition.fixture",
            source.clone(),
            owner(),
            AnchorDispositionStateV1::Deleted,
            None,
            AnchorDispositionReasonClassV1::Retention,
            UtcMicros(7),
        )
        .unwrap();
        let mut executor = RetrievalAnchorExecutor;
        for _ in 0..2 {
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            executor
                .execute_disposition_write(&savepoint, &deletion)
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        let conflict = RetrievalAnchorDispositionRecordV1::new(
            "disposition.fixture",
            source,
            owner(),
            AnchorDispositionStateV1::Unavailable,
            None,
            AnchorDispositionReasonClassV1::SourceUnavailable,
            UtcMicros(8),
        )
        .unwrap();
        let mut transaction = connection.transaction().unwrap();
        {
            let mut savepoint = transaction.savepoint().unwrap();
            assert!(
                executor
                    .execute_disposition_write(&savepoint, &conflict)
                    .is_err()
            );
            savepoint.rollback().unwrap();
        }
        transaction.rollback().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM retrieval_anchor_dispositions",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn unavailable_disposition_can_recover_without_permanent_derivative_tombstones() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let source = anchor("retrieval.source.fixture");
        insert_anchor(&connection, &source);
        let derivative = RetrievalAnchorDerivativeV1::new(
            source.clone(),
            owner(),
            AnchorDerivativeKindV1::Contribution,
            "contribution.fixture",
            true,
        )
        .unwrap();
        let unavailable = RetrievalAnchorDispositionRecordV1::new(
            "disposition.unavailable.fixture",
            source.clone(),
            owner(),
            AnchorDispositionStateV1::Unavailable,
            None,
            AnchorDispositionReasonClassV1::SourceUnavailable,
            UtcMicros(7),
        )
        .unwrap();
        let active = RetrievalAnchorDispositionRecordV1::new(
            "disposition.active.fixture",
            source.clone(),
            owner(),
            AnchorDispositionStateV1::Active,
            None,
            AnchorDispositionReasonClassV1::Correction,
            UtcMicros(8),
        )
        .unwrap();
        let mut executor = RetrievalAnchorExecutor;
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        executor
            .execute_derivative_write(&savepoint, &derivative)
            .unwrap();
        executor
            .execute_disposition_write(&savepoint, &unavailable)
            .unwrap();
        executor
            .execute_disposition_write(&savepoint, &active)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();

        let snapshot = connection.transaction().unwrap();
        assert_eq!(
            executor
                .execute_read(
                    &snapshot,
                    &RetrievalAnchorReadOperationV1::Derivatives {
                        anchor_id: source,
                        owner: owner().into(),
                    },
                )
                .unwrap(),
            RetrievalAnchorReadResultV1::Derivatives(vec![derivative])
        );
    }

    #[test]
    fn anchor_resolution_rejects_tampered_persisted_record() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let source = anchor("retrieval.source.fixture");
        insert_anchor(&connection, &source);
        let snapshot = connection.transaction().unwrap();
        assert!(
            RetrievalAnchorExecutor
                .execute_read(
                    &snapshot,
                    &RetrievalAnchorReadOperationV1::AnchorById {
                        anchor_id: source,
                        owner: owner().into(),
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn disposition_reads_reject_physical_column_tampering() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        install(&connection);
        let source = anchor("retrieval.source.fixture");
        insert_anchor(&connection, &source);
        let active = RetrievalAnchorDispositionRecordV1::new(
            "disposition.active.fixture",
            source.clone(),
            owner(),
            AnchorDispositionStateV1::Active,
            None,
            AnchorDispositionReasonClassV1::Correction,
            UtcMicros(7),
        )
        .unwrap();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        RetrievalAnchorExecutor
            .execute_disposition_write(&savepoint, &active)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
        connection
            .execute(
                "UPDATE retrieval_anchor_dispositions
                 SET state = 'deleted' WHERE disposition_id = ?1",
                [active.disposition_id()],
            )
            .unwrap();

        let snapshot = connection.transaction().unwrap();
        assert!(
            RetrievalAnchorExecutor
                .execute_read(
                    &snapshot,
                    &RetrievalAnchorReadOperationV1::CurrentDisposition {
                        anchor_id: source,
                        owner: owner().into(),
                    },
                )
                .is_err()
        );
    }
}
