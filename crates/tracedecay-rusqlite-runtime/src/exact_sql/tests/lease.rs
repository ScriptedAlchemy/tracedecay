use super::*;

#[test]
fn dropping_pinned_transaction_rolls_back() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE dropped (value INTEGER NOT NULL)".to_owned())
        .unwrap();
    {
        let transaction = channel.begin_immediate().unwrap();
        transaction
            .execute(statement(
                "INSERT INTO dropped VALUES (?)",
                vec![ExactSqlValue::Integer(8)],
            ))
            .unwrap();
    }

    let rows = channel
        .query(
            statement("SELECT count(*) FROM dropped", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();

    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn writer_shutdown_rolls_back_and_closes_a_leaked_transaction() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();
    let Fixture {
        _directory,
        writer,
        readers,
    } = fixture;
    let (finished, receive) = std::sync::mpsc::sync_channel(1);

    std::thread::spawn(move || {
        drop(writer);
        let _ = finished.send(());
    });

    receive
        .recv_timeout(Duration::from_secs(1))
        .expect("writer shutdown must not wait forever on leaked exact SQL transaction");
    assert!(matches!(
        transaction.commit(),
        Err(ExactSqlError::TransactionClosed)
    ));
    drop(readers);
    drop(_directory);
}

#[test]
fn idle_transaction_expires_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();

    std::thread::sleep(EXACT_SQL_TRANSACTION_IDLE_LIMIT + Duration::from_millis(100));

    assert!(matches!(
        transaction.commit(),
        Err(ExactSqlError::TransactionExpired)
    ));
    channel
        .execute_batch("CREATE TABLE after_idle_expiry (value INTEGER)".to_owned())
        .unwrap();
}

/// A statement that consumed its whole execution deadline and then failed is
/// writer work, not caller idleness. Charging it to the idle limit tore the
/// transaction down before the caller — still holding the statement's error —
/// could roll it back, and the caller's own rollback then reported the lease
/// expiry. The operator saw `execute batch failed: interrupted; rollback
/// failed: … lease expired`, which claims durability is in doubt.
#[test]
fn an_interrupted_statement_leaves_the_rollback_to_its_caller() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE sink (value INTEGER)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    let error = transaction
        .execute_batch(
            "INSERT INTO sink(value)
             WITH RECURSIVE counter(value) AS (
                SELECT 1 UNION ALL SELECT value + 1 FROM counter WHERE value < 1000000000
             )
             SELECT value FROM counter"
                .to_owned(),
        )
        .expect_err("the execution guard must interrupt a statement past its deadline");
    assert!(
        matches!(&error, ExactSqlError::Sqlite { message, .. } if message.contains("interrupted")),
        "expected the interrupted statement, got: {error}"
    );

    transaction
        .rollback()
        .expect("the caller still owns the rollback of its own failed statement");
    let rows = channel
        .query(
            statement("SELECT count(*) FROM sink", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

/// Lease expiry stays reachable, and when it fires the writer is the side
/// holding the transaction — so it rolls back and publishes that receipt
/// before releasing. A caller's later rollback learns its work was discarded
/// instead of being told the rollback failed; only its commit is refused.
#[test]
fn lease_expiry_reports_the_rollback_it_performed() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch("CREATE TABLE expired (value INTEGER)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();
    transaction
        .execute(statement(
            "INSERT INTO expired VALUES (?)",
            vec![ExactSqlValue::Integer(1)],
        ))
        .unwrap();

    std::thread::sleep(EXACT_SQL_TRANSACTION_IDLE_LIMIT + Duration::from_millis(100));

    let receipt = transaction
        .rollback()
        .expect("an expired lease rolls back before it releases the writer");
    assert_eq!(receipt.discarded_changed_rows, 1);
    let rows = channel
        .query(
            statement("SELECT count(*) FROM expired", vec![]),
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(0)]);
}

#[test]
fn active_transaction_hits_absolute_lease_and_releases_writer() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    let transaction = channel.begin_immediate().unwrap();
    let started = Instant::now();

    let error = loop {
        match transaction.query(statement("SELECT 1", vec![])) {
            Ok(_) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => break error,
        }
    };

    assert!(matches!(error, ExactSqlError::TransactionExpired));
    // Expiry must land near the absolute lease, not multiples beyond it.
    assert!(started.elapsed() < EXACT_SQL_TRANSACTION_LIMIT * 2);
    channel
        .execute_batch("CREATE TABLE after_absolute_expiry (value INTEGER)".to_owned())
        .unwrap();
}

/// Seeds `rows` of the shape a payload-copying predecessor left behind, in
/// batches that each fit inside one execution, so the fixture itself never
/// depends on the limit the test is about.
fn seed_migration_source(channel: &ExactSqlHandle, rows: i64) {
    const BATCH: i64 = 250_000;
    let mut seeded = 0;
    while seeded < rows {
        let batch = BATCH.min(rows - seeded);
        channel
            .execute_batch(format!(
                "INSERT INTO source (key, payload)
                 WITH RECURSIVE row_index(index_value) AS (
                    SELECT {start} UNION ALL
                    SELECT index_value + 1 FROM row_index WHERE index_value < {end}
                 )
                 SELECT index_value,
                        '{{\"mutation_digest\":\"sha256:mut' || index_value
                            || '\",\"partition_digest\":\"sha256:part\"}}'
                 FROM row_index",
                start = seeded + 1,
                end = seeded + batch,
            ))
            .unwrap();
        seeded += batch;
    }
}

/// Rows the store-sized fixture seeds.
///
/// Sized from this move's measured cost on this shape — about 1.6 µs a row —
/// so the whole-table form needs several times the shortened test execution
/// limit. A slower host only widens the overrun, so the refusal cannot stop
/// firing; the fixture would have to get faster than the limit to go quiet.
const STORE_SIZED_ROWS: i64 = 3_000_000;

/// Rows a migration moves per write, mirroring the production chunk.
const MIGRATION_CHUNK_ROWS: i64 = 5_000;

/// Why a store-sized migration cannot run as one statement inside a caller's
/// leased transaction, which is what took a daemon down on a large store: a
/// schema stage rewrote whole tables and rebuilt an index on every open, each
/// as a single statement. On a real store each one outran its execution
/// deadline, `SQLite` interrupted it, and the open failed — every open, since
/// the rewrite never got far enough to retire anything.
///
/// The same move is driven both ways over one fixture. As one statement the
/// limit must refuse it, which is what makes chunking a migration load
/// bearing rather than decorative. At the chunk size a migration actually
/// writes, the same move must commit with the limit far from reach — that
/// headroom is what lets a migration keep the limit as its safety bound
/// instead of asking for a longer one. Whether the chunk loop then finishes a
/// whole table is settled where the migrations live.
#[test]
fn a_store_sized_statement_is_refused_and_a_migration_chunk_has_headroom() {
    const MOVE: &str = "INSERT OR IGNORE INTO moved (key, mutation_digest, partition_digest)
                        SELECT key,
                               json_extract(payload, '$.mutation_digest'),
                               json_extract(payload, '$.partition_digest')
                        FROM source WHERE key <= ?";

    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
    channel
        .execute_batch(
            "CREATE TABLE source (key INTEGER PRIMARY KEY, payload TEXT NOT NULL);
             CREATE TABLE moved (
                key INTEGER PRIMARY KEY,
                mutation_digest TEXT NOT NULL,
                partition_digest TEXT NOT NULL
             );"
            .to_owned(),
        )
        .unwrap();
    seed_migration_source(&channel, STORE_SIZED_ROWS);

    let transaction = channel.begin_immediate().unwrap();
    let started = Instant::now();
    let error = transaction
        .execute(statement(
            MOVE,
            vec![ExactSqlValue::Integer(STORE_SIZED_ROWS)],
        ))
        .expect_err("a store-sized rewrite cannot fit in one leased execution");
    let refused_after = started.elapsed();
    assert!(
        matches!(&error, ExactSqlError::Sqlite { message, .. } if message.contains("interrupted")),
        "expected the execution limit to interrupt the whole-table rewrite, got: {error}"
    );
    assert!(
        refused_after < EXACT_SQL_EXECUTION_LIMIT * 2,
        "the limit must refuse the statement at its deadline, not long after: {refused_after:?}"
    );
    transaction.rollback().unwrap();
    assert_eq!(
        moved_rows(&channel),
        0,
        "the refused statement must move nothing"
    );

    let chunk = channel.begin_immediate().unwrap();
    let started = Instant::now();
    chunk
        .execute(statement(
            MOVE,
            vec![ExactSqlValue::Integer(MIGRATION_CHUNK_ROWS)],
        ))
        .expect("a bounded chunk of the same move fits inside one execution");
    chunk.commit().unwrap();
    let chunk_took = started.elapsed();
    assert_eq!(moved_rows(&channel), MIGRATION_CHUNK_ROWS);
    assert!(
        chunk_took * 4 < EXACT_SQL_EXECUTION_LIMIT,
        "a migration chunk must leave the limit room to spare on a slower host: {chunk_took:?}"
    );
}

fn moved_rows(channel: &ExactSqlHandle) -> i64 {
    let rows = channel
        .query(
            statement("SELECT count(*) FROM moved", vec![]),
            Duration::from_secs(5),
        )
        .unwrap();
    match rows.rows[0].values[0] {
        ExactSqlValue::Integer(count) => count,
        ref other => panic!("count(*) must be an integer, got {other:?}"),
    }
}
