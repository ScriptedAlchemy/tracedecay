//! The single source-edit publish/rollback authority: every primitive in
//! this module tree funnels its atomic file write through
//! [`publish_planned_source_edit`], and every crash-recovery path restores
//! preimages through [`rollback_planned_source_edit_files`]. Both consult the
//! plan captured by `tracedecay-usecases` so a preview and its later apply
//! (or rollback) are always looking at the same recorded expectation.

use std::path::Path;

/// The preview/apply plan authority is owned by `tracedecay-usecases`; the
/// root source-edit primitives consult that single set of task-locals so a
/// preview captured by the use case is the same plan the apply validates.
pub(in crate::tracedecay) use tracedecay_usecases::tracedecay::{
    PlannedSourceEditFile, capture_planned_source_edit, validate_planned_source_edit,
};

use crate::errors::{Result, TraceDecayError};

use super::file_authority::{SourceEditFileAuthority, read_source_edit_candidate};

pub(super) fn rollback_planned_source_edit_files(
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
        let (Some(current), Some(expected)) = (file.intended.as_deref(), file.expected.as_deref())
        else {
            return Err(TraceDecayError::Config {
                message: format!(
                    "source edit crash recovery cannot restore a created or removed file: {}",
                    file.relative_path
                ),
            });
        };
        publish_planned_source_edit(project_root, &file.relative_path, Some(current), expected)?;
    }
    Ok(())
}

pub(in crate::tracedecay) fn publish_planned_source_edit(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: &str,
) -> Result<()> {
    validate_planned_source_edit(relative_path, expected, Some(intended))?;
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
    use tempfile::tempdir;
    use tracedecay_usecases::tracedecay::capture_source_edit_plan;

    use super::{capture_planned_source_edit, publish_planned_source_edit};

    /// The root primitives must feed the single plan authority owned by
    /// `tracedecay-usecases`; capturing through `super` and reading back
    /// through the use-case scope proves there is no second set of statics.
    #[tokio::test]
    async fn source_edit_plan_capture_retains_exact_pre_and_post_bytes() {
        let ((), files) = capture_source_edit_plan(async {
            capture_planned_source_edit("src/lib.rs", Some("before\n"), Some("after\n"));
        })
        .await;

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "src/lib.rs");
        assert_eq!(files[0].expected.as_deref(), Some("before\n"));
        assert_eq!(files[0].intended.as_deref(), Some("after\n"));
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
