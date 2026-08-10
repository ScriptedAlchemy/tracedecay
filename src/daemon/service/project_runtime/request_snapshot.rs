//! One-lock snapshot of the project runtimes used by invocation dispatch.

use std::path::Path;
use std::sync::Arc;

use crate::application::feedback::concrete::FeedbackRuntime;
use crate::daemon::service::invocation::{
    DaemonFeedbackInvocationOwner, DaemonLspInvocationOwner, RegisteredConfigurationRuntime,
    RegisteredFeedbackRuntime, RegisteredRetainedRuntime, RegisteredWorkRuntime,
};

use super::ProjectRuntimeRegistryV1;

/// The per-project components one request may need, resolved together.
#[derive(Default)]
pub(in crate::daemon::service) struct ProjectRequestRuntimesV1 {
    pub(in crate::daemon::service) feedback: Option<Arc<FeedbackRuntime>>,
    pub(in crate::daemon::service) feedback_owner: Option<DaemonFeedbackInvocationOwner>,
    pub(in crate::daemon::service) configuration: Option<RegisteredConfigurationRuntime>,
    pub(in crate::daemon::service) work: Option<RegisteredWorkRuntime>,
    pub(in crate::daemon::service) retained: Option<RegisteredRetainedRuntime>,
    pub(in crate::daemon::service) lsp_owner: Option<DaemonLspInvocationOwner>,
}

impl ProjectRuntimeRegistryV1 {
    /// Resolve all request runtimes from one consistent registry view.
    ///
    /// Only LSP retains canonical-root fallback because it is the sole runtime
    /// historically registered by either spelling of an opened root.
    pub(in crate::daemon::service) async fn request_runtimes(
        &self,
        project_root: Option<&Path>,
        canonical_root: Option<&Path>,
    ) -> ProjectRequestRuntimesV1 {
        let Some(project_root) = project_root else {
            return ProjectRequestRuntimesV1::default();
        };
        let runtimes = self.lock_runtimes();
        let runtime = runtimes.get(project_root);
        let feedback = runtime.and_then(|runtime| runtime.feedback.as_ref());
        ProjectRequestRuntimesV1 {
            feedback: feedback.map(RegisteredFeedbackRuntime::runtime),
            feedback_owner: feedback.map(RegisteredFeedbackRuntime::invocation_owner),
            configuration: runtime.and_then(|runtime| runtime.configuration.clone()),
            work: runtime.and_then(|runtime| runtime.work.clone()),
            retained: runtime.and_then(|runtime| runtime.retained.clone()),
            lsp_owner: Self::component_with_canonical_fallback::<DaemonLspInvocationOwner>(
                &runtimes,
                project_root,
                canonical_root,
            ),
        }
    }
}
