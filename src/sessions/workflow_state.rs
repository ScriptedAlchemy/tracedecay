pub use tracedecay_sessions::runtime::workflow_state::*;

impl WorkflowStateStore for crate::global_db::GlobalDb {
    fn dashboard_connection(&self) -> libsql::Connection {
        self.dashboard_connection()
    }
}
