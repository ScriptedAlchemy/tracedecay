use std::path::Path;

use crate::project::TraceDecay;
use tracedecay_application::tracedecay::{
    add_local_counter, get_local_counter, get_tokens_saved, reset_local_counter, set_tokens_saved,
};
use tracedecay_configuration::TraceDecayConfig;
use tracedecay_domain::errors::Result;

impl TraceDecay {
    /// Returns the persisted tokens-saved counter.
    pub async fn get_tokens_saved(&self) -> Result<u64> {
        get_tokens_saved(&self.db).await
    }

    /// Persists the tokens-saved counter to the database.
    pub async fn set_tokens_saved(&self, value: u64) -> Result<()> {
        set_tokens_saved(&self.db, value).await
    }

    /// Returns the resettable project-local token counter.
    pub async fn get_local_counter(&self) -> Result<u64> {
        get_local_counter(&self.db).await
    }

    /// Resets the project-local token counter to zero.
    pub async fn reset_local_counter(&self) -> Result<()> {
        reset_local_counter(&self.db).await
    }

    /// Increments the project-local token counter by the given amount.
    pub async fn add_local_counter(&self, delta: u64) -> Result<()> {
        add_local_counter(&self.db, delta).await
    }

    /// Checkpoints the WAL and closes the database connection.
    #[hotpath::measure(label = "daemon.store_meta.checkpoint", future = true)]
    pub async fn checkpoint(&self) -> Result<()> {
        self.db.checkpoint().await
    }

    /// Consumes the code graph and closes the database connection.
    ///
    /// The Context Scout owner started by a writable open keeps a clone of
    /// `db` in the process-global owner registry, so a standalone close must
    /// retire it too: otherwise the database, its writer lease, and its
    /// authority role outlive the graph that published them. The unregister
    /// is identity-guarded, so a newer owner bound to a replacement database
    /// is left in place.
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
    #[hotpath::measure(label = "daemon.store_meta.quick_check", future = true)]
    pub async fn quick_check_report(&self) -> Result<Option<String>> {
        self.db.quick_check_report().await
    }

    /// Returns a reference to the current configuration.
    pub fn get_config(&self) -> &TraceDecayConfig {
        &self.config
    }

    /// Returns the project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}
