//! Read-only, cheap-to-query storage observability (plan 38 §7): per-store
//! size and free-page ratio for every registered profile-sharded store under
//! a profile root, plus an unregistered-directory backlog summary —
//! reachable from `tracedecay migrate storage-report` without a live daemon
//! or any [`crate::global_db::RegisteredGlobalDb`] writer authority.
//!
//! # Why this does not use `sqlite_read_snapshot`
//!
//! The registry read does, because it is one small file and needs a
//! consistent view of `code_projects`. The *per-store size sampling*
//! deliberately does not. [`crate::sqlite_read_snapshot`] freezes a database
//! family by reflinking it, falling back to a **full byte copy** when the
//! filesystem cannot reflink, and any graph database a live daemon has open
//! is WAL-backed and therefore takes that path. Running this report over the
//! profile it was written for — the owner's, at 91GB — would have copied
//! every registered store to read three pragmas off it, on a command whose
//! entire promise is being cheap enough to run on a full profile.
//!
//! So sizes come from filesystem metadata (exact, no locks, no I/O beyond
//! `stat`) and free-page counts from a short read-only connection. If that
//! connection fails — busy, corrupt, or a WAL database with no `-shm` to map
//! read-only — the free-page fields are reported as `None` rather than
//! guessed at, and the store still reports its size. Nothing here opens a
//! writable handle or mutates a byte.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

const GLOBAL_DB_FILENAME: &str = "global.db";

/// How long a size sample will wait on a lock before giving up and reporting
/// the store's free-page fields as unsampled. A live daemon writing to the
/// store must never be delayed by a report.
const SAMPLE_BUSY_TIMEOUT: Duration = Duration::from_millis(200);

/// One registered profile-sharded store's size/free-page snapshot.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StoreSizeReportEntry {
    pub project_id: String,
    pub canonical_root: String,
    /// On-disk bytes of the graph database family (main file plus `-wal` and
    /// `-shm`), from filesystem metadata.
    pub total_bytes: u64,
    /// Reclaimable free-page bytes, or `None` when the store could not be
    /// sampled without waiting on a live writer.
    pub free_bytes: Option<u64>,
    /// Free pages as a fraction of total pages, or `None` when unsampled.
    pub free_page_ratio: Option<f64>,
}

/// The full report: per-store sizes plus an unregistered-directory backlog
/// summary (plan 38 §2's disjoint on-disk-only audit class, sized here rather
/// than classified — the daemon sweep and `sweep_unregistered_stores` own
/// collection).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StorageReport {
    pub profile_root: String,
    pub stores: Vec<StoreSizeReportEntry>,
    pub unregistered_dir_count: usize,
    pub unregistered_bytes: u64,
    pub global_db_bytes: u64,
}

/// Builds the report by reading `global.db`'s `code_projects` table and every
/// registered project's graph database, then scanning `projects/` bottom-up
/// for directories with no matching registry row. Read-only throughout.
pub async fn build_storage_report(profile_root: &Path) -> crate::errors::Result<StorageReport> {
    let scratch_root = profile_root.join("scratch").join("sqlite-read");
    let global_db_path = profile_root.join(GLOBAL_DB_FILENAME);
    let mut registered_ids = HashSet::new();
    let mut stores = Vec::new();

    if global_db_path.exists() {
        // The snapshot layer creates only the final scratch component, so its
        // parent must already exist -- otherwise every report against a
        // profile root without a `scratch/` directory (including any
        // `--profile-root` the owner points at) fails on NotFound.
        std::fs::create_dir_all(&scratch_root)
            .map_err(|error| report_error("prepare read-snapshot scratch", error))?;
        let snapshot = crate::sqlite_read_snapshot::open_in(&global_db_path, &scratch_root)
            .await
            .map_err(|error| report_error("open global.db read snapshot", error))?;
        let connection = snapshot.connection();
        let mut rows = connection
            .query(
                "SELECT project_id, canonical_root FROM code_projects ORDER BY project_id",
                (),
            )
            .await
            .map_err(|error| report_error("list registered projects", error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| report_error("read registered project row", error))?
        {
            let project_id: String = row
                .get::<String>(0)
                .map_err(|error| report_error("decode project id", error))?;
            let canonical_root: String = row
                .get::<String>(1)
                .map_err(|error| report_error("decode canonical root", error))?;
            registered_ids.insert(project_id.clone());
            let graph_db_path = profile_root
                .join("projects")
                .join(&project_id)
                .join(crate::config::DB_FILENAME);
            if let Some(entry) = sample_store_size(&graph_db_path) {
                stores.push(StoreSizeReportEntry {
                    project_id,
                    canonical_root,
                    total_bytes: entry.total_bytes,
                    free_bytes: entry.free_bytes,
                    free_page_ratio: entry.free_page_ratio,
                });
            }
        }
    }

    let (unregistered_dir_count, unregistered_bytes) =
        scan_unregistered_dirs(profile_root, &registered_ids);
    let global_db_bytes = std::fs::metadata(&global_db_path).map_or(0, |metadata| metadata.len());

    Ok(StorageReport {
        profile_root: profile_root.display().to_string(),
        stores,
        unregistered_dir_count,
        unregistered_bytes,
        global_db_bytes,
    })
}

/// A store's sampled size. `total_bytes` is always available (filesystem
/// metadata); the free-page fields are only present when the database could
/// be read without waiting on a live writer.
struct StoreSizeSample {
    total_bytes: u64,
    free_bytes: Option<u64>,
    free_page_ratio: Option<f64>,
}

/// `None` only when the store has no graph database file at all.
fn sample_store_size(graph_db_path: &Path) -> Option<StoreSizeSample> {
    if !graph_db_path.is_file() {
        return None;
    }
    // Filesystem metadata over the whole family: what the owner actually sees
    // consumed on disk, including a WAL that has not been checkpointed.
    let mut total_bytes = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        let member = if suffix.is_empty() {
            graph_db_path.to_path_buf()
        } else {
            let mut name = graph_db_path.as_os_str().to_os_string();
            name.push(suffix);
            std::path::PathBuf::from(name)
        };
        if let Ok(metadata) = std::fs::metadata(&member) {
            total_bytes = total_bytes.saturating_add(metadata.len());
        }
    }

    let (free_bytes, free_page_ratio) = match sample_free_pages(graph_db_path) {
        Some((free_bytes, ratio)) => (Some(free_bytes), Some(ratio)),
        None => (None, None),
    };
    Some(StoreSizeSample {
        total_bytes,
        free_bytes,
        free_page_ratio,
    })
}

/// Reads the free-page pragmas over a strictly read-only connection with a
/// short busy timeout. Returns `None` on any failure — a store held by a busy
/// writer, a corrupt file, or a WAL database whose `-shm` cannot be mapped
/// read-only. A report must degrade, never block and never repair.
fn sample_free_pages(graph_db_path: &Path) -> Option<(u64, f64)> {
    let connection = rusqlite::Connection::open_with_flags(
        graph_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    connection.busy_timeout(SAMPLE_BUSY_TIMEOUT).ok()?;
    let page_size = pragma_u64(&connection, "page_size")?;
    let page_count = pragma_u64(&connection, "page_count")?;
    let freelist = pragma_u64(&connection, "freelist_count")?;
    if page_size == 0 || page_count == 0 {
        return None;
    }
    let free_bytes = page_size.saturating_mul(freelist);
    #[allow(clippy::cast_precision_loss)]
    let free_page_ratio = freelist as f64 / page_count as f64;
    Some((free_bytes, free_page_ratio))
}

fn pragma_u64(connection: &rusqlite::Connection, pragma: &str) -> Option<u64> {
    connection
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
        .ok()
        .map(|value: i64| value.max(0) as u64)
}

fn scan_unregistered_dirs(profile_root: &Path, registered_ids: &HashSet<String>) -> (usize, u64) {
    let projects_dir = profile_root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects_dir) else {
        return (0, 0);
    };
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if crate::storage::validate_project_id(&name).is_err() || registered_ids.contains(&name) {
            continue;
        }
        count += 1;
        // Shares the orphan sweep's walker so the bytes this report shows for
        // a backlog directory are the same bytes that sweep would reclaim.
        bytes = bytes.saturating_add(super::orphan_stores::dir_size_bytes(&entry.path()));
    }
    (count, bytes)
}

fn report_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::engine::TestConnection;

    async fn seed_global_db(profile_root: &Path, projects: &[(&str, &str)]) {
        let conn = TestConnection::open(&profile_root.join(GLOBAL_DB_FILENAME));
        conn.execute_batch(
            "CREATE TABLE code_projects (
                project_id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL
             );",
        )
        .await
        .unwrap();
        for (project_id, canonical_root) in projects {
            conn.execute(
                "INSERT INTO code_projects (project_id, canonical_root) VALUES (?1, ?2)",
                crate::db::engine::params![*project_id, *canonical_root],
            )
            .await
            .unwrap();
        }
    }

    fn seed_graph_db(profile_root: &Path, project_id: &str) {
        let data_root = profile_root.join("projects").join(project_id);
        std::fs::create_dir_all(&data_root).unwrap();
        let connection =
            rusqlite::Connection::open(data_root.join(crate::config::DB_FILENAME)).unwrap();
        connection
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY);")
            .unwrap();
    }

    #[tokio::test]
    async fn report_sizes_every_registered_store_and_counts_unregistered_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        seed_graph_db(&profile_root, "proj_a");

        // An unregistered leaf directory under `projects/`.
        let ghost = profile_root.join("projects").join("proj_ghost");
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(ghost.join("payload.bin"), vec![0u8; 2048]).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(report.stores[0].project_id, "proj_a");
        assert_eq!(report.stores[0].canonical_root, "/repos/a");
        assert!(report.stores[0].total_bytes > 0);
        assert_eq!(report.unregistered_dir_count, 1);
        assert!(report.unregistered_bytes >= 2048);
    }

    /// The report must never freeze a store family to size it: a snapshot
    /// reflinks or, when the filesystem cannot, fully copies the database.
    /// Sizing must come from metadata and leave no scratch behind.
    #[tokio::test]
    async fn sizing_a_store_copies_nothing_and_leaves_no_scratch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;
        seed_graph_db(&profile_root, "proj_a");

        let graph_db = profile_root
            .join("projects")
            .join("proj_a")
            .join(crate::config::DB_FILENAME);
        let before = std::fs::metadata(&graph_db).unwrap().modified().unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(
            report.stores[0].total_bytes,
            std::fs::metadata(&graph_db).unwrap().len(),
            "size must come from filesystem metadata"
        );
        assert!(
            report.stores[0].free_page_ratio.is_some(),
            "an idle store must sample its free pages"
        );
        assert_eq!(
            std::fs::metadata(&graph_db).unwrap().modified().unwrap(),
            before,
            "sizing must not touch the source database"
        );
        assert!(
            !profile_root
                .join("projects")
                .join("proj_a")
                .join("scratch")
                .exists(),
            "sizing must not write scratch into the store"
        );
    }

    /// A registered store whose graph database is unreadable still reports its
    /// on-disk size; only the free-page fields degrade to `None`. Reporting a
    /// zero ratio there would read as "no bloat".
    #[tokio::test]
    async fn an_unreadable_store_reports_size_with_unsampled_free_pages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[("proj_a", "/repos/a")]).await;

        let data_root = profile_root.join("projects").join("proj_a");
        std::fs::create_dir_all(&data_root).unwrap();
        // Not a SQLite database at all.
        std::fs::write(data_root.join(crate::config::DB_FILENAME), vec![9u8; 4096]).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.stores.len(), 1);
        assert_eq!(report.stores[0].total_bytes, 4096);
        assert_eq!(report.stores[0].free_bytes, None);
        assert_eq!(report.stores[0].free_page_ratio, None);
    }

    /// The backlog walker must not follow symlinks: one pointing at an
    /// ancestor would recurse until the stack ran out, and one pointing
    /// outside would bill another tree's bytes to the backlog.
    #[cfg(unix)]
    #[tokio::test]
    async fn unregistered_sizing_does_not_follow_symlinks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        seed_global_db(&profile_root, &[]).await;

        let ghost = profile_root.join("projects").join("proj_ghost");
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(ghost.join("payload.bin"), vec![0u8; 2048]).unwrap();
        // A loop back to the directory being walked.
        std::os::unix::fs::symlink(&ghost, ghost.join("loop")).unwrap();
        // And an escape hatch pointing at a large tree outside the profile.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("big.bin"), vec![0u8; 65536]).unwrap();
        std::os::unix::fs::symlink(&outside, ghost.join("escape")).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert_eq!(report.unregistered_dir_count, 1);
        assert_eq!(
            report.unregistered_bytes, 2048,
            "only the store's own bytes count"
        );
    }

    #[tokio::test]
    async fn report_on_empty_profile_root_is_empty_not_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();

        let report = build_storage_report(&profile_root).await.unwrap();

        assert!(report.stores.is_empty());
        assert_eq!(report.unregistered_dir_count, 0);
        assert_eq!(report.global_db_bytes, 0);
    }
}
