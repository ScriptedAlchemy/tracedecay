use std::path::{Path, PathBuf};

use libsql::{Builder, OpenFlags};

use super::model::{GlobalDbInventory, InventoryIntegrityMode};
use crate::global_db;

pub(super) async fn inspect_global_db(
    path: &Path,
    path_overridden: bool,
    integrity: InventoryIntegrityMode,
) -> GlobalDbInventory {
    let exists = path.is_file();
    let mut warnings = Vec::new();

    if exists {
        let authority =
            crate::db::DatabaseAuthority::for_runtime(path, "inspect global database offline");
        if let Err(error) = authority.as_ref() {
            warnings.push(format!(
                "global DB '{}' is owned by the daemon; stop it before offline inventory: {error}",
                path.display()
            ));
        }
        if authority.is_ok() {
            let db_result = Builder::new_local(path)
                .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
                .build()
                .await;
            match db_result {
                Ok(db) => match db.connect() {
                    Ok(conn) => {
                        return inventory_from_connection(
                            path,
                            path_overridden,
                            &conn,
                            warnings,
                            integrity,
                        )
                        .await;
                    }
                    Err(err) => warnings.push(format!(
                        "could not inspect global DB '{}': {err}",
                        path.display()
                    )),
                },
                Err(err) => warnings.push(format!(
                    "could not inspect global DB '{}': {err}",
                    path.display()
                )),
            }
        }
    }

    GlobalDbInventory {
        path: path.to_path_buf(),
        exists,
        path_overridden,
        accounting_mode: global_db::global_accounting_mode().as_str().to_string(),
        legacy_home_fallback: false,
        project_count: 0,
        session_count: 0,
        lcm_raw_message_count: 0,
        token_cache_present: false,
        registered_project_paths: Vec::new(),
        warnings,
    }
}

pub(super) async fn inspect_daemon_global_db(
    global_db: &global_db::GlobalDb,
    path_overridden: bool,
    integrity: InventoryIntegrityMode,
) -> GlobalDbInventory {
    let path = global_db.db_path();
    inventory_from_connection(
        path,
        path_overridden,
        global_db.read_connection(),
        Vec::new(),
        integrity,
    )
    .await
}

async fn inventory_from_connection(
    path: &Path,
    path_overridden: bool,
    conn: &libsql::Connection,
    mut warnings: Vec<String>,
    integrity: InventoryIntegrityMode,
) -> GlobalDbInventory {
    if should_verify_integrity(integrity) && !sqlite_quick_check_connection(conn).await {
        warnings.push(format!("global DB '{}' failed quick_check", path.display()));
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
        token_cache_present: table_exists(conn, "dashboard_token_counts").await,
        registered_project_paths: project_paths(conn).await,
        warnings,
    }
}

fn should_verify_integrity(integrity: InventoryIntegrityMode) -> bool {
    integrity == InventoryIntegrityMode::Full
}

pub(super) async fn sqlite_quick_check(path: &Path) -> bool {
    let Ok(_authority) =
        crate::db::DatabaseAuthority::for_runtime(path, "quick-check SQLite database offline")
    else {
        return false;
    };
    let Ok(db) = Builder::new_local(path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
    else {
        return false;
    };
    let Ok(conn) = db.connect() else {
        return false;
    };
    sqlite_quick_check_connection(&conn).await
}

async fn sqlite_quick_check_connection(conn: &libsql::Connection) -> bool {
    let Ok(mut rows) = conn.query("PRAGMA quick_check", ()).await else {
        return false;
    };
    let Ok(Some(row)) = rows.next().await else {
        return false;
    };
    row.get::<String>(0).is_ok_and(|value| value == "ok")
}

async fn table_count(conn: &libsql::Connection, table: &str) -> u64 {
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

async fn table_exists(conn: &libsql::Connection, table: &str) -> bool {
    let Ok(mut rows) = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            libsql::params![table],
        )
        .await
    else {
        return false;
    };
    matches!(rows.next().await, Ok(Some(_)))
}

async fn project_paths(conn: &libsql::Connection) -> Vec<PathBuf> {
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
    use super::{InventoryIntegrityMode, should_verify_integrity};

    #[test]
    fn metadata_only_inventory_skips_global_integrity_verification() {
        assert!(!should_verify_integrity(
            InventoryIntegrityMode::MetadataOnly
        ));
        assert!(should_verify_integrity(InventoryIntegrityMode::Full));
    }
}
