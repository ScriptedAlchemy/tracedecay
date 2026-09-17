//! Session-memory (holographic fact store) surface of [`TraceDecay`].

use tracedecay_application::tracedecay::project_memory_owner_from_layout_id;
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::errors::Result;
use tracedecay_session_memory::fact_store::{ProjectFactStore, ProjectMemoryDbHandle};
use tracedecay_session_memory::memory::MemoryApplication;
use tracedecay_session_memory::memory::memory_application_error;

use super::TraceDecay;

impl TraceDecay {
    /// Returns the only project-memory owner accepted by core routes.
    pub fn project_memory_owner(&self) -> Result<FactOwnerV1> {
        project_memory_owner_from_layout_id(self.store_layout.identity.project_id.as_deref())
    }

    /// Opens the sole project fact authority selected by the retained project
    /// layout. Code-index routing never changes this database identity.
    #[hotpath::skip]
    pub fn project_memory_db(&self) -> Result<ProjectMemoryDbHandle<'_>> {
        if tracedecay_runtime_core::path_safety::same_canonical_path(
            &self.db_path(),
            &self.store_layout.graph_db_path,
        ) {
            Ok(ProjectMemoryDbHandle::Active(&self.db))
        } else {
            let database = if self.read_only {
                self.open_project_store_db_read_only()?
            } else {
                self.open_project_store_db()?
            };
            Ok(ProjectMemoryDbHandle::Owned(Box::new(database)))
        }
    }

    /// Resolves the project-memory owner and database into one owner-bound
    /// application over a fact store that owns its resolved handle. Every
    /// project-memory route builds its application through this accessor.
    #[hotpath::skip]
    pub fn project_memory_application(&self) -> Result<MemoryApplication<ProjectFactStore<'_>>> {
        let owner = self.project_memory_owner()?;
        let store = self.project_memory_db()?.into_fact_store();
        MemoryApplication::new(owner, store).map_err(memory_application_error)
    }
}
