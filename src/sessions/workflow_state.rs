pub(crate) use tracedecay_sessions::runtime::workflow_state::WorkflowStateStore;
pub use tracedecay_sessions::runtime::workflow_state::{WorkflowStateItem, list_unfinished};

impl WorkflowStateStore for crate::global_db::GlobalDb {
    fn dashboard_connection(&self) -> libsql::Connection {
        self.dashboard_connection()
    }
}
