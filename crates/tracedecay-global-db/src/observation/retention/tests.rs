use crate::db::engine::{Connection, TestConnection, params};

use super::*;
// `super::*` re-exports the crate's one-argument `errors::Result` alias; the
// tests want the standard two-argument `Result` for their `Result<_, String>`
// signatures, so shadow it back.
use std::result::Result;

const DAY: i64 = 24 * 60 * 60;
const NOW: i64 = 1_900_000_000;
const OWNER: &str = "{\"owner\":\"o1\"}";
const GEN: &str = "projection.gen.v1";
const OTHER_GEN: &str = "projection.gen.v2";

async fn test_store() -> Result<(tempfile::TempDir, TestConnection), String> {
    let temp = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
    let conn = TestConnection::open(&temp.path().join("obs.db"));
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .await
        .map_err(|err| format!("enable fks: {err}"))?;
    super::super::ensure_observation_schema(&conn)
        .await
        .map_err(|err| format!("ensure observation schema: {err}"))?;
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS observations_immutable_update
         BEFORE UPDATE ON observations BEGIN
             SELECT RAISE(ABORT, 'observations are immutable');
         END;
         CREATE TRIGGER IF NOT EXISTS observations_immutable_delete
         BEFORE DELETE ON observations BEGIN
             SELECT RAISE(ABORT, 'observations are immutable');
         END;",
    )
    .await
    .map_err(|err| format!("install observation immutability: {err}"))?;
    Ok((temp, conn))
}

/// Fat JSON payload of `size` bytes that carries no `__retention_released` key.
fn payload(field: &str, size: usize) -> String {
    format!("{{\"{field}\":\"{}\"}}", "x".repeat(size))
}

/// Seeds a full evidence cluster (receipt + observation + anchor + binding +
/// provenance) for `anchor_id`, all owned by `OWNER` under `generation`, with
/// fat payloads. The observation id mirrors the anchor id.
async fn seed_evidence(
    conn: &Connection,
    anchor_id: &str,
    generation: &str,
    size: usize,
) -> Result<(), String> {
    let observation_id = format!("obs-{anchor_id}");
    let receipt_id = format!("receipt-{anchor_id}");
    conn.execute(
        "INSERT INTO sanitization_receipts(receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, 'v1', 'digest', '{}')",
        params![receipt_id.as_str()],
    )
    .await
    .map_err(|err| format!("insert receipt: {err}"))?;
    conn.execute(
        "INSERT INTO observations(observation_id, payload_digest, receipt_id,
             observation_json, committed_cursor_json)
         VALUES (?1, 'digest', ?2, ?3, '{}')",
        params![
            observation_id.as_str(),
            receipt_id.as_str(),
            payload("body", size)
        ],
    )
    .await
    .map_err(|err| format!("insert observation: {err}"))?;
    insert_anchor(conn, anchor_id, generation, size).await?;
    conn.execute(
        "INSERT INTO observation_retrieval_anchors(observation_id, anchor_id)
         VALUES (?1, ?2)",
        params![observation_id.as_str(), anchor_id],
    )
    .await
    .map_err(|err| format!("insert binding: {err}"))?;
    conn.execute(
        "INSERT INTO observation_repository_provenance(observation_id,
             availability_json, capture_json, retrieval_anchor_id, owner_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation_id.as_str(),
            payload("avail", size),
            payload("capture", size),
            anchor_id,
            OWNER
        ],
    )
    .await
    .map_err(|err| format!("insert provenance: {err}"))?;
    Ok(())
}

/// Inserts a bare anchor (used for evidence clusters and as a supersession
/// successor target).
async fn insert_anchor(
    conn: &Connection,
    anchor_id: &str,
    generation: &str,
    size: usize,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO retrieval_anchors(anchor_id, anchor_json, owner_json, projection_generation)
         VALUES (?1, ?2, ?3, ?4)",
        params![anchor_id, payload("target", size), OWNER, generation],
    )
    .await
    .map_err(|err| format!("insert anchor: {err}"))?;
    Ok(())
}

/// Appends a disposition to the append-only ledger.
async fn set_disposition(
    conn: &Connection,
    anchor_id: &str,
    state: &str,
    effective_at: i64,
    superseded_by: Option<&str>,
) -> Result<(), String> {
    let disposition_id = format!("disp-{anchor_id}-{state}-{effective_at}");
    conn.execute(
        "INSERT INTO retrieval_anchor_dispositions(disposition_id, anchor_id, owner_json,
             state, superseded_by, reason_class, effective_at, record_json)
         VALUES (?1, ?2, ?3, ?4, ?5, 'retention', ?6, '{}')",
        params![
            disposition_id.as_str(),
            anchor_id,
            OWNER,
            state,
            superseded_by,
            effective_at
        ],
    )
    .await
    .map_err(|err| format!("insert disposition: {err}"))?;
    Ok(())
}

async fn fetch_i64(conn: &Connection, sql: &str) -> Result<i64, String> {
    let mut rows = conn.query(sql, ()).await.map_err(|e| e.to_string())?;
    rows.next()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no row".to_string())?
        .get::<i64>(0)
        .map_err(|e| e.to_string())
}

async fn fetch_str(conn: &Connection, sql: &str) -> Result<String, String> {
    let mut rows = conn.query(sql, ()).await.map_err(|e| e.to_string())?;
    rows.next()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no row".to_string())?
        .get::<String>(0)
        .map_err(|e| e.to_string())
}

async fn seed_cursor_advance_history(conn: &Connection) -> Result<(), String> {
    let source = r#"{"session_id":"retention-session"}"#;
    let scope = r#""profile""#;
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS source_cursor_advances_immutable_update_v1
         BEFORE UPDATE ON source_cursor_advances BEGIN
             SELECT RAISE(ABORT, 'source cursor advances are immutable');
         END;
         CREATE TRIGGER IF NOT EXISTS source_cursor_advances_immutable_delete_v1
         BEFORE DELETE ON source_cursor_advances BEGIN
             SELECT RAISE(ABORT, 'source cursor advances are immutable');
         END;",
    )
    .await
    .map_err(|error| format!("install cursor immutability: {error}"))?;
    conn.execute(
        "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
         VALUES (?1, ?2, ?3)",
        params![
            source,
            scope,
            r#"{"source":{"session_id":"retention-session"},"scope":"profile","generation":2,"byte_offset":30}"#
        ],
    )
    .await
    .map_err(|error| format!("insert current cursor: {error}"))?;
    for (generation, start, end) in [(1, 0, 10), (2, 10, 20), (2, 20, 30)] {
        let coverage = format!(
            r#"{{"generation":{generation},"ordering_domain":"file_bytes","range":{{"start":{start},"end":{end}}}}}"#
        );
        conn.execute(
            "INSERT INTO source_cursor_advances(
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES (?1, ?2, ?3, 'blank_frame', NULL)",
            params![source, scope, coverage],
        )
        .await
        .map_err(|error| format!("insert cursor advance: {error}"))?;
    }
    Ok(())
}

fn released_config() -> ObservationRetentionConfig {
    ObservationRetentionConfig {
        enabled: true,
        anchor_release_after_days: Some(30),
        observation_release_after_days: Some(30),
        provenance_release_after_days: Some(30),
        ..ObservationRetentionConfig::default()
    }
}

fn is_released(json: &str) -> bool {
    json.contains("__retention_released")
}

async fn run_apply(
    conn: &Connection,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
) -> Result<ObservationRetentionReport, String> {
    run_observation_retention_authorized(
        conn,
        generation,
        config,
        RetentionMode::Apply,
        NOW,
        &|_| Ok(()),
    )
    .await
    .map_err(|error| error.to_string())
}

#[tokio::test]
async fn authority_loss_before_commit_rolls_back_observation_release() -> Result<(), String> {
    let (temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-revoked", GEN, 4096).await?;
    set_disposition(&conn, "anchor-revoked", "deleted", NOW - 90 * DAY, None).await?;
    let scope = std::sync::Mutex::new(Some(
        crate::db::enter_daemon_database_scope(
            temp.path(),
            1,
            "observation-retention-revocation-test",
        )
        .map_err(|error| error.to_string())?,
    ));
    let authority = crate::db::DatabaseAuthority::for_runtime(
        &temp.path().join("sessions.db"),
        "observation retention revocation test",
    )
    .map_err(|error| error.to_string())?;

    let error = run_observation_retention_authorized(
        &conn,
        None,
        &released_config(),
        RetentionMode::Apply,
        NOW,
        &|intent| {
            if intent == "commit anchor retention pass" {
                drop(
                    scope
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take(),
                );
            }
            authority.require_active_write_scope(intent)
        },
    )
    .await
    .expect_err("authority loss must reject observation retention commit");

    assert!(error.to_string().contains("active daemon"));
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors
             WHERE anchor_id = 'anchor-revoked'"
        )
        .await?
    ));
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT observation_json FROM observations
             WHERE observation_id = 'obs-anchor-revoked'"
        )
        .await?
    ));
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT availability_json FROM observation_repository_provenance
             WHERE observation_id = 'obs-anchor-revoked'"
        )
        .await?
    ));
    assert!(
        conn.execute(
            "UPDATE retrieval_anchors SET anchor_json = '{}'
             WHERE anchor_id = 'anchor-revoked'",
            (),
        )
        .await
        .is_err(),
        "rollback restores the anchor immutability trigger"
    );
    assert!(
        conn.execute(
            "UPDATE observation_repository_provenance SET availability_json = '{}'
             WHERE observation_id = 'obs-anchor-revoked'",
            (),
        )
        .await
        .is_err(),
        "provenance immutability trigger remains installed"
    );
    Ok(())
}

#[tokio::test]
async fn unauthorised_entry_rejects_apply() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    let error =
        run_observation_retention(&conn, None, &released_config(), RetentionMode::Apply, NOW)
            .await
            .expect_err("apply must require the authority-bound entry point");

    assert!(
        error
            .to_string()
            .contains("authority-bound observation retention entry point")
    );
    Ok(())
}

// superseded and deleted dispositions release their storage: all three fat
// payloads collapse to the compact marker and the reclaim is measurable.
#[tokio::test]
async fn superseded_and_deleted_dispositions_release_storage() -> Result<(), String> {
    for state in ["superseded", "deleted"] {
        let (_temp, conn) = test_store().await?;
        // A successor anchor is required for the FK on a superseded disposition.
        let successor = if state == "superseded" {
            insert_anchor(&conn, "successor", GEN, 8).await?;
            Some("successor")
        } else {
            None
        };
        seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
        set_disposition(&conn, "anchor-1", state, NOW - 90 * DAY, successor).await?;

        let report = run_apply(&conn, None, &released_config()).await?;

        assert_eq!(report.anchors_released.acted, 1, "{state}: anchor released");
        assert_eq!(
            report.observations_released.acted, 1,
            "{state}: observation released"
        );
        assert_eq!(
            report.provenance_released.acted, 1,
            "{state}: provenance released"
        );
        assert!(
            report.bytes_reclaimed() > 4096,
            "{state}: reclaim measurable"
        );

        assert!(is_released(
            &fetch_str(
                &conn,
                "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'anchor-1'"
            )
            .await?
        ));
        assert!(is_released(
            &fetch_str(
                &conn,
                "SELECT observation_json FROM observations WHERE observation_id = 'obs-anchor-1'"
            )
            .await?
        ));
        assert!(is_released(
            &fetch_str(&conn, "SELECT availability_json FROM observation_repository_provenance WHERE observation_id = 'obs-anchor-1'").await?
        ));
    }
    Ok(())
}

// Active and unavailable dispositions retain their storage even when old: live
// and source-unavailable evidence is never released (plan 38 non-goal).
#[tokio::test]
async fn active_and_unavailable_dispositions_retain_storage() -> Result<(), String> {
    for state in ["active", "unavailable"] {
        let (_temp, conn) = test_store().await?;
        seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
        set_disposition(&conn, "anchor-1", state, NOW - 90 * DAY, None).await?;

        let report = run_apply(&conn, None, &released_config()).await?;

        assert_eq!(report.anchors_released.acted, 0, "{state}: anchor retained");
        assert_eq!(report.observations_released.acted, 0);
        assert_eq!(report.provenance_released.acted, 0);
        assert!(!is_released(
            &fetch_str(
                &conn,
                "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'anchor-1'"
            )
            .await?
        ));
    }
    Ok(())
}

// The latest ledger entry governs: an anchor superseded then later re-activated
// has a current 'active' state and is retained (append-only, max-sequence wins).
#[tokio::test]
async fn latest_disposition_wins() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    insert_anchor(&conn, "successor", GEN, 8).await?;
    seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
    set_disposition(
        &conn,
        "anchor-1",
        "superseded",
        NOW - 90 * DAY,
        Some("successor"),
    )
    .await?;
    // A later, higher-sequence entry restores the anchor to active.
    set_disposition(&conn, "anchor-1", "active", NOW - 80 * DAY, None).await?;

    let report = run_apply(&conn, None, &released_config()).await?;

    assert_eq!(
        report.anchors_released.acted, 0,
        "current active state retained"
    );
    Ok(())
}

// The retention window is honored: a superseded disposition inside the window
// is not released.
#[tokio::test]
async fn window_is_honored() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "recent", GEN, 4096).await?;
    seed_evidence(&conn, "old", GEN, 4096).await?;
    set_disposition(&conn, "recent", "deleted", NOW - 10 * DAY, None).await?;
    set_disposition(&conn, "old", "deleted", NOW - 90 * DAY, None).await?;

    let report = run_apply(&conn, None, &released_config()).await?;

    assert_eq!(
        report.anchors_released.acted, 1,
        "only the >30d anchor released"
    );
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'recent'"
        )
        .await?
    ));
    assert!(is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'old'"
        )
        .await?
    ));
    Ok(())
}

// A dry run counts eligible reclaim without mutating anything.
#[tokio::test]
async fn dry_run_mutates_nothing() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
    set_disposition(&conn, "anchor-1", "deleted", NOW - 90 * DAY, None).await?;

    let report =
        run_observation_retention(&conn, None, &released_config(), RetentionMode::DryRun, NOW)
            .await
            .map_err(|e| e.to_string())?;

    assert_eq!(report.anchors_released.eligible, 1);
    assert_eq!(report.anchors_released.acted, 0, "dry run acts on nothing");
    assert_eq!(
        report.anchors_released.oldest_eligible_at,
        Some(NOW - 90 * DAY),
        "backlog age comes from the governing disposition"
    );
    assert!(report.bytes_reclaimed() > 4096, "dry run still measures");
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'anchor-1'"
        )
        .await?
    ));
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT observation_json FROM observations WHERE observation_id = 'obs-anchor-1'"
        )
        .await?
    ));
    Ok(())
}

// Every observation has exactly one immutable anchor binding. Retention counts
// and releases each eligible observation once.
#[tokio::test]
async fn released_observations_are_counted_once_each() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-primary", GEN, 4096).await?;
    seed_evidence(&conn, "anchor-secondary", GEN, 4096).await?;
    set_disposition(&conn, "anchor-primary", "deleted", NOW - 90 * DAY, None).await?;
    set_disposition(&conn, "anchor-secondary", "deleted", NOW - 60 * DAY, None).await?;
    let config = ObservationRetentionConfig {
        enabled: true,
        observation_release_after_days: Some(30),
        ..ObservationRetentionConfig::default()
    };

    let report = run_apply(&conn, None, &config).await?;

    assert_eq!(report.observations_released.eligible, 2);
    assert_eq!(report.observations_released.acted, 2);
    assert_eq!(
        report.observations_released.oldest_eligible_at,
        Some(NOW - 90 * DAY)
    );
    assert!(is_released(
        &fetch_str(
            &conn,
            "SELECT observation_json FROM observations
             WHERE observation_id = 'obs-anchor-primary'"
        )
        .await?
    ));
    assert!(is_released(
        &fetch_str(
            &conn,
            "SELECT observation_json FROM observations
             WHERE observation_id = 'obs-anchor-secondary'"
        )
        .await?
    ));
    Ok(())
}

#[tokio::test]
async fn active_observation_in_other_generation_remains_live() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-primary", GEN, 4096).await?;
    seed_evidence(&conn, "anchor-active", OTHER_GEN, 4096).await?;
    set_disposition(&conn, "anchor-primary", "deleted", NOW - 90 * DAY, None).await?;
    set_disposition(&conn, "anchor-active", "active", NOW - 60 * DAY, None).await?;
    let config = ObservationRetentionConfig {
        enabled: true,
        observation_release_after_days: Some(30),
        ..ObservationRetentionConfig::default()
    };

    let report = run_apply(&conn, Some(GEN), &config).await?;

    assert_eq!(report.observations_released.eligible, 1);
    assert_eq!(report.observations_released.acted, 1);
    assert!(is_released(
        &fetch_str(
            &conn,
            "SELECT observation_json FROM observations
             WHERE observation_id = 'obs-anchor-primary'"
        )
        .await?
    ));
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT observation_json FROM observations
             WHERE observation_id = 'obs-anchor-active'"
        )
        .await?
    ));
    Ok(())
}

// Reclaim is measurable via payload-count and page/free-list metrics.
#[tokio::test]
async fn reports_measurable_reclaim_metrics() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    for index in 0..8 {
        let anchor = format!("anchor-{index}");
        seed_evidence(&conn, &anchor, GEN, 2048).await?;
        set_disposition(&conn, &anchor, "deleted", NOW - 90 * DAY, None).await?;
    }

    let report = run_apply(&conn, None, &released_config()).await?;

    assert_eq!(report.anchor_payloads_before, 8);
    assert_eq!(
        report.anchor_payloads_after, 0,
        "payload-count delta measurable"
    );
    assert_eq!(report.observation_payloads_before, 8);
    assert_eq!(report.observation_payloads_after, 0);
    assert!(report.page_count_before > 0, "page_count observed");
    assert!(
        report.freelist_after >= report.freelist_before,
        "released payloads freed pages"
    );
    assert!(report.bytes_reclaimed() >= 8 * 2048);
    Ok(())
}

// Retention is generation-scoped: only the targeted generation is released.
#[tokio::test]
async fn retention_is_generation_scoped() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "gen-a", GEN, 4096).await?;
    seed_evidence(&conn, "gen-b", OTHER_GEN, 4096).await?;
    set_disposition(&conn, "gen-a", "deleted", NOW - 90 * DAY, None).await?;
    set_disposition(&conn, "gen-b", "deleted", NOW - 90 * DAY, None).await?;

    let report = run_apply(&conn, Some(GEN), &released_config()).await?;

    assert_eq!(report.anchors_released.acted, 1, "only GEN released");
    assert!(is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'gen-a'"
        )
        .await?
    ));
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'gen-b'"
        )
        .await?
    ));
    Ok(())
}

// Disabled config is an inert no-op even in Apply mode.
#[tokio::test]
async fn disabled_config_is_a_no_op() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
    set_disposition(&conn, "anchor-1", "deleted", NOW - 90 * DAY, None).await?;

    let config = ObservationRetentionConfig {
        enabled: false,
        anchor_release_after_days: Some(1),
        ..ObservationRetentionConfig::default()
    };
    let report = run_apply(&conn, None, &config).await?;

    assert_eq!(report.anchors_released.acted, 0);
    assert!(!is_released(
        &fetch_str(
            &conn,
            "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = 'anchor-1'"
        )
        .await?
    ));
    Ok(())
}

// A second run is idempotent: already-released rows carry the marker and are
// skipped rather than re-acted.
#[tokio::test]
async fn rerun_is_idempotent() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
    set_disposition(&conn, "anchor-1", "deleted", NOW - 90 * DAY, None).await?;

    let first = run_apply(&conn, None, &released_config()).await?;
    assert_eq!(first.anchors_released.acted, 1);

    let second = run_apply(&conn, None, &released_config()).await?;
    assert_eq!(second.anchors_released.acted, 0, "already-released skipped");
    assert_eq!(second.observations_released.acted, 0);
    assert_eq!(second.provenance_released.acted, 0);
    Ok(())
}

// The immutability triggers are restored after a release: direct UPDATE/DELETE
// on the anchor and provenance tables still ABORT, and the append-only
// disposition ledger is left fully intact.
#[tokio::test]
async fn immutability_and_ledger_are_preserved() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_evidence(&conn, "anchor-1", GEN, 4096).await?;
    set_disposition(&conn, "anchor-1", "deleted", NOW - 90 * DAY, None).await?;
    let ledger_before =
        fetch_i64(&conn, "SELECT COUNT(*) FROM retrieval_anchor_dispositions").await?;

    run_apply(&conn, None, &released_config()).await?;

    // Immutability triggers are back in force.
    assert!(
        conn.execute(
            "UPDATE retrieval_anchors SET anchor_json = '{}' WHERE anchor_id = 'anchor-1'",
            (),
        )
        .await
        .is_err(),
        "anchor update trigger restored"
    );
    assert!(
        conn.execute(
            "DELETE FROM retrieval_anchors WHERE anchor_id = 'anchor-1'",
            ()
        )
        .await
        .is_err(),
        "anchor delete trigger intact"
    );
    assert!(
        conn.execute(
            "UPDATE observation_repository_provenance SET availability_json = '{}'
             WHERE observation_id = 'obs-anchor-1'",
            (),
        )
        .await
        .is_err(),
        "provenance update trigger restored"
    );
    // The ledger is untouched and still immutable.
    let ledger_after =
        fetch_i64(&conn, "SELECT COUNT(*) FROM retrieval_anchor_dispositions").await?;
    assert_eq!(ledger_before, ledger_after, "ledger row count unchanged");
    assert!(
        conn.execute(
            "DELETE FROM retrieval_anchor_dispositions WHERE anchor_id = 'anchor-1'",
            (),
        )
        .await
        .is_err(),
        "ledger remains append-only"
    );
    Ok(())
}

#[tokio::test]
async fn superseded_cursor_advances_are_reclaimed_but_current_receipt_survives()
-> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_cursor_advance_history(&conn).await?;

    let dry_run = run_observation_retention(
        &conn,
        None,
        &ObservationRetentionConfig::default(),
        RetentionMode::DryRun,
        NOW,
    )
    .await
    .map_err(|error| error.to_string())?;
    assert_eq!(dry_run.cursor_advances_reclaimed.eligible, 2);
    assert_eq!(dry_run.cursor_advances_reclaimed.acted, 0);
    assert_eq!(dry_run.cursor_advances_before, 3);
    assert_eq!(dry_run.cursor_advances_after, 3);

    let applied = run_apply(&conn, None, &ObservationRetentionConfig::default()).await?;
    assert_eq!(applied.cursor_advances_reclaimed.eligible, 2);
    assert_eq!(applied.cursor_advances_reclaimed.acted, 2);
    assert_eq!(applied.cursor_advances_before, 3);
    assert_eq!(applied.cursor_advances_after, 1);
    assert_eq!(
        fetch_i64(
            &conn,
            "SELECT CAST(json_extract(coverage_json, '$.range.end') AS INTEGER)
             FROM source_cursor_advances"
        )
        .await?,
        30,
        "the exact receipt supporting the current frontier remains"
    );
    assert!(
        conn.execute("DELETE FROM source_cursor_advances", ())
            .await
            .is_err(),
        "ordinary callers still cannot delete cursor-advance evidence"
    );
    Ok(())
}

#[tokio::test]
async fn cursor_advance_authority_loss_rolls_back_and_restores_trigger() -> Result<(), String> {
    let (_temp, conn) = test_store().await?;
    seed_cursor_advance_history(&conn).await?;
    let error = run_observation_retention_authorized(
        &conn,
        None,
        &ObservationRetentionConfig::default(),
        RetentionMode::Apply,
        NOW,
        &|intent| {
            if intent == "commit source cursor advance retention pass" {
                return Err(TraceDecayError::Database {
                    operation: OPERATION.to_string(),
                    message: "test authority revoked".to_string(),
                });
            }
            Ok(())
        },
    )
    .await
    .expect_err("revocation before commit must roll back cursor reclamation");
    assert!(error.to_string().contains("test authority revoked"));
    assert_eq!(
        fetch_i64(&conn, "SELECT COUNT(*) FROM source_cursor_advances").await?,
        3
    );
    assert!(
        conn.execute("DELETE FROM source_cursor_advances", ())
            .await
            .is_err(),
        "rollback restores the immutable delete trigger"
    );
    Ok(())
}
