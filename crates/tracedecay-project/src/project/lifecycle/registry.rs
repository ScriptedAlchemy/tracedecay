//! Global-registry registration: `TraceDecay` retains owner handles and
//! delegates publishing to [`tracedecay_global_db::register_project_store`].

use tracedecay_domain::errors::Result;

use super::TraceDecay;

impl TraceDecay {
    #[hotpath::measure(label = "lifecycle.register_project_store", future = true)]
    pub async fn register_project_store_in_global_registry(&self) -> Result<()> {
        tracedecay_global_db::register_project_store(
            self.profile_database.as_ref(),
            &self.project_root,
            &self.store_layout,
        )
        .await
    }
}
