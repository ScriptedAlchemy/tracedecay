use tracedecay_tool_catalog::{CancellationPoint, EffectClass};

use super::*;

#[test]
fn public_memory_automation_uses_the_admitted_effect_contract() {
    let catalog = mcp_dispatch_catalog().unwrap();
    let contract = catalog
        .contract("tracedecay_memory_automation_run")
        .unwrap();
    assert_eq!(contract.effect(), EffectClass::Administrative);
    assert!(contract.availability().is_available());
    assert_eq!(
        contract.cancellation().points(),
        &[CancellationPoint::EffectInFlight]
    );
    assert!(tool_requires_canonical_effect_settlement(
        "tracedecay_memory_automation_run"
    ));
    for tool_name in [
        "tracedecay_memory_automation_run",
        "tracedecay_automation_run_list",
        "tracedecay_automation_run_view",
        "tracedecay_automation_run_artifact_view",
    ] {
        assert_eq!(
            binding(tool_name).map(|binding| binding.project),
            Some(RegisteredProjectAccess::ActiveProjectOnly)
        );
        assert!(!tool_accepts_registered_project_selector(tool_name));
    }
}
