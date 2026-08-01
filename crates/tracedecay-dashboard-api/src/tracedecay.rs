//! Dashboard-facing graph and memory runtime seams.

pub use tracedecay_code_index::is_test_file;
pub use tracedecay_usecases::tracedecay::*;

pub mod facts {
    use tracedecay_domain::FactOwnerV1;
    use tracedecay_runtime_core::db::Database;
    use tracedecay_runtime_core::errors::{Result, TraceDecayError};
    use tracedecay_runtime_core::store::memory::DatabaseFactStore;
    use tracedecay_usecases::memory::{MemoryApplication, MemoryApplicationError};

    fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
        TraceDecayError::database_operation("memory application", error)
    }

    pub fn memory_application_for_db(
        owner: FactOwnerV1,
        db: &Database,
    ) -> Result<MemoryApplication<DatabaseFactStore<'_>>> {
        MemoryApplication::new(owner, DatabaseFactStore::new(db)).map_err(memory_application_error)
    }
}
