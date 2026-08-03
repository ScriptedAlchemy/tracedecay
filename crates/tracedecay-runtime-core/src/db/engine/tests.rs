use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::ExactSqlHandle,
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::{Connection, Error, IntoParams, Row, Rows, Value, params, params_from_iter};

#[test]
fn params_preserve_order_nulls_and_owned_values() {
    let params = params![
        "text",
        String::from("owned"),
        42_i64,
        3.5_f64,
        vec![1_u8, 2, 3],
        Option::<i64>::None,
    ]
    .into_params()
    .unwrap();

    assert_eq!(
        params,
        vec![
            Value::Text("text".to_string()),
            Value::Text("owned".to_string()),
            Value::Integer(42),
            Value::Real(3.5),
            Value::Blob(vec![1, 2, 3]),
            Value::Null,
        ]
    );

    let iterated = params_from_iter([Value::Integer(7), Value::Text("eight".to_string())])
        .into_params()
        .unwrap();
    assert_eq!(
        iterated,
        vec![Value::Integer(7), Value::Text("eight".to_string())]
    );
}

#[tokio::test]
async fn owned_rows_decode_the_engine_value_set() {
    let row = Row::from_values(vec![
        Value::Text("node".to_string()),
        Value::Integer(9),
        Value::Null,
        Value::Blob(vec![0xff, 0x00]),
    ]);
    let mut rows = Rows::from_rows(vec![row]);
    let row = rows.next().await.unwrap().unwrap();

    assert_eq!(row.get::<String>(0).unwrap(), "node");
    assert_eq!(row.get::<i64>(1).unwrap(), 9);
    assert_eq!(row.get::<u32>(1).unwrap(), 9);
    assert_eq!(row.get::<u64>(1).unwrap(), 9);
    assert_eq!(row.get::<Option<i64>>(2).unwrap(), None);
    assert_eq!(row.get::<Value>(3).unwrap(), Value::Blob(vec![0xff, 0x00]));
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn runtime_rows_preserve_column_metadata_without_materialized_rows() {
    let fixture = runtime_fixture();
    let assert_columns = |rows: &Rows| {
        assert_eq!(rows.column_count(), 3);
        assert_eq!(rows.column_name(0), Some("first"));
        assert_eq!(rows.column_name(1), Some("duplicate"));
        assert_eq!(rows.column_name(2), Some("duplicate"));
        assert_eq!(rows.column_name(-1), None);
        assert_eq!(rows.column_name(3), None);
    };
    let sql = "SELECT 1 AS first, 2 AS duplicate, 3 AS duplicate WHERE 0";

    let rows = fixture.connection.query(sql, ()).await.unwrap();
    assert_columns(&rows);

    let transaction = fixture.connection.transaction().await.unwrap();
    let rows = transaction.query(sql, ()).await.unwrap();
    assert_columns(&rows);
    transaction.rollback().await.unwrap();

    let snapshot = fixture.connection.read_snapshot().await.unwrap();
    let rows = snapshot.query(sql, ()).await.unwrap();
    assert_columns(&rows);
}

#[test]
fn row_decode_rejects_wrong_types_and_invalid_indexes() {
    let row = Row::from_values(vec![Value::Integer(-1)]);

    assert!(row.get::<String>(0).is_err());
    assert!(row.get::<u64>(0).is_err());
    assert!(row.get::<i64>(-1).is_err());
    assert!(row.get::<i64>(1).is_err());
}

struct NoWrites;

impl StorageOperationExecutor for NoWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1> {
        unreachable!("engine exact SQL does not use the product read contract")
    }
}

struct Fixture {
    connection: Connection,
    path: std::path::PathBuf,
    _readers: ReaderPool<NoReads>,
    _writer: PersistentWriter,
    _directory: TempDir,
}

fn runtime_fixture() -> Fixture {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("engine.sqlite3");
    rusqlite::Connection::open(&path).unwrap();
    let path = path.canonicalize().unwrap();
    let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.engine-test",
            "profile_id": "profile.engine-test",
            "scope": { "kind": "project", "project_id": "project.engine-test" }
        },
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .unwrap();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    );
    let writer = PersistentWriter::start(
        ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
        AdmissionConfigV1::default(),
        NoWrites,
    )
    .unwrap();
    let readers = ReaderPool::start(
        ExistingReaderLocator::new(binding, locator, path.clone()).unwrap(),
        AdmissionConfigV1::default().readers,
        NoReads,
    )
    .unwrap();
    let connection = Connection::attach(ExactSqlHandle::attach(&writer, &readers).unwrap());
    Fixture {
        connection,
        path,
        _readers: readers,
        _writer: writer,
        _directory: directory,
    }
}

#[tokio::test]
async fn runtime_connection_and_transactions_preserve_sqlite_semantics() {
    let fixture = runtime_fixture();
    let connection = &fixture.connection;
    connection
        .execute_batch(
            "CREATE TABLE items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                label TEXT NOT NULL UNIQUE
            );",
        )
        .await
        .unwrap();

    assert_eq!(
        connection
            .execute("INSERT INTO items(label) VALUES (?1)", params!["first"])
            .await
            .unwrap(),
        1
    );
    assert_eq!(connection.last_insert_rowid(), 1);
    let statement = connection
        .prepare("INSERT INTO items(label) VALUES (?1)")
        .await
        .unwrap();
    statement.execute(params!["second"]).await.unwrap();
    statement.reset();
    assert_eq!(connection.last_insert_rowid(), 2);
    connection
        .execute_batch(
            "INSERT INTO items(label) VALUES ('batch-one');
             INSERT INTO items(label) VALUES ('batch-two');",
        )
        .await
        .unwrap();
    assert_eq!(connection.last_insert_rowid(), 4);

    let committed = connection
        .transaction_with_behavior(super::TransactionBehavior::Immediate)
        .await
        .unwrap();
    committed
        .execute("INSERT INTO items(label) VALUES (?1)", params!["committed"])
        .await
        .unwrap();
    assert_eq!(committed.last_insert_rowid(), 5);
    committed.commit().await.unwrap();
    assert_eq!(connection.last_insert_rowid(), 5);

    let rolled_back = connection
        .transaction_with_behavior(super::TransactionBehavior::Immediate)
        .await
        .unwrap();
    rolled_back
        .execute(
            "INSERT INTO items(label) VALUES (?1)",
            params!["rolled-back"],
        )
        .await
        .unwrap();
    rolled_back.rollback().await.unwrap();
    assert_eq!(connection.last_insert_rowid(), 6);

    let snapshot = connection.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query("SELECT label FROM items ORDER BY id", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "first"
    );
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "second"
    );
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "batch-one"
    );
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "batch-two"
    );
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "committed"
    );
    assert!(rows.next().await.unwrap().is_none());

    let error = connection
        .execute("INSERT INTO items(label) VALUES (?1)", params!["first"])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Sqlite {
            code: Some(_),
            extended_code: Some(_),
            ..
        }
    ));
    assert!(connection.prepare("SELEC invalid syntax").await.is_err());
}

#[tokio::test]
async fn default_transaction_is_deferred_and_does_not_take_the_write_lock() {
    let fixture = runtime_fixture();
    let deferred = fixture.connection.transaction().await.unwrap();
    let mut external = rusqlite::Connection::open(&fixture.path).unwrap();

    let external_write = external
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("default engine transaction must remain deferred");

    external_write.rollback().unwrap();
    deferred.rollback().await.unwrap();
}
