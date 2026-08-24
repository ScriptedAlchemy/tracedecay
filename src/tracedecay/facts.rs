//! Session-memory (holographic fact store) surface of [`TraceDecay`].

use tracedecay_usecases::memory::MemoryApplication;
// The shared resolvers live in `tracedecay_usecases::memory` (the crate that
// owns `MemoryApplication`/`MemoryApplicationError`) rather than in
// `tracedecay-runtime-core` — that crate is a *dependency* of
// `tracedecay-usecases`, so hosting these there would require a circular
// crate dependency. Both this module and
// `tracedecay-dashboard-api::tracedecay::facts` delegate to the same
// functions instead of keeping independent copies.
use crate::errors::{Result, TraceDecayError};
use crate::store::memory::{ProjectFactStore, ProjectMemoryDbHandle};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_usecases::memory::memory_application_error;

use super::TraceDecay;

fn project_memory_owner_from_layout_id(project_id: Option<&str>) -> Result<FactOwnerV1> {
    let project_id = project_id.ok_or_else(|| TraceDecayError::Config {
        message: "active project has no authoritative project_id for memory".to_string(),
    })?;
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid authoritative project_id for memory: {error}"),
        })?;
    Ok(FactOwnerV1::Project { project_id })
}

impl TraceDecay {
    /// Returns the only project-memory owner accepted by core routes.
    ///
    /// The ID is supplied by the resolved store layout, never reconstructed
    /// from a filesystem path or a caller-provided display label.
    pub(crate) fn project_memory_owner(&self) -> Result<FactOwnerV1> {
        project_memory_owner_from_layout_id(self.store_layout.identity.project_id.as_deref())
    }

    /// Opens the sole project fact authority selected by the retained project
    /// layout. Code-index routing never changes this database identity.
    pub(crate) async fn project_memory_db(&self) -> Result<ProjectMemoryDbHandle<'_>> {
        if self.db_path() == self.store_layout.graph_db_path {
            Ok(ProjectMemoryDbHandle::Active(&self.db))
        } else {
            let database = if self.read_only {
                self.open_project_store_db_read_only().await?
            } else {
                self.open_project_store_db().await?
            };
            Ok(ProjectMemoryDbHandle::Owned(Box::new(database)))
        }
    }

    /// Resolves the project-memory owner and database into one owner-bound
    /// application over a fact store that owns its resolved handle. Every
    /// project-memory route builds its application through this accessor.
    pub(crate) async fn project_memory_application(
        &self,
    ) -> Result<MemoryApplication<ProjectFactStore<'_>>> {
        let owner = self.project_memory_owner()?;
        let store = self.project_memory_db().await?.into_fact_store();
        MemoryApplication::new(owner, store).map_err(memory_application_error)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn project_memory_owner_requires_a_valid_authoritative_layout_id() {
        assert!(project_memory_owner_from_layout_id(None).is_err());
        assert!(project_memory_owner_from_layout_id(Some("")).is_err());
    }
}
