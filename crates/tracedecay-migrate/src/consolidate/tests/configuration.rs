//! Configuration migration-lineage consolidation tests.

use super::*;

async fn insert_configuration_quarantine(
    path: &Path,
    source_kind: &str,
    source_key_digest: &str,
    reason_code: &str,
    redacted_value_digest: &str,
) {
    let (database, _) = test_open(path).await;
    let transaction = database
        .begin_write_transaction("insert configuration quarantine fixture")
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO configuration_migration_quarantine(
                 source_kind, source_key_digest, reason_code,
                 redacted_value_digest, quarantined_at
             ) VALUES(?1, ?2, ?3, ?4, 41)",
            params![
                source_kind,
                source_key_digest,
                reason_code,
                redacted_value_digest
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    database.checkpoint().await.unwrap();
    database.close();
}

async fn configuration_quarantine_count(path: &Path, source_key_digest: &str) -> i64 {
    let (database, _) = test_open_read_only(path).await;
    let snapshot = database
        .begin_engine_read_snapshot("read configuration quarantine fixture")
        .await
        .unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM configuration_migration_quarantine
             WHERE source_key_digest=?1",
            params![source_key_digest],
        )
        .await
        .unwrap();
    let count = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    drop(rows);
    drop(snapshot);
    database.close();
    count
}

#[tokio::test]
async fn consolidation_preserves_configuration_quarantine_lineage() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    insert_configuration_quarantine(
        &source.sessions_db_path,
        "legacy_file",
        "sha256:source-key",
        "invalid_scope",
        "sha256:redacted-value",
    )
    .await;

    let options = fixture.options();
    let report = plan(&options).await.unwrap();
    let applied = apply(&options, &report.confirmation_token).await.unwrap();
    let destination = applied
        .destination_data_root
        .join(storage::SESSIONS_DB_FILENAME);

    assert_eq!(
        configuration_quarantine_count(&destination, "sha256:source-key").await,
        1,
        "source quarantine lineage must survive consolidation"
    );
}
