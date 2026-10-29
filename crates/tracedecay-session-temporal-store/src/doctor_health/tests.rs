use crate::{SessionTemporalAccess, SessionTemporalHealthFindingKind, SessionTemporalHealthStatus};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;

#[asupersync::test]
async fn temporal_health_adapter_is_read_only_and_clean_on_canonical_schema() {
    let runtime = RegisteredGlobalDbHarness::open("doctor temporal health adapter").await;
    let db = runtime.registered.as_ref();
    let db_path = db.db_path().to_path_buf();
    // Keep the byte-level assertion stable while diagnosis runs through the
    // retained registered reader pool.
    db.checkpoint_result().await.unwrap();
    let before = std::fs::read(&db_path).unwrap();
    let before_family = temporal_family_manifest(&db_path);

    let report = SessionTemporalAccess::new(db)
        .session_temporal_doctor_health()
        .await;

    let encoded = serde_json::to_value(report).unwrap();
    assert_eq!(encoded["status"], "complete");
    assert_eq!(encoded["findings"], serde_json::json!([]));
    assert!(encoded.get("reason").is_none());
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        before,
        "temporal health diagnosis must not mutate the authoritative database"
    );
    assert_eq!(temporal_family_manifest(&db_path), before_family);
}

fn temporal_family_manifest(db_path: &Path) -> BTreeMap<String, (u64, Option<SystemTime>)> {
    let mut manifest = BTreeMap::new();
    for path in [
        db_path.to_path_buf(),
        {
            let mut wal = db_path.as_os_str().to_os_string();
            wal.push("-wal");
            PathBuf::from(wal)
        },
        {
            let mut shm = db_path.as_os_str().to_os_string();
            shm.push("-shm");
            PathBuf::from(shm)
        },
    ] {
        if let Ok(metadata) = std::fs::metadata(&path) {
            manifest.insert(
                path.file_name().unwrap().to_string_lossy().into_owned(),
                (metadata.len(), metadata.modified().ok()),
            );
        }
    }
    manifest
}

#[asupersync::test]
async fn temporal_health_detects_index_and_column_migration_gaps() {
    let runtime = RegisteredGlobalDbHarness::open("doctor temporal migration gap test").await;
    let db = runtime.registered.as_ref();
    let writer = db.writer_connection().unwrap();
    writer
        .execute(
            "DROP INDEX IF EXISTS idx_session_occurrences_generation_order",
            (),
        )
        .await
        .unwrap();
    writer
        .execute(
            "ALTER TABLE session_occurrences ADD COLUMN doctor_probe_column TEXT",
            (),
        )
        .await
        .unwrap();
    let report = serde_json::to_value(
        SessionTemporalAccess::new(db)
            .session_temporal_doctor_health()
            .await,
    )
    .unwrap();
    assert_eq!(report["status"], "partial");
    let findings = report["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|finding| {
            finding["kind"] == "migration_gap" && finding["count"].as_u64().unwrap_or(0) >= 2
        }),
        "{report}"
    );
}

#[asupersync::test]
async fn temporal_health_does_not_block_on_a_saturated_general_reader_lane() {
    let runtime = RegisteredGlobalDbHarness::open("doctor temporal health concurrent reader").await;
    let db = runtime.registered.as_ref();
    let occupancy = db
        .read_connection()
        .reader_pool_occupancy()
        .expect("reader pool occupancy");
    let mut held = Vec::new();
    for _ in 0..occupancy.available_general {
        held.push(
            db.read_snapshot()
                .await
                .expect("hold a general-lane snapshot"),
        );
    }
    assert_eq!(
        db.read_connection()
            .reader_pool_occupancy()
            .map(|snapshot| snapshot.available_general),
        Some(0),
        "the general lane must stay leased for the whole diagnosis"
    );

    let report = SessionTemporalAccess::new(db)
        .session_temporal_doctor_health()
        .await;

    assert_eq!(
        report.status(),
        SessionTemporalHealthStatus::Complete,
        "{report:?}"
    );
    assert!(
        !report.findings().iter().any(|finding| {
            finding.kind() == SessionTemporalHealthFindingKind::RelationGraphUnavailable
        }),
        "relation health must use the reserved health snapshot, not the general lane: {report:?}"
    );
    drop(held);
}
