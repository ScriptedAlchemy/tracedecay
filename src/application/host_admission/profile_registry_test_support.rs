use super::*;
use sha2::Sha256;

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostAdmissionDatabaseIdentityV1([u8; 32]);

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn checkpoint_profile_database_for_test(&self) {
        self.profile_database.checkpoint().await;
    }

    #[doc(hidden)]
    pub async fn snapshot_profile_database_for_test(&self, destination: &Path) -> Result<()> {
        self.profile_database.snapshot_to(destination).await
    }

    #[doc(hidden)]
    pub async fn snapshot_session_database_for_test(
        &self,
        scope: HostAdmissionScope,
        destination: &Path,
    ) -> Result<()> {
        self.session_database_for_test(scope)?
            .snapshot_to(destination)
            .await
    }

    #[doc(hidden)]
    pub fn session_database_identity_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<HostAdmissionDatabaseIdentityV1> {
        let database = self.session_database_for_test(scope)?;
        let digest: [u8; 32] =
            Sha256::digest(database.db_path().as_os_str().as_encoded_bytes()).into();
        Ok(HostAdmissionDatabaseIdentityV1(digest))
    }

    #[doc(hidden)]
    pub async fn validate_profile_registry_schema_contract_for_test(&self) -> Result<()> {
        self.profile_database
            .validate_registry_schema_contract_for_test()
            .await
    }

    #[doc(hidden)]
    pub fn profile_relative_path_for_test(&self, path: &Path) -> Result<PathBuf> {
        let profile_root =
            self.profile_database
                .db_path()
                .parent()
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "resolve test profile-relative path".to_owned(),
                    message: "profile database has no parent directory".to_owned(),
                })?;
        path.strip_prefix(profile_root)
            .map(Path::to_path_buf)
            .map_err(|error| TraceDecayError::Database {
                operation: "resolve test profile-relative path".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn delete_code_projects(&self, project_ids: &[String]) -> usize {
        self.profile_database
            .delete_code_projects(project_ids)
            .await
    }

    #[doc(hidden)]
    pub async fn delete_project(&self, project_path: &Path) {
        self.profile_database.delete_project(project_path).await;
    }

    #[doc(hidden)]
    pub async fn delete_projects(&self, project_paths: &[String]) -> usize {
        self.profile_database.delete_projects(project_paths).await
    }

    #[doc(hidden)]
    pub async fn get_code_project(
        &self,
        project_id: &str,
    ) -> Option<crate::global_db::CodeProjectRecord> {
        self.profile_database.get_code_project(project_id).await
    }

    #[doc(hidden)]
    pub async fn list_code_projects(
        &self,
        limit: usize,
    ) -> Vec<crate::global_db::CodeProjectRecord> {
        self.profile_database
            .list_code_projects(limit)
            .await
            .unwrap_or_default()
    }

    #[doc(hidden)]
    pub async fn list_project_paths_compat(&self) -> Vec<String> {
        self.profile_database.list_project_paths_compat().await
    }

    #[doc(hidden)]
    pub async fn registered_project_paths_for_test(&self) -> Result<Vec<PathBuf>> {
        self.profile_database
            .try_list_code_project_paths(usize::MAX)
            .await
    }

    #[doc(hidden)]
    pub async fn search_code_projects(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<crate::global_db::CodeProjectRecord> {
        self.profile_database
            .search_code_projects(query, limit)
            .await
    }

    #[doc(hidden)]
    pub async fn project_registry_context_by_alias(
        &self,
        alias_path: &Path,
    ) -> Result<Option<crate::global_db::ProjectRegistryContext>> {
        self.profile_database
            .project_registry_context_by_alias(alias_path)
            .await
    }

    #[doc(hidden)]
    pub async fn project_registry_context_by_id(
        &self,
        project_id: &str,
    ) -> Option<crate::global_db::ProjectRegistryContext> {
        self.profile_database
            .project_registry_context_by_id(project_id)
            .await
            .ok()
            .flatten()
    }

    #[doc(hidden)]
    pub async fn project_registry_context_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Result<Option<crate::global_db::ProjectRegistryContext>> {
        self.profile_database
            .project_registry_context_by_identity(project_root, git_common_dir)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_project_store_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<crate::global_db::ProjectStoreResolution> {
        self.profile_database
            .resolve_project_store_by_alias(alias_path)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_project_store_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Result<Option<crate::global_db::ProjectStoreResolution>> {
        self.profile_database
            .resolve_project_store_by_identity(project_root, git_common_dir)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_unique_project_store_by_git_remote(
        &self,
        git_remote_url: &str,
    ) -> Option<crate::global_db::ProjectStoreResolution> {
        self.profile_database
            .resolve_unique_project_store_by_git_remote(git_remote_url)
            .await
    }

    #[doc(hidden)]
    pub async fn resolve_project_observation_store(
        &self,
        project_root: &Path,
    ) -> std::result::Result<
        crate::global_db::ProjectObservationStoreResolution,
        crate::global_db::ProjectObservationStoreError,
    > {
        self.profile_database
            .resolve_project_observation_store(project_root)
            .await
    }

    #[doc(hidden)]
    pub async fn plan_registry_reap(&self) -> Result<crate::project_registry::RegistryReapPlan> {
        self.profile_database
            .plan_registry_reap()
            .await
            .map(legacy_registry_reap_plan)
    }

    #[doc(hidden)]
    pub async fn apply_registry_reap(
        &self,
        plan: &crate::project_registry::RegistryReapPlan,
    ) -> Result<usize> {
        let plan = registered_registry_reap_plan(plan);
        self.profile_database.apply_registry_reap(&plan).await
    }

    #[doc(hidden)]
    pub async fn apply_registry_reconstruction_report(
        &self,
        report: &crate::migrate::registry::RegistryReconstructionReport,
    ) -> std::result::Result<crate::migrate::registry::RegistryReconstructionApplyReport, Vec<String>>
    {
        crate::migrate::registry::apply_registry_reconstruction_report(
            self.profile_database.as_ref(),
            report,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn apply_single_registry_reconstruction_report(
        &self,
        report: &crate::migrate::registry::RegistryReconstructionReport,
    ) -> std::result::Result<crate::migrate::registry::RegistryReconstructionApplyReport, Vec<String>>
    {
        crate::migrate::registry::apply_single_registry_reconstruction_report(
            self.profile_database.as_ref(),
            report,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn upsert_graph_scope(
        &self,
        upsert: crate::global_db::GraphScopeUpsert,
    ) -> Option<crate::global_db::GraphScopeRecord> {
        self.profile_database.upsert_graph_scope(upsert).await
    }

    #[doc(hidden)]
    pub async fn upsert_store_artifact(
        &self,
        upsert: crate::global_db::StoreArtifactUpsert,
    ) -> Option<crate::global_db::StoreArtifactRecord> {
        self.profile_database.upsert_store_artifact(upsert).await
    }
}

fn legacy_registry_reap_plan(
    plan: tracedecay_global_db::RegistryReapPlan,
) -> crate::project_registry::RegistryReapPlan {
    crate::project_registry::RegistryReapPlan {
        reapable: plan
            .reapable
            .into_iter()
            .map(legacy_registry_reap_entry)
            .collect(),
        retained: plan
            .retained
            .into_iter()
            .map(|retained| crate::project_registry::RetainedRegistryEntry {
                entry: legacy_registry_reap_entry(retained.entry),
                reason: retained.reason,
            })
            .collect(),
    }
}

fn legacy_registry_reap_entry(
    entry: tracedecay_global_db::RegistryReapEntry,
) -> crate::project_registry::RegistryReapEntry {
    crate::project_registry::RegistryReapEntry {
        kind: match entry.kind {
            tracedecay_global_db::ReapEntryKind::SavingsLedgerPath => {
                crate::project_registry::ReapEntryKind::SavingsLedgerPath
            }
            tracedecay_global_db::ReapEntryKind::ProjectAlias => {
                crate::project_registry::ReapEntryKind::ProjectAlias
            }
            tracedecay_global_db::ReapEntryKind::CodeProject => {
                crate::project_registry::ReapEntryKind::CodeProject
            }
        },
        key: entry.key,
        missing_path: entry.missing_path,
        project_id: entry.project_id,
    }
}

fn registered_registry_reap_plan(
    plan: &crate::project_registry::RegistryReapPlan,
) -> tracedecay_global_db::RegistryReapPlan {
    tracedecay_global_db::RegistryReapPlan {
        reapable: plan
            .reapable
            .iter()
            .map(registered_registry_reap_entry)
            .collect(),
        retained: plan
            .retained
            .iter()
            .map(|retained| tracedecay_global_db::RetainedRegistryEntry {
                entry: registered_registry_reap_entry(&retained.entry),
                reason: retained.reason.clone(),
            })
            .collect(),
    }
}

fn registered_registry_reap_entry(
    entry: &crate::project_registry::RegistryReapEntry,
) -> tracedecay_global_db::RegistryReapEntry {
    tracedecay_global_db::RegistryReapEntry {
        kind: match entry.kind {
            crate::project_registry::ReapEntryKind::SavingsLedgerPath => {
                tracedecay_global_db::ReapEntryKind::SavingsLedgerPath
            }
            crate::project_registry::ReapEntryKind::ProjectAlias => {
                tracedecay_global_db::ReapEntryKind::ProjectAlias
            }
            crate::project_registry::ReapEntryKind::CodeProject => {
                tracedecay_global_db::ReapEntryKind::CodeProject
            }
        },
        key: entry.key.clone(),
        missing_path: entry.missing_path.clone(),
        project_id: entry.project_id.clone(),
    }
}
