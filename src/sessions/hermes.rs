use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracedecay_sessions::SessionRecord;
use tracedecay_sessions::runtime::shared::StoredCursor;
use tracedecay_sessions::runtime::shared::TranscriptIngestStats;

pub use tracedecay_sessions::runtime::hermes::{
    HermesStore, ProjectIngestDestination, TranscriptBatch,
};

fn profile_project_pin(profile_dir: &Path) -> Option<PathBuf> {
    crate::agents::hermes::read_config_pinned_project_root(&profile_dir.join("config.yaml"))
        .map(PathBuf::from)
}

fn hermes_homes() -> Vec<PathBuf> {
    crate::agents::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default()
}

pub async fn ingest_for_project(
    db: &dyn HermesStore,
    project_root: &Path,
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::hermes::ingest_homes_with_project_pins(
        db,
        &hermes_homes(),
        project_root,
        profile_project_pin,
    )
    .await
}

pub async fn ingest_for_projects(
    destinations: &[ProjectIngestDestination<'_>],
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::hermes::ingest_homes_for_projects_with_project_pins(
        &hermes_homes(),
        destinations,
        profile_project_pin,
    )
    .await
}

pub async fn ingest_homes_for_projects(
    hermes_homes: &[PathBuf],
    destinations: &[ProjectIngestDestination<'_>],
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::hermes::ingest_homes_for_projects_with_project_pins(
        hermes_homes,
        destinations,
        profile_project_pin,
    )
    .await
}

pub async fn ingest_homes(
    db: &dyn HermesStore,
    hermes_homes: &[PathBuf],
    project_root: &Path,
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::hermes::ingest_homes_with_project_pins(
        db,
        hermes_homes,
        project_root,
        profile_project_pin,
    )
    .await
}

pub async fn ingest_user_sessions(
    db: &dyn HermesStore,
    registered_roots: &[PathBuf],
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::hermes::ingest_user_homes_with_project_pins(
        db,
        &hermes_homes(),
        registered_roots,
        profile_project_pin,
    )
    .await
}

pub async fn ingest_user_homes(
    db: &dyn HermesStore,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
) -> TranscriptIngestStats {
    tracedecay_sessions::runtime::hermes::ingest_user_homes_with_project_pins(
        db,
        hermes_homes,
        registered_roots,
        profile_project_pin,
    )
    .await
}

pub async fn ingest_legacy_pinned_profile(
    db: &dyn HermesStore,
    profile_dir: &Path,
    project_root: &Path,
) -> Result<TranscriptIngestStats, String> {
    tracedecay_sessions::runtime::hermes::ingest_legacy_pinned_profile_with_project_pin(
        db,
        profile_dir,
        project_root,
        profile_project_pin(profile_dir),
    )
    .await
}

impl HermesStore for crate::global_db::GlobalDb {
    fn load_cursor<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = StoredCursor> + Send + 'a>> {
        Box::pin(async move {
            let offset = self.get_parse_offset(path).await.unwrap_or_default();
            StoredCursor {
                position: offset.byte_offset,
                mtime: offset.mtime,
                file_id: offset.file_id,
            }
        })
    }

    fn advance_cursor<'a>(
        &'a self,
        path: &'a str,
        cursor: StoredCursor,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.set_parse_offset(
                path,
                crate::global_db::ParseOffset {
                    byte_offset: cursor.position,
                    mtime: cursor.mtime,
                    file_id: cursor.file_id,
                },
            )
            .await;
        })
    }

    fn upsert_transcript_projection_batches<'a>(
        &'a self,
        batches: &'a [TranscriptBatch],
        path: &'a str,
        cursor: StoredCursor,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            self.upsert_transcript_projection_batches(
                batches,
                path,
                crate::global_db::ParseOffset {
                    byte_offset: cursor.position,
                    mtime: cursor.mtime,
                    file_id: cursor.file_id,
                },
            )
            .await
        })
    }

    fn existing_session<'a>(
        &'a self,
        provider: &'a str,
        session_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<SessionRecord>> + Send + 'a>> {
        Box::pin(async move { self.get_session(provider, session_id).await })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::profile_project_pin;

    fn pin_from_config(config: &str) -> Option<std::path::PathBuf> {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::fs::write(profile.join("config.yaml"), config).unwrap();
        profile_project_pin(&profile)
    }

    #[test]
    fn profile_pin_ignores_sibling_plugin_settings() {
        assert_eq!(
            pin_from_config(
                "plugins:\n  sibling:\n    project_root: /wrong\n  tracedecay:\n    project_root: /right\n",
            ),
            Some(std::path::PathBuf::from("/right")),
        );
    }

    #[test]
    fn profile_pin_ignores_tracedecay_outside_plugins() {
        assert_eq!(
            pin_from_config(
                "tracedecay:\n  project_root: /wrong\nplugins:\n  enabled:\n    - tracedecay\n"
            ),
            None,
        );
    }

    #[test]
    fn profile_pin_decodes_quoted_yaml_scalar() {
        assert_eq!(
            pin_from_config("plugins:\n  tracedecay:\n    project_root: '/repo/it''s-ok'\n"),
            Some(std::path::PathBuf::from("/repo/it's-ok")),
        );
    }
}
