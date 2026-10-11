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

    // The writer's test clock stands still, so no lease can end the leaked
    // transaction: only shutdown releases it.
    receive
        .recv()
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
    let clock = FakeClock::frozen();
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(FakeClockAuthority::installing(&clock))
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    clock.advance(EXACT_SQL_TRANSACTION_IDLE_LIMIT);
    // The writer is serial: this runs only once the idle lease has released
    // the transaction.
    channel
        .execute_batch("CREATE TABLE after_idle_expiry (value INTEGER)".to_owned())
        .unwrap();

    assert!(matches!(
        transaction.commit(),
        Err(ExactSqlError::TransactionExpired)
    ));
}

/// A statement that consumed its whole execution deadline and then failed is
/// writer work, not caller idleness. Charging it to the idle limit tore the
/// transaction down before the caller, still holding the statement's error,
/// could roll it back, and the caller's own rollback then reported the lease
/// expiry. The operator saw `execute batch failed: interrupted; rollback
/// failed: … lease expired`, which claims durability is in doubt.
#[test]
fn an_interrupted_statement_leaves_the_rollback_to_its_caller() {
    let fixture = fixture('a', 'a');
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(FakeClockAuthority {
            clock: FakeClock::frozen(),
            advance: Some((
                ExactSqlWriteIntent::ExecuteBatch,
                EXACT_SQL_TRANSACTION_LIMIT,
            )),
        }))
        .unwrap();
    channel
        .execute_batch("CREATE TABLE sink (value INTEGER)".to_owned())
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    // The batch's own authority check carries the writer past the
    // transaction deadline that bounds the statement.
    let error = transaction
        .execute_batch(
            "INSERT INTO sink(value)
             WITH RECURSIVE counter(value) AS (
                SELECT 1 UNION ALL SELECT value + 1 FROM counter WHERE value < 1000000
             )
             SELECT value FROM counter"
                .to_owned(),
        )
        .expect_err("the execution guard must interrupt a statement past its deadline");
    assert!(
        matches!(
            &error,
            ExactSqlError::Sqlite {
                kind: ErrorKind::Interrupted,
                ..
            }
        ),
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
/// holding the transaction, so it rolls back and publishes that receipt
/// before releasing. A caller's later rollback learns its work was discarded
/// instead of being told the rollback failed; only its commit is refused.
#[test]
fn lease_expiry_reports_the_rollback_it_performed() {
    let fixture = fixture('a', 'a');
    let clock = FakeClock::frozen();
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(FakeClockAuthority::installing(&clock))
        .unwrap();
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

    clock.advance(EXACT_SQL_TRANSACTION_IDLE_LIMIT);
    // The writer is serial: this runs only once the idle lease has released
    // the transaction.
    channel
        .execute_batch("CREATE TABLE after_lease_expiry (value INTEGER)".to_owned())
        .unwrap();

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
    let clock = FakeClock::frozen();
    let channel = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(FakeClockAuthority::installing(&clock))
        .unwrap();
    let transaction = channel.begin_immediate().unwrap();

    // Half-idle steps renew the idle lease every time, so only the absolute
    // lease can end the transaction.
    let step = EXACT_SQL_TRANSACTION_IDLE_LIMIT / 2;
    let steps = (0..EXACT_SQL_TRANSACTION_LIMIT.as_millis() / step.as_millis())
        .map(|_| {
            clock.advance(step);
            transaction.query(statement("SELECT 1", vec![])).map(|_| ())
        })
        .collect::<Vec<_>>();

    let (last, earlier) = steps.split_last().unwrap();
    assert!(earlier.iter().all(Result::is_ok), "{steps:?}");
    // Expiry lands on the first step at the absolute lease, not later.
    assert!(
        matches!(last, Err(ExactSqlError::TransactionExpired)),
        "{steps:?}"
    );
    channel
        .execute_batch("CREATE TABLE after_absolute_expiry (value INTEGER)".to_owned())
        .unwrap();
}
