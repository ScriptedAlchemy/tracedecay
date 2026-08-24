use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;

pub mod claude;
pub mod cline_like;
pub mod codex;
pub mod codex_app_server;
pub mod cursor;
pub mod cursor_agent;
pub mod cursor_composer;
pub mod git_correlation;
pub mod hermes;
pub mod kiro;
pub mod lcm;
pub(crate) mod message_noise;
pub mod providers;
pub mod shared;
pub mod source;
pub mod transcript_backfill;
pub mod vibe;
pub mod workflow_index;
pub mod workflow_ingest;
pub mod workflow_state;

pub use providers::{ProviderScope, SessionProvider};
pub use shared::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;

pub const USER_SESSIONS_DB_FILENAME: &str = "user-sessions.db";

pub fn user_sessions_db_path(profile_root: &Path) -> PathBuf {
    profile_root.join(USER_SESSIONS_DB_FILENAME)
}

pub async fn open_user_session_db(profile_root: &Path) -> Option<GlobalDb> {
    GlobalDb::open_at(&user_sessions_db_path(profile_root)).await
}

/// All registry paths that may identify project-owned transcript evidence.
pub async fn registered_project_roots() -> Vec<PathBuf> {
    try_registered_project_roots().await.unwrap_or_default()
}

pub async fn try_registered_project_roots() -> Option<Vec<PathBuf>> {
    let global = GlobalDb::open().await?;
    registered_project_roots_from(&global).await
}

async fn try_registered_project_roots_at(profile_root: &Path) -> Option<Vec<PathBuf>> {
    let global = GlobalDb::open_at(&profile_root.join("global.db")).await?;
    registered_project_roots_from(&global).await
}

pub(crate) async fn registered_project_roots_from(global: &GlobalDb) -> Option<Vec<PathBuf>> {
    let mut roots = global
        .list_project_paths()
        .await
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    for project in global.list_code_projects(usize::MAX).await {
        roots.push(PathBuf::from(project.canonical_root));
        roots.push(PathBuf::from(project.display_root));
    }
    roots.extend(
        global
            .list_project_alias_paths()
            .await
            .into_iter()
            .map(PathBuf::from),
    );
    roots.sort();
    roots.dedup();
    Some(roots)
}

pub async fn ingest_user_codex_sessions(session_id: Option<String>) -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(registered_roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_codex_sessions_at(&profile_root, session_id, registered_roots).await
}

pub(crate) async fn ingest_user_codex_sessions_at(
    profile_root: &Path,
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let Some(db) = open_user_session_db(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    tracedecay_sessions::runtime::ingest::ingest_user_codex_sessions(
        &db,
        profile_root,
        session_id,
        registered_roots,
    )
    .await
}

pub async fn ingest_user_cursor_sessions() -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(registered_roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_cursor_sessions_at(&profile_root, registered_roots).await
}

async fn ingest_user_cursor_sessions_at(
    profile_root: &Path,
    registered_roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let Some(db) = open_user_session_db(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    tracedecay_sessions::runtime::ingest::ingest_user_cursor_sessions(
        &db,
        profile_root,
        registered_roots,
    )
    .await
}

pub async fn ingest_user_global_sources() -> TranscriptIngestStats {
    ingest_user_global_sources_for_provider(None).await
}

pub async fn ingest_user_global_sources_for_provider(
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(roots) = try_registered_project_roots().await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_global_sources_for_provider_with_roots(&profile_root, provider, roots).await
}

pub(crate) async fn ingest_user_global_sources_for_provider_at(
    profile_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let Some(roots) = try_registered_project_roots_at(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    ingest_user_global_sources_for_provider_with_roots(profile_root, provider, roots).await
}

async fn ingest_user_global_sources_for_provider_with_roots(
    profile_root: &Path,
    provider: Option<SessionProvider>,
    roots: Vec<PathBuf>,
) -> TranscriptIngestStats {
    let Some(db) = open_user_session_db(profile_root).await else {
        return TranscriptIngestStats::default();
    };
    let stats = tracedecay_sessions::runtime::ingest::ingest_user_sources_for_provider(
        &db,
        profile_root,
        provider,
        roots,
    )
    .await;
    if stats.messages_upserted > 0 {
        crate::hooks::schedule_user_session_review(
            provider.map_or("all", SessionProvider::id),
            None,
        );
    }
    stats
}

pub(crate) async fn ingest_user_global_sources_for_startup() -> TranscriptIngestStats {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return TranscriptIngestStats::default();
    };
    let Some(mut guard) =
        tracedecay_sessions::runtime::ingest::StartupUserIngestGuard::claim(profile_root)
    else {
        return TranscriptIngestStats::default();
    };
    let stats = ingest_user_global_sources().await;
    guard.complete();
    stats
}

pub async fn ingest_global_sources(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    ingest_global_sources_for_provider(db, project_root, None).await
}

pub async fn ingest_global_sources_for_provider(
    db: &GlobalDb,
    project_root: &Path,
    provider: Option<SessionProvider>,
) -> TranscriptIngestStats {
    let _ = ingest_user_global_sources_for_provider(provider).await;
    ingest_project_sources_for_provider(db, project_root, provider, true).await
}

pub(crate) async fn ingest_project_sources_for_provider(
    db: &GlobalDb,
    project_root: &Path,
    provider: Option<SessionProvider>,
    include_hermes: bool,
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::ingest::ingest_project_sources_for_provider(
        db,
        project_root,
        provider,
        include_hermes,
    )
    .await
}

pub(crate) async fn finalize_project_ingest(db: &GlobalDb, project_root: &Path) {
    tracedecay_sessions::runtime::ingest::finalize_project_ingest(db, project_root).await;
}

pub(crate) async fn ingest_global_sources_for_startup(
    db: &GlobalDb,
    project_root: &Path,
) -> TranscriptIngestStats {
    let user = ingest_user_global_sources_for_startup().await;
    user.merge(
        tracedecay_sessions::runtime::ingest::ingest_project_sources_for_provider(
            db,
            project_root,
            None,
            true,
        )
        .await,
    )
}

impl tracedecay_sessions::runtime::ingest::SessionIngestStore for GlobalDb {
    fn session_connection(&self) -> &libsql::Connection {
        self.conn()
    }

    async fn ingest_hermes_for_project(&self, project_root: &Path) -> TranscriptIngestStats {
        hermes::ingest_for_project(self, project_root).await
    }

    async fn ingest_hermes_for_user(&self, registered_roots: &[PathBuf]) -> TranscriptIngestStats {
        hermes::ingest_user_sessions(self, registered_roots).await
    }
}

pub use tracedecay_sessions::{
    SessionMessageRecord, SessionMessageSearchResult, SessionMessageType, SessionRecord,
    SessionSearchFilters, SessionSearchScope, SessionSearchTimeRange,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registered_project_roots_include_modern_registry_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().join("repo");
        let worktree = temp.path().join("repo-worktree");
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let canonical = std::fs::canonicalize(canonical).unwrap();
        let worktree = std::fs::canonicalize(worktree).unwrap();
        let db = GlobalDb::open_at(&temp.path().join("global.db"))
            .await
            .unwrap();
        db.upsert_code_project("project-1", &canonical, None, None, None)
            .await
            .unwrap();
        db.upsert_project_alias(&worktree, "project-1")
            .await
            .unwrap();

        let roots = registered_project_roots_from(&db).await.unwrap();

        assert!(roots.contains(&canonical));
        assert!(roots.contains(&worktree));
    }
}
