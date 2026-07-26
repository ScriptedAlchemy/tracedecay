use tracedecay_tool_catalog::{EffectClass, ScopeDimension};

use super::{RetainedSurfaceOperation, RetainedSurfaceSpec};

const MEMORY_SCOPE: &[ScopeDimension] = &[ScopeDimension::Resource];

pub(super) const SPECS: [RetainedSurfaceSpec; 3] = [
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStore,
        summary: "Use the retained fact store",
        description: "Read or curate facts through the owner-bound memory application.",
        example: "Search the retained project facts",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: true,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactFeedback,
        summary: "Record fact feedback",
        description: "Record scoped fact feedback through the owner-bound memory application.",
        example: "Mark this retained fact as helpful",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: false,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::MemoryStatus,
        summary: "Inspect and repair memory status",
        description: "Inspect and repair derived memory state through its retained owner.",
        example: "Show retained project memory status",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: false,
    },
];
