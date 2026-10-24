//! Exact forward publication plans captured by preview and consumed by apply.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::file_authority::SourceEditFileAuthority;

/// A preview retains only the expected content digest and intended publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedSourceEditFile {
    pub relative_path: String,
    pub expected_digest: Option<[u8; 32]>,
    pub intended: String,
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

#[tracing::instrument(name = "usecases.edit.plan", level = "trace", skip_all)]
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

#[tracing::instrument(name = "usecases.edit.apply", level = "trace", skip_all)]
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
    intended: &str,
) -> bool {
    SOURCE_EDIT_PLAN_CAPTURE
        .try_with(|capture| {
            capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(PlannedSourceEditFile {
                    relative_path: relative_path.to_owned(),
                    expected_digest: expected
                        .map(|content| Sha256::digest(content.as_bytes()).into()),
                    intended: intended.to_owned(),
                });
        })
        .is_ok()
}

/// Admits one publication against the plan an apply is running under.
/// Outside an apply scope every publication is admitted.
#[tracing::instrument(name = "usecases.edit.validate", level = "trace", skip_all)]
pub fn validate_planned_source_edit(
    relative_path: &str,
    expected: Option<&str>,
    intended: &str,
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
            if planned.expected_digest
                != expected.map(|content| Sha256::digest(content.as_bytes()).into())
                || planned.intended != intended
            {
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

/// Publish one candidate's postimage, or record it into the active preview
/// plan when a plan capture is in scope.
#[tracing::instrument(name = "edits.publish_planned", level = "trace", skip_all)]
pub fn publish_planned_source_edit(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: &str,
) -> Result<()> {
    if capture_planned_source_edit(relative_path, expected, intended) {
        return Ok(());
    }
    validate_planned_source_edit(relative_path, expected, intended)?;

    let file = SourceEditFileAuthority::open(project_root, Path::new(relative_path))?;
    let expected_identity = file.current_identity()?;
    file.publish(
        relative_path,
        expected,
        expected_identity.as_ref(),
        intended,
        || {},
    )
}

#[cfg(test)]
mod tests {
    use sha2::Digest;
    use tempfile::tempdir;

    use super::{PlannedSourceEditFile, capture_source_edit_plan, publish_planned_source_edit};

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
                expected_digest: Some(sha2::Sha256::digest(b"before\n").into()),
                intended: "after\n".to_owned(),
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
}
