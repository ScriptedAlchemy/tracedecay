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

use super::super::TraceDecay;
use super::file_authority::{SourceEditFileAuthority, read_source_edit_candidate};

impl TraceDecay {
    /// Restore every retained preimage for a caller-requested rollback of an
    /// already-completed source edit. Unlike crash recovery this is a live
    /// operation, so the graph is resynchronized wholesale rather than
    /// reindexed file by file: a rollback may delete a file the edit created,
    /// and a deleted path has no bytes left to reindex.
    pub(crate) async fn apply_source_edit_rollback(
        &self,
        files: &[PlannedSourceEditFile],
    ) -> Result<()> {
        rollback_planned_source_edit_files(&self.project_root, files)?;
        self.sync().await?;
        Ok(())
    }

    pub(crate) async fn recover_source_edit_preimages(
        &self,
        files: &[PlannedSourceEditFile],
    ) -> Result<()> {
        rollback_planned_source_edit_files(&self.project_root, files)?;
        // A missing preimage means the edit created the file, so recovery
        // removed it: there is nothing to reindex and the graph must be
        // resynchronized instead of silently skipping the candidate.
        if files.iter().any(|file| file.expected.is_none()) {
            self.sync().await?;
            return Ok(());
        }
        for file in files {
            let expected = file
                .expected
                .as_ref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "source edit recovery lost a required preimage".to_owned(),
                })?;
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&file.relative_path))?;
            self.reindex_file(&file.relative_path, expected, &authority)
                .await?;
        }
        self.sync().await?;
        Ok(())
    }

    /// Reconcile the graph after a completed source edit whose durable journal
    /// did not advance before restart. Source bytes are already final; this
    /// operation only republishes their indexed representation.
    pub(crate) async fn commit_source_edit_postimages(
        &self,
        files: &[PlannedSourceEditFile],
    ) -> Result<()> {
        // A missing postimage means the edit removed the file; the same
        // resynchronization argument as the preimage path applies.
        if files.iter().any(|file| file.intended.is_none()) {
            self.sync().await?;
            return Ok(());
        }
        for file in files {
            let intended = file
                .intended
                .as_ref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "source edit recovery lost a required postimage".to_owned(),
                })?;
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&file.relative_path))?;
            self.reindex_file(&file.relative_path, intended, &authority)
                .await?;
        }
        self.sync().await?;
        Ok(())
    }
}

pub(in crate::tracedecay) fn rollback_planned_source_edit_files(
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

pub(in crate::tracedecay) fn publish_planned_source_edit(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: &str,
) -> Result<()> {
    publish_planned_source_edit_state(project_root, relative_path, expected, Some(intended))
}

pub(super) fn publish_planned_source_edit_state(
    project_root: &Path,
    relative_path: &str,
    expected: Option<&str>,
    intended: Option<&str>,
) -> Result<()> {
    validate_planned_source_edit(relative_path, expected, intended)?;
    publish_source_edit_state(project_root, relative_path, expected, intended)
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
    use tracedecay_usecases::tracedecay::{PlannedSourceEditFile, capture_source_edit_plan};

    use super::{
        capture_planned_source_edit, publish_planned_source_edit,
        rollback_planned_source_edit_files,
    };

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
