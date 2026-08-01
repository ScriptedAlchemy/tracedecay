//! API-migration plan apply/rollback: publishes an entire immutable
//! file-family plan through the source-edit CAS authority, validating every
//! preimage before the first write and restoring every published file if
//! cancellation or a later publication fails.

use std::collections::BTreeSet;
use std::path::Path;

use crate::errors::{Result, TraceDecayError};

use super::super::TraceDecay;
use super::file_authority::{SourceEditFileAuthority, read_source_edit_candidate};
use super::plan::{
    capture_planned_source_edit, publish_planned_source_edit, rollback_planned_source_edit_files,
    validate_planned_source_edit,
};
use tracedecay_usecases::tracedecay::PlannedSourceEditFile;

impl TraceDecay {
    pub(crate) async fn recover_source_edit_preimages(
        &self,
        files: &[PlannedSourceEditFile],
    ) -> Result<()> {
        rollback_planned_source_edit_files(&self.project_root, files)?;
        for file in files {
            let Some(expected) = &file.expected else {
                continue;
            };
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&file.relative_path))?;
            self.reindex_file(&file.relative_path, expected, &authority)
                .await?;
        }
        Ok(())
    }

    /// Roll a completed-but-unfinalized source edit forward: the intended bytes
    /// are already published on disk, so this only reconciles the graph index to
    /// the post-edit content. A crash between the atomic publish and the reindex
    /// can leave the index pointing at the preimage; reindexing is idempotent, so
    /// running it whether or not the original apply reached it is safe. This
    /// never writes source bytes and so can never revert a written edit.
    pub(crate) async fn commit_source_edit_postimages(
        &self,
        files: &[PlannedSourceEditFile],
    ) -> Result<()> {
        for file in files {
            let Some(intended) = &file.intended else {
                continue;
            };
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&file.relative_path))?;
            self.reindex_file(&file.relative_path, intended, &authority)
                .await?;
        }
        Ok(())
    }

    /// Applies one immutable API-migration file family through the source-edit
    /// CAS authority. Every candidate is captured during preview. Real apply
    /// validates every preimage before the first write and restores all
    /// published files if cancellation or a later publication fails.
    pub(crate) async fn apply_api_migration_plan(
        &self,
        plan: &tracedecay_application::ApiMigrationPlanV1,
        dry_run: bool,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<tracedecay_application::ApiMigrationApplyResultV1> {
        plan.validate().map_err(|error| TraceDecayError::Config {
            message: format!("invalid API migration plan: {error}"),
        })?;
        if plan.blocked {
            return Ok(tracedecay_application::ApiMigrationApplyResultV1 {
                success: false,
                dry_run,
                family_id: plan.family_id.clone(),
                plan_digest: plan.plan_digest.clone(),
                changed_files: Vec::new(),
                changed_sites: 0,
                compatibility_sites: 0,
                protected_values_verified: 0,
                rolled_back: false,
                message: "API migration plan contains blocked sites".to_owned(),
            });
        }
        let current_revision = {
            let repository =
                gix::open(&self.project_root).map_err(|error| TraceDecayError::Config {
                    message: format!("cannot revalidate API migration repository: {error}"),
                })?;
            repository
                .head_commit()
                .map(|commit| commit.id().to_hex().to_string())
                .map_err(|error| TraceDecayError::Config {
                    message: format!("cannot revalidate API migration HEAD: {error}"),
                })?
        };
        if current_revision != plan.repository_revision {
            return Err(TraceDecayError::Config {
                message: "API migration repository revision is stale; replan before apply"
                    .to_owned(),
            });
        }

        for candidate in &plan.files {
            let observed =
                read_source_edit_candidate(&self.project_root, Path::new(&candidate.path))?
                    .ok_or_else(|| TraceDecayError::Config {
                        message: format!("API migration candidate disappeared: {}", candidate.path),
                    })?;
            if observed != candidate.expected_content.as_bytes() {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "API migration candidate {} is stale; replan before apply",
                        candidate.path
                    ),
                });
            }
            capture_planned_source_edit(
                &candidate.path,
                Some(&candidate.expected_content),
                Some(&candidate.intended_content),
            );
        }

        let changed_files = plan
            .files
            .iter()
            .filter(|file| file.expected_content != file.intended_content)
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let changed_sites = plan
            .sites
            .iter()
            .filter(|site| {
                site.disposition == tracedecay_application::ApiMigrationSiteDispositionV1::Changed
            })
            .count();
        let compatibility_operations = plan
            .operations
            .iter()
            .filter_map(|operation| match operation {
                tracedecay_application::ApiMigrationOperationRequestV1::InsertCompatibility {
                    operation_id,
                    ..
                } => Some(operation_id.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let compatibility_sites = plan
            .sites
            .iter()
            .filter(|site| compatibility_operations.contains(site.operation_id.as_str()))
            .count();
        let protected_operations = plan
            .operations
            .iter()
            .filter_map(|operation| match operation {
                tracedecay_application::ApiMigrationOperationRequestV1::AssertStableValue {
                    operation_id,
                    ..
                } => Some(operation_id.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let protected_values_verified = plan
            .sites
            .iter()
            .filter(|site| protected_operations.contains(site.operation_id.as_str()))
            .count();
        if dry_run {
            return Ok(tracedecay_application::ApiMigrationApplyResultV1 {
                success: true,
                dry_run: true,
                family_id: plan.family_id.clone(),
                plan_digest: plan.plan_digest.clone(),
                changed_files,
                changed_sites,
                compatibility_sites,
                protected_values_verified,
                rolled_back: false,
                message: "API migration dry-run revalidated the immutable plan; no files changed"
                    .to_owned(),
            });
        }

        let mut published = Vec::<&tracedecay_application::ApiMigrationFilePlanV1>::new();
        for candidate in &plan.files {
            if is_cancelled() {
                rollback_api_migration_files(&self.project_root, &published)?;
                return Ok(tracedecay_application::ApiMigrationApplyResultV1 {
                    success: false,
                    dry_run: false,
                    family_id: plan.family_id.clone(),
                    plan_digest: plan.plan_digest.clone(),
                    changed_files: Vec::new(),
                    changed_sites: 0,
                    compatibility_sites,
                    protected_values_verified,
                    rolled_back: true,
                    message: "API migration cancelled; every published file was restored"
                        .to_owned(),
                });
            }
            if candidate.expected_content == candidate.intended_content {
                validate_planned_source_edit(
                    &candidate.path,
                    Some(&candidate.expected_content),
                    Some(&candidate.intended_content),
                )?;
                continue;
            }
            if let Err(error) = publish_planned_source_edit(
                &self.project_root,
                &candidate.path,
                Some(&candidate.expected_content),
                &candidate.intended_content,
            ) {
                rollback_api_migration_files(&self.project_root, &published)?;
                return Err(TraceDecayError::Config {
                    message: format!(
                        "API migration publication failed and prior files were restored: {error}"
                    ),
                });
            }
            published.push(candidate);
        }

        for candidate in &published {
            let file =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&candidate.path))?;
            if let Err(error) = self
                .reindex_file(&candidate.path, &candidate.intended_content, &file)
                .await
            {
                rollback_api_migration_files(&self.project_root, &published)?;
                for restored in &published {
                    if let Ok(file) =
                        SourceEditFileAuthority::open(&self.project_root, Path::new(&restored.path))
                    {
                        let _ = self
                            .reindex_file(&restored.path, &restored.expected_content, &file)
                            .await;
                    }
                }
                return Err(TraceDecayError::Config {
                    message: format!(
                        "API migration graph refresh failed and workspace bytes were restored: {error}"
                    ),
                });
            }
        }
        Ok(tracedecay_application::ApiMigrationApplyResultV1 {
            success: true,
            dry_run: false,
            family_id: plan.family_id.clone(),
            plan_digest: plan.plan_digest.clone(),
            changed_files,
            changed_sites,
            compatibility_sites,
            protected_values_verified,
            rolled_back: false,
            message: "API migration applied atomically and refreshed graph evidence".to_owned(),
        })
    }

    pub(crate) async fn rollback_api_migration_plan(
        &self,
        plan: &tracedecay_application::ApiMigrationPlanV1,
    ) -> Result<()> {
        let published = plan
            .files
            .iter()
            .filter(|file| file.expected_content != file.intended_content)
            .collect::<Vec<_>>();
        rollback_api_migration_files(&self.project_root, &published)?;
        for restored in published {
            let file =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&restored.path))?;
            self.reindex_file(&restored.path, &restored.expected_content, &file)
                .await?;
        }
        Ok(())
    }
}

fn rollback_api_migration_files(
    project_root: &Path,
    published: &[&tracedecay_application::ApiMigrationFilePlanV1],
) -> Result<()> {
    for candidate in published.iter().rev() {
        let file = SourceEditFileAuthority::open(project_root, Path::new(&candidate.path))?;
        let (_, current_identity) = file.read_optional_with_identity()?;
        file.publish(
            &candidate.path,
            Some(&candidate.intended_content),
            current_identity.as_ref(),
            &candidate.expected_content,
            || {},
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::super::file_authority::SourceEditFileAuthority;
    use super::rollback_api_migration_files;

    #[test]
    fn api_migration_rollback_restores_every_published_preimage() {
        let root = tempdir().expect("temporary project");
        std::fs::create_dir_all(root.path().join("src")).expect("create source directory");
        let candidates = [
            ("src/a.rs", "pub fn old_a() {}\n", "pub fn new_a() {}\n"),
            ("src/b.rs", "pub fn old_b() {}\n", "pub fn new_b() {}\n"),
        ]
        .into_iter()
        .map(|(path, expected, intended)| {
            std::fs::write(root.path().join(path), expected).expect("seed source");
            tracedecay_application::ApiMigrationFilePlanV1 {
                path: path.to_owned(),
                expected_digest: tracedecay_application::api_migration_file_digest(expected)
                    .expect("expected digest"),
                predicted_digest: tracedecay_application::api_migration_file_digest(intended)
                    .expect("predicted digest"),
                expected_content: expected.to_owned(),
                intended_content: intended.to_owned(),
            }
        })
        .collect::<Vec<_>>();

        for candidate in &candidates {
            let file = SourceEditFileAuthority::open(root.path(), Path::new(&candidate.path))
                .expect("open source authority");
            let identity = file.current_identity().expect("read identity");
            file.publish(
                &candidate.path,
                Some(&candidate.expected_content),
                identity.as_ref(),
                &candidate.intended_content,
                || {},
            )
            .expect("publish candidate");
        }

        let published = candidates.iter().collect::<Vec<_>>();
        rollback_api_migration_files(root.path(), &published).expect("rollback migration");
        for candidate in &candidates {
            assert_eq!(
                std::fs::read_to_string(root.path().join(&candidate.path)).expect("read restored"),
                candidate.expected_content
            );
        }
    }
}
