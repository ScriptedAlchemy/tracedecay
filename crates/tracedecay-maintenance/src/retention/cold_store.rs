//! Bounded retention over profile-sharded stores that are not mounted.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::CancellationToken;

use super::{incident_debris, orphan_stores};

const COLD_STORE_PAGE_LIMIT: usize = 8;
const CHECKPOINT_DIRECTORY: &str = "maintenance";
const CHECKPOINT_FILE: &str = "retention-cold-store-cursor-v1.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct ColdStoreCursorV1 {
    after_project_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdStorePageOutcomeV1 {
    Processed,
    Missing,
    Unreadable,
    Cancelled,
}

impl ColdStorePageOutcomeV1 {
    #[must_use]
    pub const fn was_processed(self) -> bool {
        matches!(self, Self::Processed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColdStorePageReportV1 {
    pub processed_stores: u64,
    pub unavailable_stores: u64,
    pub reclaimed_bytes: u64,
    pub outcome: ColdStorePageOutcomeV1,
}

impl Default for ColdStorePageReportV1 {
    fn default() -> Self {
        Self {
            processed_stores: 0,
            unavailable_stores: 0,
            reclaimed_bytes: 0,
            outcome: ColdStorePageOutcomeV1::Processed,
        }
    }
}

/// Applies one bounded retention page to unmounted profile stores.
#[hotpath::measure(label = "maintenance.cold_store.page", future = true)]
pub async fn run_cold_store_page(
    profile_root: &Path,
    profile_database: &RegisteredGlobalDb,
    orphan_store_gc_days: Option<u64>,
    incident_debris_retention_days: Option<u64>,
    cancellation: &CancellationToken,
) -> tracedecay_domain::errors::Result<ColdStorePageReportV1> {
    let checkpoint_path = checkpoint_path(profile_root);
    let cursor = load_cursor(&checkpoint_path).unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    let page = orphan_stores::build_store_census_page(
        profile_database,
        profile_root,
        cursor.after_project_id.as_deref(),
        COLD_STORE_PAGE_LIMIT,
    )
    .await?;
    let retention_now =
        if orphan_store_gc_days.is_some() || incident_debris_retention_days.is_some() {
            Some(now_secs_i64().map_err(|message| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: message.to_owned(),
                }
            })?)
        } else {
            None
        };
    let mut report = ColdStorePageReportV1::default();
    for entry in &page.entries {
        let outcome = classify_cold_store_state(
            cancellation.is_cancelled(),
            entry.manifest_readable,
            entry.data_root.is_dir(),
        );
        match outcome {
            ColdStorePageOutcomeV1::Processed => {
                report.processed_stores = report.processed_stores.saturating_add(1);
            }
            ColdStorePageOutcomeV1::Cancelled => {
                report.outcome = outcome;
                return Ok(report);
            }
            ColdStorePageOutcomeV1::Missing | ColdStorePageOutcomeV1::Unreadable => {
                if report.outcome == ColdStorePageOutcomeV1::Processed {
                    report.outcome = outcome;
                }
                report.unavailable_stores = report.unavailable_stores.saturating_add(1);
            }
        }
    }
    if let Some(days) = orphan_store_gc_days {
        let findings = orphan_stores::classify_stores(
            &page.entries,
            retention_now.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "maintenance retention clock unavailable".to_owned(),
            })?,
        );
        let plan = orphan_stores::plan_collection(findings, retention_window_secs(days));
        let (outcome, _) =
            orphan_stores::execute_registered_collection(profile_database, &plan, profile_root)
                .await?;
        report.reclaimed_bytes = report
            .reclaimed_bytes
            .saturating_add(outcome.reclaimed_bytes);
        report.unavailable_stores = report
            .unavailable_stores
            .saturating_add(outcome.errors.len() as u64);
        if !outcome.errors.is_empty() {
            report.outcome = ColdStorePageOutcomeV1::Unreadable;
        }
    }
    if let Some(days) = incident_debris_retention_days {
        let sweep = incident_debris::sweep_incident_debris(
            &page.entries,
            profile_root,
            retention_window_secs(days),
            retention_now.ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "maintenance retention clock unavailable".to_owned(),
            })?,
        );
        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(sweep.reclaimed_bytes);
        report.unavailable_stores = report
            .unavailable_stores
            .saturating_add(sweep.errors.len() as u64);
        if !sweep.errors.is_empty() {
            report.outcome = ColdStorePageOutcomeV1::Unreadable;
        }
    }
    let project_ids = page
        .entries
        .iter()
        .map(|entry| entry.project_id.clone())
        .collect::<Vec<_>>();
    let next_cursor = next_cold_store_cursor(
        cursor.after_project_id.as_deref(),
        &project_ids,
        page.next_cursor.is_some(),
    )
    .unwrap_or(ColdStoreCursorV1 {
        after_project_id: None,
    });
    persist_cursor(&checkpoint_path, &next_cursor).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("persist maintenance cold-store cursor: {error}"),
        }
    })?;
    Ok(report)
}

fn next_cold_store_cursor(
    previous: Option<&str>,
    project_ids: &[String],
    has_more: bool,
) -> Option<ColdStoreCursorV1> {
    if !has_more {
        return None;
    }
    Some(ColdStoreCursorV1 {
        after_project_id: project_ids
            .last()
            .cloned()
            .or_else(|| previous.map(str::to_owned)),
    })
}

fn classify_cold_store_state(
    cancelled: bool,
    manifest_readable: bool,
    data_root_exists: bool,
) -> ColdStorePageOutcomeV1 {
    if cancelled {
        ColdStorePageOutcomeV1::Cancelled
    } else if !data_root_exists {
        ColdStorePageOutcomeV1::Missing
    } else if !manifest_readable {
        ColdStorePageOutcomeV1::Unreadable
    } else {
        ColdStorePageOutcomeV1::Processed
    }
}

fn checkpoint_path(profile_root: &Path) -> PathBuf {
    profile_root
        .join(CHECKPOINT_DIRECTORY)
        .join(CHECKPOINT_FILE)
}

#[hotpath::measure(label = "maintenance.cold_store.load_cursor")]
fn load_cursor(path: &Path) -> Option<ColdStoreCursorV1> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[hotpath::measure(label = "maintenance.cold_store.persist_cursor")]
fn persist_cursor(path: &Path, cursor: &ColdStoreCursorV1) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("maintenance cursor has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(cursor).map_err(std::io::Error::other)?;
    let mut file = std::fs::File::create(&temporary)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)
}

fn retention_window_secs(days: u64) -> i64 {
    i64::try_from(days)
        .ok()
        .and_then(|days| days.checked_mul(24 * 60 * 60))
        .unwrap_or(i64::MAX)
}

fn now_secs_i64() -> Result<i64, &'static str> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the unix epoch")?
        .as_secs();
    i64::try_from(seconds).map_err(|_| "system clock exceeds the supported retention range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_resumes_after_the_last_complete_project() {
        let first = next_cold_store_cursor(
            None,
            &["project-a".to_owned(), "project-b".to_owned()],
            true,
        )
        .expect("first page cursor");
        assert_eq!(
            first,
            ColdStoreCursorV1 {
                after_project_id: Some("project-b".to_owned()),
            }
        );
        assert_eq!(
            next_cold_store_cursor(
                first.after_project_id.as_deref(),
                &["project-c".to_owned()],
                false,
            ),
            None
        );
    }

    #[test]
    fn checkpoint_survives_restart() {
        let root = tempfile::tempdir().expect("checkpoint root");
        let path = checkpoint_path(root.path());
        let expected = ColdStoreCursorV1 {
            after_project_id: Some("project-b".to_owned()),
        };

        persist_cursor(&path, &expected).expect("persist cursor");

        assert_eq!(load_cursor(&path), Some(expected));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn state_distinguishes_missing_unreadable_and_cancelled() {
        assert_eq!(
            classify_cold_store_state(false, true, true),
            ColdStorePageOutcomeV1::Processed
        );
        assert_eq!(
            classify_cold_store_state(false, true, false),
            ColdStorePageOutcomeV1::Missing
        );
        assert_eq!(
            classify_cold_store_state(false, false, true),
            ColdStorePageOutcomeV1::Unreadable
        );
        assert_eq!(
            classify_cold_store_state(true, true, true),
            ColdStorePageOutcomeV1::Cancelled
        );
    }

    #[test]
    fn retention_window_conversion_never_wraps_negative() {
        assert_eq!(retention_window_secs(u64::MAX), i64::MAX);
    }
}
