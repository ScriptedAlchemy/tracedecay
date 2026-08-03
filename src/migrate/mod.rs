//! Compatibility façade for the extracted migration subsystem.

pub mod consolidate {
    pub use tracedecay_migrate::consolidate::*;

    #[cfg(test)]
    use crate::branch_meta::{self, BranchEntry, BranchMeta};
    #[cfg(test)]
    use crate::global_db::{GlobalDb, StoreInstanceUpsert};
    #[cfg(test)]
    use crate::storage::{self, EnrollmentMarker, StorageMode, StoreLayout};

    /// Root-owned daemon and registry composition for the legacy public API.
    pub async fn plan(
        options: &ConsolidationOptions,
    ) -> crate::errors::Result<ConsolidationReport> {
        tracedecay_migrate::consolidate::plan_with_daemon_status(
            options,
            crate::daemon::daemon_reachable(),
        )
        .await
    }

    /// Root-owned daemon and registry composition for the legacy public API.
    pub async fn apply(
        options: &ConsolidationOptions,
        confirmation_token: &str,
    ) -> crate::errors::Result<ConsolidationReport> {
        tracedecay_migrate::consolidate::apply_with_registry(
            options,
            confirmation_token,
            crate::daemon::daemon_reachable(),
            &super::hermes::RootRegistry,
        )
        .await
    }

    /// Root-owned registry composition for the post-update health pass.
    pub async fn retire_applied_input_manifests(
        profile_root: &std::path::Path,
    ) -> ManifestRetirementReport {
        tracedecay_migrate::consolidate::retire_applied_input_manifests_with_registry(
            profile_root,
            &super::hermes::RootRegistry,
        )
        .await
    }

    #[cfg(test)]
    mod tests;
}

pub mod hermes {
    pub use tracedecay_migrate::hermes::*;

    #[cfg(test)]
    use std::fs;
    use std::path::{Path, PathBuf};

    use libsql::Connection;

    #[cfg(test)]
    use crate::db::Database;
    use crate::global_db::{
        CodeProjectRecord, GlobalDb, GraphScopeUpsert, ProjectAliasRecord, ProjectRegistryContext,
        StoreArtifactUpsert, StoreInstanceUpsert,
    };
    #[cfg(test)]
    use crate::memory::store::MemoryStore;
    use tracedecay_migrate::registry_adapter::{
        self, GraphScopeUpsert as MigrateGraphScopeUpsert,
        ProjectAliasRecord as MigrateProjectAliasRecord,
        ProjectRegistryContext as MigrateProjectRegistryContext,
        StoreArtifactUpsert as MigrateStoreArtifactUpsert,
        StoreInstanceUpsert as MigrateStoreInstanceUpsert,
    };

    pub(super) struct RootRegistry;

    pub(super) struct RootHermesStateImporter;

    impl registry_adapter::RegistryRuntime for RootRegistry {
        type Database = GlobalDb;

        async fn open_at(&self, path: &Path) -> Option<Self::Database> {
            GlobalDb::open_at(path).await
        }

        async fn open_read_only_at(&self, path: &Path) -> Option<Self::Database> {
            GlobalDb::open_read_only_at(path).await
        }
    }

    impl registry_adapter::RegistryDatabase for GlobalDb {
        fn conn(&self) -> &Connection {
            GlobalDb::conn(self)
        }

        async fn get_code_project(
            &self,
            project_id: &str,
        ) -> Option<registry_adapter::CodeProjectRecord> {
            self.get_code_project(project_id).await.map(code_project)
        }

        async fn delete_code_projects(&self, project_ids: &[String]) -> usize {
            self.delete_code_projects(project_ids).await
        }

        async fn project_registry_context_by_alias(
            &self,
            alias_path: &Path,
        ) -> Option<MigrateProjectRegistryContext> {
            self.project_registry_context_by_alias(alias_path)
                .await
                .map(project_registry_context)
        }

        async fn upsert_code_project(
            &self,
            project_id: &str,
            project_root: &Path,
            git_common_dir: Option<&Path>,
            git_remote_url: Option<&str>,
            default_branch: Option<&str>,
        ) -> Option<registry_adapter::CodeProjectRecord> {
            self.upsert_code_project(
                project_id,
                project_root,
                git_common_dir,
                git_remote_url,
                default_branch,
            )
            .await
            .map(code_project)
        }

        async fn upsert_project_alias(&self, alias_path: &Path, project_id: &str) -> bool {
            self.upsert_project_alias(alias_path, project_id)
                .await
                .is_some()
        }

        async fn upsert_store_instance(&self, upsert: MigrateStoreInstanceUpsert) -> bool {
            self.upsert_store_instance(StoreInstanceUpsert {
                store_id: upsert.store_id,
                project_id: upsert.project_id,
                store_kind: upsert.store_kind,
                storage_mode: upsert.storage_mode,
                store_relpath: upsert.store_relpath,
                manifest_relpath: upsert.manifest_relpath,
                last_verified_at: upsert.last_verified_at,
                last_write_at: upsert.last_write_at,
            })
            .await
            .is_some()
        }

        async fn upsert_graph_scope(&self, upsert: MigrateGraphScopeUpsert) -> bool {
            self.upsert_graph_scope(GraphScopeUpsert {
                graph_scope_id: upsert.graph_scope_id,
                project_id: upsert.project_id,
                store_id: upsert.store_id,
                branch_name: upsert.branch_name,
                db_relpath: upsert.db_relpath,
                parent_scope_id: upsert.parent_scope_id,
                last_synced_at: upsert.last_synced_at,
                writable: upsert.writable,
            })
            .await
            .is_some()
        }

        async fn upsert_store_artifact(&self, upsert: MigrateStoreArtifactUpsert) -> bool {
            self.upsert_store_artifact(StoreArtifactUpsert {
                store_id: upsert.store_id,
                artifact_kind: upsert.artifact_kind,
                relpath: upsert.relpath,
                size_bytes: upsert.size_bytes,
                schema_version: upsert.schema_version,
                updated_at: upsert.updated_at,
            })
            .await
            .is_some()
        }

        async fn ensure_token_count_cache(&self) -> bool {
            self.ensure_token_count_cache().await
        }

        async fn checkpoint(&self) {
            self.checkpoint().await;
        }
    }

    impl HermesStateImporter for RootHermesStateImporter {
        fn user_sessions_db_path(&self, profile_root: &Path) -> PathBuf {
            crate::sessions::user_sessions_db_path(profile_root)
        }

        async fn ingest_legacy_pinned_profile(
            &self,
            target_sessions_db_path: &Path,
            profile_dir: &Path,
            project_root: &Path,
        ) -> Result<LegacyHermesStateImport, String> {
            let db = GlobalDb::open_at(target_sessions_db_path)
                .await
                .ok_or_else(|| {
                    format!(
                        "could not open target session store '{}'",
                        target_sessions_db_path.display()
                    )
                })?;
            let stats = crate::sessions::hermes::ingest_legacy_pinned_profile(
                &db,
                profile_dir,
                project_root,
            )
            .await?;
            Ok(LegacyHermesStateImport {
                sessions_upserted: stats.sessions_upserted,
                messages_upserted: stats.messages_upserted,
            })
        }
    }

    pub async fn migrate_legacy_hermes_stores(user_home: &Path) -> LegacyHermesMigrationReport {
        let Ok(profile_root) = crate::storage::default_profile_root() else {
            return LegacyHermesMigrationReport {
                failed: vec![LegacyHermesMigrationIssue {
                    source_db: user_home.join(".hermes/.tracedecay/sessions.db"),
                    reason: "could not resolve the TraceDecay user-profile store".to_string(),
                }],
                ..LegacyHermesMigrationReport::default()
            };
        };
        migrate_legacy_hermes_stores_to(user_home, &profile_root).await
    }

    /// Root-owned compatibility seam for callers that select a temporary
    /// `TraceDecay` profile while testing a legacy Hermes migration.
    pub async fn migrate_legacy_hermes_stores_to(
        user_home: &Path,
        tracedecay_profile_root: &Path,
    ) -> LegacyHermesMigrationReport {
        tracedecay_migrate::hermes::migrate_legacy_hermes_stores_to_with_runtime(
            user_home,
            tracedecay_profile_root,
            &RootRegistry,
            &crate::agents::hermes::read_config_pinned_project_root,
            &RootHermesStateImporter,
        )
        .await
    }

    fn code_project(project: CodeProjectRecord) -> registry_adapter::CodeProjectRecord {
        registry_adapter::CodeProjectRecord {
            project_id: project.project_id,
            canonical_root: project.canonical_root,
            display_root: project.display_root,
            git_common_dir: project.git_common_dir,
            git_remote_url: project.git_remote_url,
            default_branch: project.default_branch,
            created_at: project.created_at,
            last_seen_at: project.last_seen_at,
        }
    }

    fn project_alias(alias: ProjectAliasRecord) -> MigrateProjectAliasRecord {
        MigrateProjectAliasRecord {
            alias_path: alias.alias_path,
            project_id: alias.project_id,
            last_seen_at: alias.last_seen_at,
        }
    }

    fn project_registry_context(context: ProjectRegistryContext) -> MigrateProjectRegistryContext {
        MigrateProjectRegistryContext {
            project: code_project(context.project),
            aliases: context.aliases.into_iter().map(project_alias).collect(),
        }
    }

    #[cfg(test)]
    mod tests;
}

pub mod inventory {
    pub use tracedecay_migrate::inventory::*;

    /// Root-owned global-accounting composition for the legacy public API.
    pub async fn build_inventory(
        options: MigrationInventoryOptions,
    ) -> crate::errors::Result<MigrationInventory> {
        tracedecay_migrate::inventory::build_inventory_with_global_db(
            options,
            crate::global_db::global_db_path(),
            crate::global_db::global_db_path_is_overridden(),
            crate::global_db::global_accounting_mode()
                .as_str()
                .to_string(),
        )
        .await
    }
}

pub mod manifest {
    pub use tracedecay_migrate::manifest::*;
}

pub mod registry {
    use std::path::{Path, PathBuf};

    pub use tracedecay_migrate::registry::{
        RegistryProjectPlan, RegistryReconstructionApplyReport, RegistryReconstructionDiffReport,
        RegistryReconstructionPlan, RegistryReconstructionReport, RegistryReconstructionStatus,
        StaleRootScope, apply_registry_reconstruction_report,
        apply_single_registry_reconstruction_report, diff_registry_reconstruction_report,
        reconstruct_registry_from_store_manifest, scan_profile_store_manifests,
    };

    /// Root-record adapter retained for the existing public migration API.
    pub fn code_project_root_exists(project: &crate::global_db::CodeProjectRecord) -> bool {
        Path::new(&project.canonical_root).exists() || Path::new(&project.display_root).exists()
    }

    /// Root-record adapter retained for the existing public migration API.
    pub fn stale_code_projects<'a>(
        projects: &'a [crate::global_db::CodeProjectRecord],
        prefixes: &[PathBuf],
        scope: StaleRootScope,
    ) -> Vec<&'a crate::global_db::CodeProjectRecord> {
        projects
            .iter()
            .filter(|project| {
                let canonical_root = Path::new(&project.canonical_root);
                prefixes.is_empty()
                    || prefixes
                        .iter()
                        .any(|prefix| canonical_root.starts_with(prefix))
            })
            .filter(|project| match scope {
                StaleRootScope::CanonicalRootMissing => {
                    !Path::new(&project.canonical_root).exists()
                }
                StaleRootScope::AllRootsMissing => !code_project_root_exists(project),
            })
            .collect()
    }
}
