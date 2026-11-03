use std::path::Path;

use crate::project::TraceDecay;
use tracedecay_application::tracedecay::{
    get_local_counter, get_tokens_saved, record_token_savings, reset_local_counter,
};
use tracedecay_configuration::config::RuntimeTraceDecayConfig;
use tracedecay_domain::errors::Result;

impl TraceDecay {
    /// Returns the persisted tokens-saved counter.
    pub async fn get_tokens_saved(&self) -> Result<u64> {
        get_tokens_saved(&self.db).await
    }

    /// Atomically credits lifetime and resettable savings for one response.
    /// Returns the committed lifetime total.
    pub async fn record_token_savings(&self, delta: u64) -> Result<u64> {
        record_token_savings(&self.db, delta).await
    }

    /// Returns the resettable project-local token counter.
    pub async fn get_local_counter(&self) -> Result<u64> {
        get_local_counter(&self.db).await
    }

    /// Resets the project-local token counter to zero.
    pub async fn reset_local_counter(&self) -> Result<()> {
        reset_local_counter(&self.db).await
    }

    /// Checkpoints the WAL and closes the database connection.
    #[tracing::instrument(name = "daemon.store_meta.checkpoint", level = "trace", skip_all)]
    pub async fn checkpoint(&self) -> Result<()> {
        self.db.checkpoint().await
    }

    /// Consumes the code graph and closes the database connection.
    ///
    /// Close also removes the process-global route to the Context Scout owner
    /// this graph published, even while another graph still holds that
    /// owner. The unregister is identity-guarded, so a newer owner bound to a
    /// replacement database is left in place.
    pub fn close(self) {
        if !self.read_only
            && let Some(project_id) =
                tracedecay_agent_hosts::hooks::hook_project_id_for_layout(&self.store_layout)
        {
            let _ = tracedecay_agent_hosts::agents::context_scout::owner::unregister_registered_context_scout_owner(
                project_id,
                self.db.canonical_database_path(),
            );
        }
        self.db.close();
    }

    /// Run the quick integrity check and return the first problem row, if any.
    #[tracing::instrument(name = "daemon.store_meta.quick_check", level = "trace", skip_all)]
    pub async fn quick_check_report(&self) -> Result<Option<String>> {
        self.db.quick_check_report().await
    }

    /// Returns a reference to the current configuration.
    pub fn get_config(&self) -> &RuntimeTraceDecayConfig {
        &self.config
    }

    /// Returns the project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}
