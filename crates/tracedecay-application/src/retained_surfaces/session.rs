use tracedecay_tool_catalog::{EffectClass, ScopeDimension};

use super::{RetainedSurfaceOperation, RetainedSurfaceSpec};

const SESSION_SCOPE: &[ScopeDimension] = &[ScopeDimension::Session, ScopeDimension::Resource];
const PROJECT_SCOPE: &[ScopeDimension] = &[ScopeDimension::Project];

pub(super) const SPECS: [RetainedSurfaceSpec; 3] = [
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::SessionRefresh,
        summary: "Control session refresh",
        description: "Start, inspect, resume, or cancel the exact daemon-owned session refresh.",
        example: "Inspect this session refresh",
        effect: EffectClass::Administrative,
        scope: SESSION_SCOPE,
        paginated: false,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::MessageSearch,
        summary: "Search retained session messages",
        description: "Read authorized temporal message evidence without opening another store.",
        example: "Search retained session messages",
        effect: EffectClass::Read,
        scope: SESSION_SCOPE,
        paginated: true,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::SessionsFor,
        summary: "Find sessions for a Git reference",
        description: "Read project sessions correlated with one admitted Git reference.",
        example: "Find sessions for this branch",
        effect: EffectClass::Read,
        scope: PROJECT_SCOPE,
        paginated: true,
    },
];
