//! In-place upgrade of the writer idempotency ledger to its narrow shape.
//!
//! The ledger shards carry no `PRAGMA user_version`: they are created lazily by
//! [`super::schema::initialize_schema`] inside whatever transaction the writer
//! already holds, and they share their file with 200-odd unrelated tables whose
//! own versioning does not cover them. Claiming that file-global pragma for the
//! ledger would let a stamp disagree with the shape it claims to describe.
//!
//! So the shape *is* the version. The presence of the `original_receipt_json`
//! column is the old shape and its absence is the new one. That predicate is
//! read from `pragma_table_info`, which is derived from the same schema record
//! the queries compile against, so it cannot drift from what is actually there.
//!
//! # Why the rewrite loses nothing
//!
//! `original_receipt_json` held a serialized [`StoreCommitReceiptV1`]. That type
//! is `deny_unknown_fields` over exactly seven fields, and six of them are
//! already columns of this table — `operation_id`, `idempotency.key`,
//! `idempotency.command_digest`, `shard_id`, `incarnation`, `authority_epoch`,
//! and `committed_at` map onto `operation_id`, `idempotency_key`,
//! `request_digest`, `shard_json`, `incarnation`, `authority_epoch`, and
//! `committed_at_micros`. `super::idempotency::decode_row` already *required*
//! every one of those equalities and failed closed otherwise, so the encoded
//! receipt was never authority for anything except its `commit_sequence`.
//! Carrying that one integer across is therefore a lossless projection.
//!
//! # Why an interruption is safe
//!
//! The rewrite runs entirely inside the caller's transaction. SQLite makes DDL
//! transactional, so a crash, a kill, or a rolled-back savepoint leaves the old
//! table exactly as it was — including its column set, which is the version.
//! The next open re-reads the predicate, still sees the old shape, and retries.
//! There is no half-migrated state to detect because there is no state outside
//! the transaction to disagree with.
//!
//! # Why a concurrent writer is safe
//!
//! One writer actor owns one shard, and SQLite serializes writers across
//! processes besides. A racing process either waits for the exclusive lock and
//! then observes the new shape, or is refused with `SQLITE_BUSY` and rolls back
//! having changed nothing.
//!
//! # Why an unmigratable row stops the migration
//!
//! A receipt whose `commit_sequence` is not a positive integer is already fatal:
//! today `decode_row` answers [`LedgerError::Corrupt`] the moment that key is
//! looked up. The preflight below raises the same error class before touching
//! anything, so the transaction rolls back and the store is left byte-identical
//! at its old shape. Dropping such a row instead would silently shrink the set
//! of submissions the ledger can recognise, which is the one outcome that could
//! admit a duplicate write.

use super::{LedgerError, sqlite::LedgerTransaction};

const IDEMPOTENCY_TABLE: &str = "td_runtime_writer_idempotency_v1";

/// True while the table still carries the encoded receipt column.
const DETECT_LEGACY_SHAPE: &str = r#"
SELECT count(*)
FROM pragma_table_info('td_runtime_writer_idempotency_v1')
WHERE name = 'original_receipt_json'
"#;

/// Counts rows whose encoded receipt cannot yield a usable commit sequence.
///
/// `typeof` is checked explicitly because SQLite orders text above every
/// integer, so a `'garbage' > 0` comparison would otherwise pass.
const COUNT_UNMIGRATABLE: &str = r#"
SELECT count(*)
FROM td_runtime_writer_idempotency_v1
WHERE typeof(json_extract(original_receipt_json, '$.commit_sequence')) <> 'integer'
   OR json_extract(original_receipt_json, '$.commit_sequence') <= 0
"#;

/// The narrow shape, built beside the old table so the swap is one rename.
const CREATE_NARROW: &str = r#"
CREATE TABLE td_runtime_writer_idempotency_migrate_v1 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    commit_sequence INTEGER NOT NULL CHECK (commit_sequence > 0),
    transaction_scope_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation, authority_epoch, idempotency_key)
) WITHOUT ROWID
"#;

/// Projects every legacy row onto the narrow shape. `NOT NULL` and the `CHECK`
/// stay as a backstop under the preflight, so a row the preflight somehow missed
/// aborts the transaction rather than landing unreadable.
const COPY_ROWS: &str = r#"
INSERT INTO td_runtime_writer_idempotency_migrate_v1 (
    shard_json, incarnation, authority_epoch, idempotency_key, request_digest,
    commit_sequence, transaction_scope_json, operation_id, durability_json,
    committed_at_micros
)
SELECT shard_json, incarnation, authority_epoch, idempotency_key, request_digest,
       json_extract(original_receipt_json, '$.commit_sequence'),
       transaction_scope_json, operation_id, durability_json, committed_at_micros
FROM td_runtime_writer_idempotency_v1
"#;

const DROP_LEGACY: &str = "DROP TABLE td_runtime_writer_idempotency_v1";
const RENAME_NARROW: &str = "ALTER TABLE td_runtime_writer_idempotency_migrate_v1
     RENAME TO td_runtime_writer_idempotency_v1";
const COUNT_LEGACY: &str = "SELECT count(*) FROM td_runtime_writer_idempotency_v1";
const COUNT_NARROW: &str = "SELECT count(*) FROM td_runtime_writer_idempotency_migrate_v1";

/// Upgrades the idempotency ledger in the caller's transaction when it is still
/// at the legacy shape, and does nothing otherwise.
pub(super) fn upgrade_idempotency_shape(
    transaction: &impl LedgerTransaction,
) -> Result<(), LedgerError> {
    if count(transaction, DETECT_LEGACY_SHAPE)? == 0 {
        return Ok(());
    }

    if count(transaction, COUNT_UNMIGRATABLE)? != 0 {
        return Err(LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "original receipt commit sequence",
        });
    }

    let legacy_rows = count(transaction, COUNT_LEGACY)?;
    transaction.execute_batch(CREATE_NARROW)?;
    transaction.execute(COPY_ROWS, [])?;

    // Every legacy row must have landed. A short copy would silently forget
    // submissions the ledger is supposed to recognise, so it fails closed.
    if count(transaction, COUNT_NARROW)? != legacy_rows {
        return Err(LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "migrated row count",
        });
    }

    transaction.execute_batch(DROP_LEGACY)?;
    transaction.execute_batch(RENAME_NARROW)?;
    Ok(())
}

fn count(transaction: &impl LedgerTransaction, sql: &str) -> Result<i64, LedgerError> {
    let mut statement = transaction.prepare(sql)?;
    let mut rows = statement.query([])?;
    let row = rows.next()?.ok_or(LedgerError::Corrupt {
        table: IDEMPOTENCY_TABLE,
        field: "schema probe",
    })?;
    Ok(row.get(0)?)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tracedecay_store::{StoreCommitReceiptV1, StoreOperationMetadataV1};

    use crate::{
        ledger::{
            LedgerDisposition, LedgerError, initialize_schema, lookup_receipt, record_commit,
            sqlite::{BindingKey, encode_json},
        },
        test_support::{binding, metadata, scope},
    };

    /// The exact shape every existing store carries today, reproduced verbatim
    /// so the migration tests start from a real pre-migration table rather than
    /// from something the new code could have written.
    const LEGACY_IDEMPOTENCY_DDL: &str = r#"
CREATE TABLE td_runtime_writer_idempotency_v1 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    original_receipt_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation, authority_epoch, idempotency_key)
) WITHOUT ROWID;
"#;

    const INSERT_LEGACY: &str = "INSERT INTO td_runtime_writer_idempotency_v1 (
         shard_json, incarnation, authority_epoch, idempotency_key, request_digest,
         original_receipt_json, transaction_scope_json, operation_id, durability_json,
         committed_at_micros
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

    fn legacy_receipt(metadata: &StoreOperationMetadataV1, sequence: u64) -> StoreCommitReceiptV1 {
        serde_json::from_value(serde_json::json!({
            "operation_id": metadata.operation_id,
            "idempotency": metadata.idempotency,
            "shard_id": metadata.shard_id,
            "incarnation": metadata.incarnation,
            "authority_epoch": metadata.authority_epoch,
            "commit_sequence": sequence,
            "committed_at": metadata.admitted_at,
        }))
        .unwrap()
    }

    /// Writes one authentic legacy row. The receipt is encoded by the same
    /// serializer production used, so the row is byte-for-byte what a store
    /// written before this change holds.
    fn insert_legacy(
        connection: &Connection,
        metadata: &StoreOperationMetadataV1,
        sequence: u64,
        receipt_json_override: Option<&str>,
    ) -> StoreCommitReceiptV1 {
        let receipt = legacy_receipt(metadata, sequence);
        let scope = scope(metadata);
        let binding_key = BindingKey::from_parts(&metadata.shard_id, metadata.incarnation).unwrap();
        let receipt_json = receipt_json_override
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string(&receipt).unwrap());
        connection
            .execute(
                INSERT_LEGACY,
                rusqlite::params![
                    &binding_key.shard_json,
                    binding_key.incarnation_sql,
                    i64::try_from(metadata.authority_epoch.get()).unwrap(),
                    metadata.idempotency.key.as_str(),
                    metadata.idempotency.command_digest.as_str(),
                    receipt_json,
                    encode_json(&scope, "transaction_scope_json").unwrap(),
                    metadata.operation_id.as_str(),
                    encode_json(&metadata.durability, "durability_json").unwrap(),
                    metadata.admitted_at.0,
                ],
            )
            .unwrap();
        receipt
    }

    fn seed_legacy(
        connection: &Connection,
        metadata: &StoreOperationMetadataV1,
        sequence: u64,
        receipt_json_override: Option<&str>,
    ) -> StoreCommitReceiptV1 {
        connection.execute_batch(LEGACY_IDEMPOTENCY_DDL).unwrap();
        insert_legacy(connection, metadata, sequence, receipt_json_override)
    }

    fn has_column(connection: &Connection, column: &str) -> bool {
        connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('td_runtime_writer_idempotency_v1')
                 WHERE name = ?1",
                [column],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    fn row_count(connection: &Connection) -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM td_runtime_writer_idempotency_v1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn migration_preserves_the_exact_receipt_a_legacy_store_recorded() {
        let mut connection = Connection::open_in_memory().unwrap();
        let metadata = metadata("operation.legacy", "key.legacy", 'a');
        let binding = binding(&metadata);
        let expected = seed_legacy(&connection, &metadata, 4_242, None);
        assert!(has_column(&connection, "original_receipt_json"));

        let transaction = connection.transaction().unwrap();
        initialize_schema(&transaction).unwrap();
        transaction.commit().unwrap();

        assert!(!has_column(&connection, "original_receipt_json"));
        assert!(has_column(&connection, "commit_sequence"));
        assert_eq!(row_count(&connection), 1);

        let transaction = connection.transaction().unwrap();
        let found = lookup_receipt(&transaction, &binding, &metadata.idempotency)
            .unwrap()
            .expect("migrated row must still answer the original lookup");
        assert_eq!(found, expected);
    }

    #[test]
    fn a_migrated_duplicate_still_replays_instead_of_committing_again() {
        let mut connection = Connection::open_in_memory().unwrap();
        let original = metadata("operation.legacy", "key.dup", 'a');
        let expected = seed_legacy(&connection, &original, 7, None);

        let transaction = connection.transaction().unwrap();
        initialize_schema(&transaction).unwrap();

        // Same key, same digest, different operation id: a retry.
        let replay = metadata("operation.retry", "key.dup", 'a');
        assert!(matches!(
            record_commit(&transaction, &replay, &scope(&replay), None).unwrap(),
            LedgerDisposition::Replay(found) if found == expected
        ));
        // Same key, different digest: a conflict, never a second commit.
        let conflict = metadata("operation.other", "key.dup", 'b');
        assert!(matches!(
            record_commit(&transaction, &conflict, &scope(&conflict), None).unwrap(),
            LedgerDisposition::Conflict(found) if found == expected
        ));
        transaction.commit().unwrap();

        // Neither submission added a row.
        assert_eq!(row_count(&connection), 1);
    }

    #[test]
    fn an_interrupted_migration_leaves_the_legacy_store_untouched_and_retries() {
        let mut connection = Connection::open_in_memory().unwrap();
        let metadata = metadata("operation.legacy", "key.interrupt", 'a');
        let binding = binding(&metadata);
        let expected = seed_legacy(&connection, &metadata, 99, None);

        // A migration that never reaches its commit.
        let transaction = connection.transaction().unwrap();
        initialize_schema(&transaction).unwrap();
        assert!(!has_column(&transaction, "original_receipt_json"));
        transaction.rollback().unwrap();

        // The shape is the version, so rolling back restores both.
        assert!(has_column(&connection, "original_receipt_json"));
        assert!(!has_column(&connection, "commit_sequence"));
        assert_eq!(row_count(&connection), 1);

        // The next open migrates cleanly and the row is still answerable.
        let transaction = connection.transaction().unwrap();
        initialize_schema(&transaction).unwrap();
        transaction.commit().unwrap();
        assert!(!has_column(&connection, "original_receipt_json"));

        let transaction = connection.transaction().unwrap();
        let found = lookup_receipt(&transaction, &binding, &metadata.idempotency)
            .unwrap()
            .expect("row survives an interrupted migration");
        assert_eq!(found, expected);
    }

    #[test]
    fn an_unmigratable_receipt_fails_closed_without_dropping_the_row() {
        let mut connection = Connection::open_in_memory().unwrap();
        let metadata = metadata("operation.legacy", "key.corrupt", 'a');
        // A receipt whose commit sequence is text. SQLite orders text above
        // every integer, so a bare `> 0` guard would have let this through.
        seed_legacy(
            &connection,
            &metadata,
            1,
            Some(r#"{"commit_sequence":"not-a-number"}"#),
        );

        let transaction = connection.transaction().unwrap();
        assert!(matches!(
            initialize_schema(&transaction),
            Err(LedgerError::Corrupt { .. })
        ));
        transaction.rollback().unwrap();

        // The store is left exactly as it was: nothing dropped, nothing rewritten.
        assert!(has_column(&connection, "original_receipt_json"));
        assert_eq!(row_count(&connection), 1);
    }

    #[test]
    fn migrating_an_already_narrow_store_is_a_no_op() {
        let mut connection = Connection::open_in_memory().unwrap();
        let metadata = metadata("operation.fresh", "key.fresh", 'a');
        let binding = binding(&metadata);

        let transaction = connection.transaction().unwrap();
        initialize_schema(&transaction).unwrap();
        let receipt = match record_commit(&transaction, &metadata, &scope(&metadata), None).unwrap()
        {
            LedgerDisposition::Committed(receipt) => receipt,
            other => panic!("expected commit, got {other:?}"),
        };
        transaction.commit().unwrap();

        // Re-initialising repeatedly must not disturb a narrow store.
        for _ in 0..3 {
            let transaction = connection.transaction().unwrap();
            initialize_schema(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        assert_eq!(row_count(&connection), 1);

        let transaction = connection.transaction().unwrap();
        assert_eq!(
            lookup_receipt(&transaction, &binding, &metadata.idempotency).unwrap(),
            Some(receipt)
        );
    }

    /// A binary from before this change reads `original_receipt_json`. Against a
    /// migrated store that column does not exist, so its statement fails to
    /// prepare. The point of pinning it is the *direction* of the failure: the
    /// old binary is refused, never told "no such row", so it can never mistake
    /// a shape it cannot read for an absent record and admit a duplicate write.
    #[test]
    fn an_older_binary_is_refused_by_a_migrated_store_rather_than_misreading_it() {
        const LEGACY_SELECT: &str = "SELECT request_digest, original_receipt_json,
                 transaction_scope_json, operation_id, durability_json, committed_at_micros
             FROM td_runtime_writer_idempotency_v1
             WHERE shard_json = ?1 AND incarnation = ?2 AND authority_epoch = ?3
               AND idempotency_key = ?4";

        let mut connection = Connection::open_in_memory().unwrap();
        let metadata = metadata("operation.downgrade", "key.downgrade", 'a');
        seed_legacy(&connection, &metadata, 5, None);
        // The legacy statement is valid against the legacy store.
        connection.prepare(LEGACY_SELECT).unwrap();

        let transaction = connection.transaction().unwrap();
        initialize_schema(&transaction).unwrap();
        transaction.commit().unwrap();

        let error = connection
            .prepare(LEGACY_SELECT)
            .expect_err("a pre-migration binary must be refused, not answered");
        assert!(
            error.to_string().contains("original_receipt_json"),
            "expected a missing-column refusal, got {error}"
        );
    }

    /// Falsifiable size claim, measured as on-disk pages over the same rows at
    /// the two shapes. A legacy row averages ~1,486 bytes and so exceeds the
    /// 1,002-byte `maxLocal` of a 4 KiB index b-tree page, spilling one overflow
    /// page each; the narrow row fits and spills none.
    #[test]
    fn the_narrow_shape_removes_the_per_row_overflow_page() {
        let directory = tempfile::tempdir().unwrap();
        let rows = 500_usize;
        let legacy_path = directory.path().join("legacy.db");
        let narrow_path = directory.path().join("narrow.db");

        let cases = [(&legacy_path, true), (&narrow_path, false)];
        for (path, legacy) in cases {
            let mut connection = Connection::open(path).unwrap();
            if legacy {
                connection.execute_batch(LEGACY_IDEMPOTENCY_DDL).unwrap();
            } else {
                let transaction = connection.transaction().unwrap();
                initialize_schema(&transaction).unwrap();
                transaction.commit().unwrap();
            }
            let transaction = connection.transaction().unwrap();
            for index in 0..rows {
                let metadata = metadata(
                    &format!("operation.host-observation.{index:064x}"),
                    &format!("host.{index:064x}"),
                    'a',
                );
                if legacy {
                    insert_legacy(
                        &transaction,
                        &metadata,
                        u64::try_from(index + 1).unwrap(),
                        None,
                    );
                } else {
                    record_commit(&transaction, &metadata, &scope(&metadata), None).unwrap();
                }
            }
            transaction.commit().unwrap();
            connection.execute_batch("VACUUM").unwrap();
        }

        let bytes = |path: &std::path::Path| -> i64 {
            let connection = Connection::open(path).unwrap();
            let pages: i64 = connection
                .query_row("PRAGMA page_count", [], |row| row.get(0))
                .unwrap();
            let size: i64 = connection
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .unwrap();
            pages * size
        };
        let legacy_bytes = bytes(&legacy_path);
        let narrow_bytes = bytes(&narrow_path);
        println!(
            "rows={rows} legacy_bytes={legacy_bytes} narrow_bytes={narrow_bytes} \
             saved={} reduction={}%",
            legacy_bytes - narrow_bytes,
            (legacy_bytes - narrow_bytes) * 100 / legacy_bytes
        );
        assert!(
            narrow_bytes * 2 < legacy_bytes,
            "narrow shape must more than halve the ledger: \
             legacy={legacy_bytes} narrow={narrow_bytes}"
        );
    }
}
