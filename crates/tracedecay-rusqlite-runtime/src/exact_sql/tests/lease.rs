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
    assert!(started.elapsed() < Duration::from_secs(2));
    channel
        .execute_batch("CREATE TABLE after_absolute_expiry (value INTEGER)".to_owned())
        .unwrap();
}
