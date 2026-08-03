use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;
use crate::storage::{
    SESSIONS_DB_FILENAME, default_profile_project_id, default_profile_root,
    profile_sharded_data_root, resolve_layout_for_current_profile,
};

pub use tracedecay_sessions::runtime::cursor::{
    CursorSweepSource, CursorTranscriptIngestStats, TimestampCarry, cursor_project_slug,
};

const PROJECT_SESSION_DB_FILENAME: &str = SESSIONS_DB_FILENAME;

pub fn project_session_db_path(project_root: &Path) -> PathBuf {
    resolve_layout_for_current_profile(project_root).map_or_else(
        |_| {
            let profile_root = default_profile_root()
                .unwrap_or_else(|_| PathBuf::from(crate::config::TRACEDECAY_DIR));
            profile_sharded_data_root(&profile_root, &default_profile_project_id(project_root))
                .join(PROJECT_SESSION_DB_FILENAME)
        },
        |layout| layout.sessions_db_path,
    )
}

pub async fn open_project_session_db(project_root: &Path) -> Option<GlobalDb> {
    let db_path = resolved_project_session_db_path(project_root).await?;
    GlobalDb::open_at(&db_path).await
}

pub async fn resolved_project_session_db_path(project_root: &Path) -> Option<PathBuf> {
    match crate::storage::read_enrollment_marker(project_root) {
        Ok(Some(_)) => {
            return resolve_layout_for_current_profile(project_root)
                .ok()
                .map(|layout| layout.sessions_db_path);
        }
        Ok(None) => {}
        Err(_) => return None,
    }
    if let Some(db_path) = registry_profile_session_db_path(project_root).await {
        return Some(db_path);
    }
    resolve_layout_for_current_profile(project_root)
        .ok()
        .map(|layout| layout.sessions_db_path)
}

async fn registry_profile_session_db_path(project_root: &Path) -> Option<PathBuf> {
    let profile_root = crate::storage::default_profile_root().ok()?;
    let global = GlobalDb::open().await?;
    let git_common_dir = (!crate::worktree::is_detached_linked_worktree(project_root))
        .then(|| crate::worktree::git_common_dir(project_root))
        .flatten();
    let resolution = if let Some(resolution) = global
        .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
        .await
    {
        resolution
    } else {
        let remote = crate::tracedecay::git_remote_url(project_root)?;
        let resolution = global
            .resolve_unique_project_store_by_git_remote(&remote)
            .await?;
        if registered_checkout_present(&resolution.project) {
            return None;
        }
        resolution
    };
    (resolution.store.storage_mode == "profile_sharded").then(|| {
        profile_root
            .join(resolution.store.store_relpath)
            .join(PROJECT_SESSION_DB_FILENAME)
    })
}

fn registered_checkout_present(project: &crate::global_db::CodeProjectRecord) -> bool {
    [
        Some(project.canonical_root.as_str()),
        Some(project.display_root.as_str()),
        project.git_common_dir.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|root| !root.is_empty())
    .any(|root| Path::new(root).exists())
}

pub async fn ingest_cursor_transcript_event(
    event_json: &str,
    db: &GlobalDb,
) -> CursorTranscriptIngestStats {
    tracedecay_sessions::runtime::cursor::ingest_cursor_transcript_event(event_json, db).await
}

pub async fn ingest_cursor_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    tracedecay_sessions::runtime::cursor::ingest_cursor_transcript_event_capped(
        event_json,
        db,
        max_new_bytes,
    )
    .await
}

pub async fn ingest_cursor_user_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    tracedecay_sessions::runtime::cursor::ingest_cursor_user_transcript_event_capped(
        event_json,
        db,
        max_new_bytes,
    )
    .await
}

pub async fn ingest_cursor_user_transcript_event_capped_with_registered_roots(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
    registered_roots: &[PathBuf],
) -> CursorTranscriptIngestStats {
    tracedecay_sessions::runtime::cursor::ingest_cursor_user_transcript_event_capped_with_registered_roots(
        event_json,
        db,
        max_new_bytes,
        registered_roots,
    )
    .await
}
