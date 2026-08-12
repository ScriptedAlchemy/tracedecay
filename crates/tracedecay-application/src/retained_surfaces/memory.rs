use tracedecay_tool_catalog::{EffectClass, ScopeDimension};

use super::{CURRENT_SURFACES, RetainedSurfaceOperation, RetainedSurfaceSpec};

const MEMORY_SCOPE: &[ScopeDimension] = &[ScopeDimension::Resource];

pub(super) const SPECS: [RetainedSurfaceSpec; 13] = [
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::MemoryAutomationRun,
        summary: "Run canonical automatic memory maintenance",
        description: "Executes one admitted automatic-memory run with durable inner mutation receipts and reconciliation.",
        example: r#"{"run_id":"run.memory.example","task":{"kind":"memory_curator","options":{"fact_review_limit":24,"min_confidence_millionths":720000}}}"#,
        effect: EffectClass::Administrative,
        scope: &[ScopeDimension::Project],
        paginated: false,
        surfaces: &[],
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreAdd,
        summary: "Add a retained fact",
        description: "Add one fact through the owner-bound memory application.",
        example: "Remember this project fact",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreSearch,
        summary: "Search retained facts",
        description: "Search authorized facts through the owner-bound memory application.",
        example: "Search the retained project facts",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: true,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreProbe,
        summary: "Probe retained facts",
        description: "Probe authorized facts through the owner-bound memory application.",
        example: "Probe retained project facts for this topic",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: true,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreRelated,
        summary: "Find related retained facts",
        description: "Find authorized related facts through the owner-bound memory application.",
        example: "Find facts related to this retained fact",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: true,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreReason,
        summary: "Reason over retained facts",
        description: "Read authorized supporting facts through the owner-bound memory application.",
        example: "Find retained facts supporting this claim",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: true,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreContradict,
        summary: "Find contradicting retained facts",
        description: "Read authorized contradicting facts through the owner-bound memory application.",
        example: "Find retained facts contradicting this claim",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreGet,
        summary: "Read a retained fact",
        description: "Read one authorized fact through the owner-bound memory application.",
        example: "Read this retained fact",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreUpdate,
        summary: "Update a retained fact",
        description: "Update one authorized fact through the owner-bound memory application.",
        example: "Update this retained fact",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreRemove,
        summary: "Remove a retained fact",
        description: "Remove one authorized fact through the owner-bound memory application.",
        example: "Remove this retained fact",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactStoreList,
        summary: "List retained facts",
        description: "List authorized facts through the owner-bound memory application.",
        example: "List retained project facts",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: true,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::FactFeedback,
        summary: "Record fact feedback",
        description: "Record scoped fact feedback through the owner-bound memory application.",
        example: "Mark this retained fact as helpful",
        effect: EffectClass::Administrative,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
    RetainedSurfaceSpec {
        operation: RetainedSurfaceOperation::MemoryStatus,
        summary: "Inspect memory status",
        description: "Inspect derived memory state through its retained owner.",
        example: "Show retained project memory status",
        effect: EffectClass::Read,
        scope: MEMORY_SCOPE,
        paginated: false,
        surfaces: CURRENT_SURFACES,
    },
];

#[cfg(test)]
mod tests {
    use super::SPECS;
    use crate::retained_surfaces::RetainedSurfaceOperation;

    #[test]
    fn contradiction_is_bounded_while_resumable_memory_reads_are_paginated() {
        let paginated = |operation| {
            SPECS
                .iter()
                .find(|spec| spec.operation == operation)
                .expect("memory operation has a retained catalog entry")
                .paginated
        };

        assert!(!paginated(RetainedSurfaceOperation::FactStoreContradict));
        for operation in [
            RetainedSurfaceOperation::FactStoreSearch,
            RetainedSurfaceOperation::FactStoreProbe,
            RetainedSurfaceOperation::FactStoreRelated,
            RetainedSurfaceOperation::FactStoreReason,
            RetainedSurfaceOperation::FactStoreList,
        ] {
            assert!(paginated(operation));
        }
    }
}
