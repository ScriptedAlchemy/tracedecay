use tracedecay_api::WorkflowOperation;

use super::work::ApplicationInvocationArgs;

pub type WorkflowInvocationArgs = ApplicationInvocationArgs<WorkflowOperation>;
