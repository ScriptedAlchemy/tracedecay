use tracedecay_tool_catalog::{EffectClass, ScopeDimension};

use super::{CURRENT_SURFACES, RetainedSurfaceOperation, RetainedSurfaceSpec};

pub(super) const SPECS: [RetainedSurfaceSpec; 1] = [RetainedSurfaceSpec {
    operation: RetainedSurfaceOperation::FactStoreCurate,
    summary: "Curate retained facts automatically",
    description: "Runs the canonical Memory Curator with caller-owned bounds and daemon-owned run identity, operations, validation, policy, and apply authority.",
    example: r#"{"fact_review_limit":24,"min_confidence_millionths":720000}"#,
    effect: EffectClass::Administrative,
    scope: &[ScopeDimension::Project],
    paginated: false,
    surfaces: CURRENT_SURFACES,
}];
