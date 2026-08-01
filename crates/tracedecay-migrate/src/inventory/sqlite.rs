use std::path::{Path, PathBuf};

use crate::inventory::{GlobalDbInventory, InventoryIntegrityMode, SqliteIntegrityOutcome};
use crate::root_seam::db::engine::{Error as EngineError, QueryExecutor, params};
use crate::root_seam::global_db::{self, RegisteredGlobalDb};

pub(super) async fn inspect_global_db(
    path: &Path,
    path_overridden: bool,
    integrity: InventoryIntegrityMode,
) -> GlobalDbInventory {
    let exists = path.is_file();
    let mut warnings = Vec::new();
    let mut unavailable_reason = None;

    if exists {
        let authority = crate::root_seam::db::DatabaseAuthority::for_runtime(
            path,
            "inspect global database offline",
        );
        if let Err(error) = authority.as_ref() {
            let warning = format!(
                "global DB '{}' is owned by the daemon; stop it before offline inventory: {error}",
                path.display()
            );
            unavailable_reason = Some(warning.clone());
            warnings.push(warning);
        }
        if authority.is_ok() {
            drop(authority);
            let scratch_root = path.parent().unwrap_or_else(|| Path::new("."));
            match crate::root_seam::sqlite_read_snapshot::open_in(path, scratch_root).await {
                Ok(db) => {
                    return inventory_from_connection(
                        path,
                        path_overridden,
                        db.connection(),
                        warnings,
                        integrity,
                    )
                    .await;
                }
                Err(error) => {
                    let warning =
                        format!("could not snapshot global DB '{}': {error}", path.display());
                    unavailable_reason = Some(warning.clone());
                    warnings.push(warning);
                }
            }
        }
    }
    let integrity = if !should_verify_integrity(integrity) {
        SqliteIntegrityOutcome::NotChecked
    } else if !exists {
        SqliteIntegrityOutcome::NoData {
            reason: format!("global DB '{}' does not exist", path.display()),
        }
    } else {
        SqliteIntegrityOutcome::Unavailable {
            reason: unavailable_reason.unwrap_or_else(|| {
                format!("global DB '{}' could not be inspected", path.display())
            }),
        }
    };

    GlobalDbInventory {
        path: path.to_path_buf(),
        exists,
        path_overridden,
        accounting_mode: global_db::global_accounting_mode().as_str().to_string(),
        legacy_home_fallback: false,
        project_count: 0,
        session_count: 0,
        lcm_raw_message_count: 0,
        registered_project_paths: Vec::new(),
        integrity,
        warnings,
    }
}

pub(super) async fn inspect_daemon_global_db(
    global_db: &RegisteredGlobalDb,
    path_overridden: bool,
    integrity: InventoryIntegrityMode,
) -> GlobalDbInventory {
    let path = global_db.db_path();
    match global_db.read_snapshot().await {
        Ok(snapshot) => {
            inventory_from_connection(path, path_overridden, &snapshot, Vec::new(), integrity).await
        }
        Err(error) => GlobalDbInventory {
            path: path.to_path_buf(),
            exists: path.is_file(),
            path_overridden,
            accounting_mode: global_db::global_accounting_mode().as_str().to_string(),
            legacy_home_fallback: false,
            project_count: 0,
            session_count: 0,
            lcm_raw_message_count: 0,
            registered_project_paths: Vec::new(),
            integrity: if should_verify_integrity(integrity) {
                SqliteIntegrityOutcome::Unavailable {
                    reason: format!("could not snapshot global DB '{}': {error}", path.display()),
                }
            } else {
                SqliteIntegrityOutcome::NotChecked
            },
            warnings: vec![format!(
                "could not snapshot global DB '{}': {error}",
                path.display()
            )],
        },
    }
}

async fn inventory_from_connection<Q>(
    path: &Path,
    path_overridden: bool,
    conn: &Q,
    mut warnings: Vec<String>,
    integrity: InventoryIntegrityMode,
) -> GlobalDbInventory
where
    Q: QueryExecutor + ?Sized,
{
    let integrity = if should_verify_integrity(integrity) {
        sqlite_quick_check_connection(conn).await
    } else {
        SqliteIntegrityOutcome::NotChecked
    };
    if let Some(detail) = integrity_warning(path, &integrity) {
        warnings.push(detail);
    }
    GlobalDbInventory {
        path: path.to_path_buf(),
        exists: path.is_file(),
        path_overridden,
        accounting_mode: global_db::global_accounting_mode().as_str().to_string(),
        legacy_home_fallback: false,
        project_count: table_count(conn, "projects").await,
        session_count: table_count(conn, "sessions").await,
        lcm_raw_message_count: table_count(conn, "lcm_raw_messages").await,
        registered_project_paths: project_paths(conn).await,
        integrity,
        warnings,
    }
}

fn should_verify_integrity(integrity: InventoryIntegrityMode) -> bool {
    integrity == InventoryIntegrityMode::Full
}

pub(super) async fn sqlite_quick_check(path: &Path) -> SqliteIntegrityOutcome {
    let authority = match crate::root_seam::db::DatabaseAuthority::for_runtime(
        path,
        "quick-check SQLite database offline",
    ) {
        Ok(authority) => authority,
        Err(error) => {
            return SqliteIntegrityOutcome::Unavailable {
                reason: format!("could not acquire offline database authority: {error}"),
            };
        }
    };
    drop(authority);
    let scratch_root = path.parent().unwrap_or_else(|| Path::new("."));
    let db = match crate::root_seam::sqlite_read_snapshot::open_in(path, scratch_root).await {
        Ok(db) => db,
        Err(error) => {
            return SqliteIntegrityOutcome::Unavailable {
                reason: format!("could not open SQLite snapshot: {error}"),
            };
        }
    };
    sqlite_quick_check_connection(db.connection()).await
}

async fn sqlite_quick_check_connection<Q>(conn: &Q) -> SqliteIntegrityOutcome
where
    Q: QueryExecutor + ?Sized,
{
    let mut rows = match conn.query("PRAGMA quick_check", ()).await {
        Ok(rows) => rows,
        Err(error) => return classify_quick_check_failure("could not run quick_check", error),
    };
    let mut values = Vec::new();
    loop {
        match rows.next().await {
            Ok(Some(row)) => values.push(row.get::<String>(0).map_err(|error| error.to_string())),
            Ok(None) => break,
            Err(error) => {
                return SqliteIntegrityOutcome::Unavailable {
                    reason: format!("could not read quick_check result: {error}"),
                };
            }
        }
    }
    classify_quick_check_values(values)
}

fn classify_quick_check_failure(
    operation: &'static str,
    error: EngineError,
) -> SqliteIntegrityOutcome {
    const SQLITE_CORRUPT: i32 = 11;
    const SQLITE_NOTADB: i32 = 26;

    let detail = format!("{operation}: {error}");
    match error.sqlite_code().map(|code| code & 0xff) {
        Some(SQLITE_CORRUPT | SQLITE_NOTADB) => SqliteIntegrityOutcome::Damaged {
            details: vec![detail],
        },
        _ => SqliteIntegrityOutcome::Unavailable { reason: detail },
    }
}

fn classify_quick_check_values(values: Vec<Result<String, String>>) -> SqliteIntegrityOutcome {
    let mut decoded = Vec::with_capacity(values.len());
    for value in values {
        match value {
            Ok(value) => decoded.push(value),
            Err(error) => {
                return SqliteIntegrityOutcome::Unavailable {
                    reason: format!("could not decode quick_check result: {error}"),
                };
            }
        }
    }
    if decoded.is_empty() {
        return SqliteIntegrityOutcome::NoData {
            reason: "quick_check returned no rows".to_string(),
        };
    }
    let details = decoded
        .into_iter()
        .filter(|value| value != "ok")
        .collect::<Vec<_>>();
    if details.is_empty() {
        SqliteIntegrityOutcome::Verified
    } else {
        SqliteIntegrityOutcome::Damaged { details }
    }
}

fn integrity_warning(path: &Path, outcome: &SqliteIntegrityOutcome) -> Option<String> {
    match outcome {
        SqliteIntegrityOutcome::Damaged { details } => Some(format!(
            "global DB '{}' quick_check reported damage: {}",
            path.display(),
            details.join("; ")
        )),
        SqliteIntegrityOutcome::Unavailable { reason } => Some(format!(
            "global DB '{}' quick_check could not be performed: {reason}",
            path.display()
        )),
        SqliteIntegrityOutcome::NoData { reason } => Some(format!(
            "global DB '{}' had no integrity result: {reason}",
            path.display()
        )),
        SqliteIntegrityOutcome::NotChecked | SqliteIntegrityOutcome::Verified => None,
    }
}

async fn table_count<Q>(conn: &Q, table: &str) -> u64
where
    Q: QueryExecutor + ?Sized,
{
    if !table_exists(conn, table).await {
        return 0;
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let Ok(mut rows) = conn.query(&sql, ()).await else {
        return 0;
    };
    let Ok(Some(row)) = rows.next().await else {
        return 0;
    };
    row.get::<i64>(0).unwrap_or(0).max(0) as u64
}

async fn table_exists<Q>(conn: &Q, table: &str) -> bool
where
    Q: QueryExecutor + ?Sized,
{
    let Ok(mut rows) = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
        )
        .await
    else {
        return false;
    };
    matches!(rows.next().await, Ok(Some(_)))
}

async fn project_paths<Q>(conn: &Q) -> Vec<PathBuf>
where
    Q: QueryExecutor + ?Sized,
{
    if !table_exists(conn, "projects").await {
        return Vec::new();
    }
    let Ok(mut rows) = conn.query("SELECT path FROM projects", ()).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            paths.push(PathBuf::from(path));
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::{
        EngineError, InventoryIntegrityMode, SqliteIntegrityOutcome, classify_quick_check_failure,
        classify_quick_check_values, should_verify_integrity,
    };

    #[test]
    fn metadata_only_inventory_skips_global_integrity_verification() {
        assert!(!should_verify_integrity(
            InventoryIntegrityMode::MetadataOnly
        ));
        assert!(should_verify_integrity(InventoryIntegrityMode::Full));
    }

    #[test]
    fn quick_check_outcomes_distinguish_damage_unavailable_and_no_data() {
        assert_eq!(
            classify_quick_check_values(vec![Ok(
                "row 5 missing from index facts_by_key".to_string()
            )]),
            SqliteIntegrityOutcome::Damaged {
                details: vec!["row 5 missing from index facts_by_key".to_string()]
            }
        );
        assert_eq!(
            classify_quick_check_values(vec![Err("column 0 was not text".to_string())]),
            SqliteIntegrityOutcome::Unavailable {
                reason: "could not decode quick_check result: column 0 was not text".to_string()
            }
        );
        assert_eq!(
            classify_quick_check_values(Vec::new()),
            SqliteIntegrityOutcome::NoData {
                reason: "quick_check returned no rows".to_string()
            }
        );
    }
    #[test]
    fn quick_check_sqlite_corruption_error_is_specific_damage() {
        let outcome = classify_quick_check_failure(
            "could not run quick_check",
            EngineError::Sqlite {
                operation: "read snapshot row",
                code: Some(11),
                extended_code: Some(11),
                message: "database disk image is malformed".to_string(),
            },
        );

        assert!(matches!(
            outcome,
            SqliteIntegrityOutcome::Damaged { details }
                if details.iter().any(|detail| detail.contains("malformed"))
        ));
    }
}
