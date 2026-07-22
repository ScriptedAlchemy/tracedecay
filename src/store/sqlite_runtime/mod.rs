//! Borrowed [`GlobalDb`] compatibility ports for the storage-runtime cutover.
//!
//! Call sites still land behind these facades during S8, so unused helpers are
//! retained until every live open routes through the registry adapters.

#![allow(dead_code)]

use std::path::Path;

use crate::global_db::{GlobalDb, GlobalDbSessionTemporalExecution, ProjectRegistryContext};
use crate::store::{GlobalDbSessionTemporalStore, GlobalDbTranscriptStore};

/// Borrowed facade for the established `GlobalDb` compatibility ports.
pub(crate) struct GlobalDbRuntime<'db> {
    db: &'db GlobalDb,
}

/// Typed registry-identity reads over an already-open profile database.
///
/// This adapter deliberately retains only the borrowed `GlobalDb` handle;
/// `GlobalDb` remains responsible for connection and transaction lifecycle.
pub(crate) struct GlobalDbProjectRegistry<'db> {
    db: &'db GlobalDb,
}

impl<'db> GlobalDbProjectRegistry<'db> {
    const fn new(db: &'db GlobalDb) -> Self {
        Self { db }
    }

    pub(crate) async fn project_registry_context_by_id(
        &self,
        project_id: &str,
    ) -> Option<ProjectRegistryContext> {
        self.db.project_registry_context_by_id(project_id).await
    }

    pub(crate) async fn project_registry_context_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<ProjectRegistryContext> {
        self.db.project_registry_context_by_alias(alias_path).await
    }

    pub(crate) async fn project_registry_context_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Option<ProjectRegistryContext> {
        self.db
            .project_registry_context_by_identity(project_root, git_common_dir)
            .await
    }
}

impl<'db> GlobalDbRuntime<'db> {
    pub(crate) const fn new(db: &'db GlobalDb) -> Self {
        Self { db }
    }

    pub(crate) const fn project_registry(&self) -> GlobalDbProjectRegistry<'db> {
        GlobalDbProjectRegistry::new(self.db)
    }

    pub(crate) const fn transcript_store(&self) -> GlobalDbTranscriptStore<'db> {
        GlobalDbTranscriptStore::new(self.db)
    }

    pub(crate) const fn session_store(&self) -> GlobalDbSessionTemporalStore<'db> {
        GlobalDbSessionTemporalStore::new(self.db)
    }

    pub(crate) const fn session_execution(&self) -> GlobalDbSessionTemporalExecution<'db> {
        GlobalDbSessionTemporalExecution::new(self.db)
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, path::Path};

    use tracedecay_store::{
        SessionRefreshStore, SessionTemporalCapabilityProvider, SessionTemporalProjectionStore,
        TranscriptStore,
    };

    use crate::application::session::SessionTemporalExecutionPort;
    use crate::global_db::ProjectRegistryContext;

    use super::*;

    #[test]
    fn facade_exposes_only_typed_ports() {
        fn assert_transcript_port<T: TranscriptStore>(_: &T) {}
        fn assert_session_port<
            T: SessionTemporalCapabilityProvider
                + SessionTemporalProjectionStore
                + SessionRefreshStore,
        >(
            _: &T,
        ) {
        }
        fn assert_execution_port<T: SessionTemporalExecutionPort>(_: &T) {}
        fn assert_registry_lookup<F>(_: F)
        where
            F: Future<Output = Option<ProjectRegistryContext>>,
        {
        }

        fn assert_facade(db: &GlobalDb) {
            let facade = GlobalDbRuntime::new(db);
            let registry = facade.project_registry();
            assert_registry_lookup(registry.project_registry_context_by_id("project-id"));
            assert_registry_lookup(
                registry.project_registry_context_by_alias(Path::new("/project-alias")),
            );
            assert_registry_lookup(
                registry.project_registry_context_by_identity(Path::new("/project"), None),
            );
            assert_transcript_port(&facade.transcript_store());
            assert_session_port(&facade.session_store());
            assert_execution_port(&facade.session_execution());
        }

        let _ = assert_facade;
    }

    #[test]
    fn facade_adapters_only_borrow_global_db() {
        fn assert_facade_fields(runtime: &GlobalDbRuntime<'_>) {
            let GlobalDbRuntime { db: _ } = runtime;
        }
        fn assert_registry_fields(registry: &GlobalDbProjectRegistry<'_>) {
            let GlobalDbProjectRegistry { db: _ } = registry;
        }

        let _ = assert_facade_fields;
        let _ = assert_registry_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbRuntime<'static>>(),
            std::mem::size_of::<&'static GlobalDb>()
        );
        assert_eq!(
            std::mem::size_of::<GlobalDbProjectRegistry<'static>>(),
            std::mem::size_of::<&'static GlobalDb>()
        );
    }

    #[tokio::test]
    async fn project_registry_adapter_delegates_identity_lookups() {
        let temporary = tempfile::TempDir::new().expect("create temporary registry root");
        let project_root = temporary.path().join("project");
        std::fs::create_dir_all(&project_root).expect("create project root");
        let db = GlobalDb::open_at(&temporary.path().join("global.db"))
            .await
            .expect("open temporary global database");
        db.upsert_code_project("project-id", &project_root, None, None, None)
            .await
            .expect("register project");

        let registry = GlobalDbRuntime::new(&db).project_registry();
        let by_id = registry
            .project_registry_context_by_id("project-id")
            .await
            .expect("resolve registered project by id");
        let by_alias = registry
            .project_registry_context_by_alias(&project_root)
            .await
            .expect("resolve registered project by alias");
        let by_identity = registry
            .project_registry_context_by_identity(&project_root, None)
            .await
            .expect("resolve registered project by identity");

        assert_eq!(by_id.project.project_id, "project-id");
        assert_eq!(by_alias.project.project_id, by_id.project.project_id);
        assert_eq!(by_identity.project.project_id, by_id.project.project_id);
    }
}
