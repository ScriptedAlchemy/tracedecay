use tracedecay_automation_runtime::ports::project_runtime::AutomationProjectContext;
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::TraceDecay;

impl TraceDecay {
    /// Captures the immutable authorities required by one project automation run.
    ///
    /// Read-only graphs are rejected because every project automation task may
    /// publish memory, ledger, or managed-skill effects.
    pub fn automation_project_context(&self) -> Result<AutomationProjectContext> {
        if self.is_read_only() {
            return Err(TraceDecayError::Config {
                message:
                    "cannot open project store for writing: active TraceDecay store is open read-only"
                        .to_owned(),
            });
        }
        let FactOwnerV1::Project { project_id } = self.project_memory_owner()? else {
            return Err(TraceDecayError::Config {
                message: "project automation requires authoritative project scope".to_owned(),
            });
        };
        Ok(AutomationProjectContext {
            project_root: self.project_root().to_path_buf(),
            dashboard_root: self.store_layout().dashboard_root.clone(),
            host_io: tracedecay_agent_hosts::host_io(),
            project_id,
            profile_id: self.project_store_runtime().profile_id().clone(),
            profile_database: self.profile_database().clone(),
            project_sessions: self.configuration_runtime().registered_database(),
            project_memory_database: self.retained_project_store_db()?,
        })
    }
}
