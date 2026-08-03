use std::path::{Path, PathBuf};

use libsql::{Builder, OpenFlags};

use super::GlobalDbInventory;

pub(super) async fn inspect_global_db(
    path: &Path,
    path_overridden: bool,
    accounting_mode: String,
) -> GlobalDbInventory {
    let exists = path.is_file();
    let mut project_count = 0;
    let mut session_count = 0;
    let mut lcm_raw_message_count = 0;
    let mut token_cache_present = false;
    let mut registered_project_paths = Vec::new();
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
                        if !sqlite_quick_check(path).await {
                            warnings
                                .push(format!("global DB '{}' failed quick_check", path.display()));
                        }
                        project_count = table_count(&conn, "projects").await;
                        session_count = table_count(&conn, "sessions").await;
                        lcm_raw_message_count = table_count(&conn, "lcm_raw_messages").await;
                        token_cache_present = table_exists(&conn, "dashboard_token_counts").await;
                        registered_project_paths = project_paths(&conn).await;
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
        accounting_mode,
        legacy_home_fallback: false,
        project_count,
        session_count,
        lcm_raw_message_count,
        token_cache_present,
        registered_project_paths,
        warnings,
    }
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
