use tracedecay_tool_catalog::{EffectClass, ScopeDimension};

use super::{RetainedSurfaceOperation, RetainedSurfaceSpec};

pub(super) const SPECS: [RetainedSurfaceSpec; 1] = [RetainedSurfaceSpec {
    operation: RetainedSurfaceOperation::AutomationRun,
    summary: "Run canonical automation",
    description: "Executes one admitted automation run with durable committed-effect receipts and reconciliation.",
    example: r#"{"run_id":"run.automation.example","task":{"kind":"memory_curator","options":{"fact_review_limit":24,"min_confidence_millionths":720000}}}"#,
    effect: EffectClass::Administrative,
    scope: &[ScopeDimension::Project],
    paginated: false,
    surfaces: &[],
}];
