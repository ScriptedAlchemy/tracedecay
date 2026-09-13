use tracedecay_tool_catalog::{EffectClass, ScopeDimension};

use super::{CURRENT_SURFACES, RetainedSurfaceOperation, RetainedSurfaceSpec};

const PROJECT_SESSION_SCOPE: &[ScopeDimension] =
    &[ScopeDimension::Project, ScopeDimension::Session];

pub(super) const SPECS: [RetainedSurfaceSpec; 1] = [RetainedSurfaceSpec {
    operation: RetainedSurfaceOperation::Workflows,
    summary: "Read retained workflow runs",
    description: "Read workflow runs through the registered workflow-index owner.",
    example: "Show workflow runs for this session",
    effect: EffectClass::Read,
    scope: PROJECT_SESSION_SCOPE,
    paginated: true,
    surfaces: CURRENT_SURFACES,
}];
