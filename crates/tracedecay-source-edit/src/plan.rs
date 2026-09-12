//! The single source-edit plan authority.
//!
//! A preview runs the edit primitive inside [`capture_source_edit_plan`],
//! which intercepts every publication as a [`PlannedSourceEditFile`] instead
//! of writing it. The later apply runs the same primitive inside
//! [`apply_source_edit_plan`], which admits a write only when it matches the
//! captured plan byte for byte. Every edit primitive funnels its atomic file
//! write through [`publish_planned_source_edit`], and every crash-recovery
//! path restores preimages through [`rollback_planned_source_edit_files`], so
//! a preview and its later apply (or rollback) are always looking at the same
//! recorded expectation.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::file_authority::{SourceEditFileAuthority, read_source_edit_candidate};

/// One candidate file's exact preimage and postimage as recorded by a preview.
/// Serialized verbatim into the durable journal and rollback records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedSourceEditFile {
    pub relative_path: String,
    pub expected: Option<String>,
    pub intended: Option<String>,
}

#[derive(Debug)]
struct SourceEditApplyState {
    files: Vec<PlannedSourceEditFile>,
    consumed: BTreeSet<String>,
}

tokio::task_local! {
    static SOURCE_EDIT_PLAN_CAPTURE: Arc<Mutex<Vec<PlannedSourceEditFile>>>;
    static SOURCE_EDIT_APPLY_STATE: Arc<Mutex<SourceEditApplyState>>;
}

#[hotpath::measure(label = "usecases.edit.plan", future = true)]
pub(crate) async fn capture_source_edit_plan<T>(
    future: impl Future<Output = T>,
) -> (T, Vec<PlannedSourceEditFile>) {
    let capture = Arc::new(Mutex::new(Vec::new()));
    let result = SOURCE_EDIT_PLAN_CAPTURE
        .scope(Arc::clone(&capture), future)
        .await;
    let files = capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (result, files)
}

#[hotpath::measure(label = "usecases.edit.apply", future = true)]
pub(crate) async fn apply_source_edit_plan<T>(
    files: Vec<PlannedSourceEditFile>,
    future: impl Future<Output = T>,
) -> (T, bool) {
    let expected_count = files.len();
    let state = Arc::new(Mutex::new(SourceEditApplyState {
        files,
        consumed: BTreeSet::new(),
    }));
    let result = SOURCE_EDIT_APPLY_STATE
        .scope(Arc::clone(&state), future)
        .await;
    let complete = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .consumed
        .len()
        == expected_count;
    (result, complete)
}

/// Records one planned publication when a preview capture is in scope.
/// Returns `false` outside a capture, in which case the caller must publish.
pub fn capture_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) -> bool {
    SOURCE_EDIT_PLAN_CAPTURE
        .try_with(|capture| {
            capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(PlannedSourceEditFile {
                    relative_path: relative_path.to_owned(),
                    expected: expected.map(str::to_owned),
                    intended: intended.map(str::to_owned),
                });
        })
        .is_ok()
}

/// Admits one publication against the plan an apply is running under.
/// Outside an apply scope every publication is admitted.
#[hotpath::measure(label = "usecases.edit.validate")]
pub fn validate_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) -> Result<()> {
    SOURCE_EDIT_APPLY_STATE
        .try_with(|state| {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(planned) = state
                .files
                .iter()
                .find(|file| file.relative_path == relative_path)
            else {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "source edit apply produced unplanned candidate {relative_path}"
                    ),
                });
            };
            if planned.expected.as_deref() != expected || planned.intended.as_deref() != intended {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "source edit candidate {relative_path} drifted from its exact preview"
                    ),
                });
            }
            state.consumed.insert(relative_path.to_owned());
            Ok(())
        })
        .unwrap_or(Ok(()))
}

/// Restore every retained preimage. Refuses foreign bytes outright: a file is
/// only touched when it can be proven to hold either the preimage or the
/// intended edit, so unaccountable content fails recovery instead of being
/// erased.
#[hotpath::measure(label = "edits.rollback_planned_files")]
pub fn rollback_planned_source_edit_files(
    project_root: &Path,
    files: &[PlannedSourceEditFile],
) -> Result<()> {
    let observed = files
        .iter()
        .map(|file| {
            let current = read_source_edit_candidate(project_root, Path::new(&file.relative_path))?;
            let expected = file.expected.as_deref().map(str::as_bytes);
            let intended = file.intended.as_deref().map(str::as_bytes);
            if current.as_deref() != expected && current.as_deref() != intended {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "source edit crash recovery refused foreign bytes in {}",
                        file.relative_path
                    ),
                });
            }
            Ok(current)
        })
        .collect::<Result<Vec<_>>>()?;
    for (file, current) in files.iter().zip(observed).rev() {
        if current.as_deref() == file.expected.as_deref().map(str::as_bytes) {
            continue;
        }
        publish_source_edit_state(
            project_root,
            &file.relative_path,
            file.intended.as_deref(),
            file.expected.as_deref(),
        )?;
    }
    Ok(())
}

/// Confirm that a completed source edit still has every exact postimage.
///
/// Code-index generations are immutable and refreshed by the daemon-owned
/// scheduler. Crash reconciliation therefore verifies the transaction's byte
/// authority here instead of mutating a graph store.
#[hotpath::measure(label = "edits.commit_postimages")]
pub(crate) fn commit_source_edit_postimages(
    project_root: &Path,
    files: &[PlannedSourceEditFile],
) -> Result<()> {
    for file in files {
        let current = read_source_edit_candidate(project_root, Path::new(&file.relative_path))?;
        if current.as_deref() != file.intended.as_deref().map(str::as_bytes) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "source edit postimage changed before reconciliation in {}",
                    file.relative_path
                ),
            });
        }
    }
    Ok(())
}

/// Publish one candidate's postimage, or record it into the active preview
/// plan when a plan capture is in scope.
#[hotpath::measure(label = "edits.publish_planned")]
pub fn publish_planned_source_edit(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: &str,
) -> Result<()> {
    if capture_planned_source_edit(relative_path, expected, Some(intended)) {
        return Ok(());
    }
    validate_planned_source_edit(relative_path, expected, Some(intended))?;
    publish_source_edit_state(project_root, relative_path, expected, Some(intended))
}

fn publish_source_edit_state(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) -> Result<()> {
    let file = SourceEditFileAuthority::open(project_root, Path::new(relative_path))?;
    let expected_identity = file.current_identity()?;
    match intended {
        Some(intended) => file.publish(
            relative_path,
            expected,
            expected_identity.as_ref(),
            intended,
            || {},
        ),
        None => {
            let expected = expected.ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "source edit candidate {relative_path} cannot remove an absent file"
                ),
            })?;
            let expected_identity = expected_identity.as_ref().ok_or_else(|| {
                TraceDecayError::Config {
                    message: format!(
                        "source edit candidate {relative_path} disappeared before atomic removal"
                    ),
                }
            })?;
            file.remove(relative_path, expected, expected_identity)
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        PlannedSourceEditFile, capture_source_edit_plan, publish_planned_source_edit,
        rollback_planned_source_edit_files,
    };

    #[tokio::test]
    async fn source_edit_plan_capture_intercepts_apply_publication() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        std::fs::write(&path, "before\n").unwrap();

        let (result, files) = capture_source_edit_plan(async {
            publish_planned_source_edit(directory.path(), "lib.rs", Some("before\n"), "after\n")
        })
        .await;

        result.unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "before\n");
        assert_eq!(
            files,
            vec![PlannedSourceEditFile {
                relative_path: "lib.rs".to_owned(),
                expected: Some("before\n".to_owned()),
                intended: Some("after\n".to_owned()),
            }]
        );
    }

    #[test]
    fn atomic_publication_rejects_content_drift() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        std::fs::write(&path, "changed\n").unwrap();

        assert!(
            publish_planned_source_edit(
                directory.path(),
                "lib.rs",
                Some("previewed\n"),
                "intended\n"
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "changed\n");
    }

    #[test]
    fn rollback_removes_a_file_created_by_the_edit() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("created.rs");
        std::fs::write(&path, "created\n").unwrap();
        let files = vec![PlannedSourceEditFile {
            relative_path: "created.rs".to_owned(),
            expected: None,
            intended: Some("created\n".to_owned()),
        }];

        rollback_planned_source_edit_files(directory.path(), &files).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn rollback_recreates_a_file_removed_by_the_edit() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("removed.rs");
        let files = vec![PlannedSourceEditFile {
            relative_path: "removed.rs".to_owned(),
            expected: Some("original\n".to_owned()),
            intended: None,
        }];

        rollback_planned_source_edit_files(directory.path(), &files).unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "original\n");
    }

    #[test]
    fn rollback_refuses_foreign_bytes_in_a_created_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("created.rs");
        std::fs::write(&path, "foreign\n").unwrap();
        let files = vec![PlannedSourceEditFile {
            relative_path: "created.rs".to_owned(),
            expected: None,
            intended: Some("created\n".to_owned()),
        }];

        assert!(rollback_planned_source_edit_files(directory.path(), &files).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "foreign\n");
    }
}
