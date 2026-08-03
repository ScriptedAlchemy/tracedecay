pub use tracedecay_sessions::runtime::workflow_state::{WorkflowStateItem, list_unfinished};
pub(crate) use tracedecay_sessions::runtime::workflow_state::WorkflowStateStore;

impl WorkflowStateStore for crate::global_db::GlobalDb {
    fn dashboard_connection(&self) -> libsql::Connection {
        self.dashboard_connection()
    }
}
