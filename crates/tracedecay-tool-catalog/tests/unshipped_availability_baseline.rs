//! Unshipped V2 availability baseline.
//!
//! `Deprecated`, `DeprecationWindow`, and unused `UnavailabilityReason`
//! variants existed only on the redesign branch (`bb9674d9a`) and are absent
//! from released `origin/master` (the `tracedecay-tool-catalog` crate itself
//! is unshipped there). Their removal is an accepted unshipped-V2 source
//! break; this file locks the closed final set and proves SDK generation does
//! not depend on the deleted symbols.

use tracedecay_tool_catalog::{AvailabilityContract, UnavailabilityReason};

const TOOL_CATALOG_MANIFEST: &str = include_str!("../Cargo.toml");
const TOOL_CATALOG_LIB: &str = include_str!("../src/lib.rs");
const TOOL_CATALOG_MANIFEST_RS: &str = include_str!("../src/manifest.rs");
const SDK_MANIFEST: &str = include_str!("../../tracedecay-sdk/Cargo.toml");
const SDK_LIB: &str = include_str!("../../tracedecay-sdk/src/lib.rs");
const SDK_GENERATE: &str = include_str!("../../tracedecay-sdk/src/bin/generate.rs");
const SDK_OPERATIONS: &str = include_str!("../../tracedecay-sdk/src/operations.rs");

#[test]
fn availability_contract_is_closed_final_v2_set() {
    assert!(AvailabilityContract::Available.is_callable());
    assert!(!AvailabilityContract::Unavailable {
        reason: UnavailabilityReason::NotImplemented,
    }
    .is_callable());

    // Exhaustive match fails to compile if unshipped variants return.
    match AvailabilityContract::Available {
        AvailabilityContract::Available => {}
        AvailabilityContract::Unavailable { reason } => match reason {
            UnavailabilityReason::NotImplemented => {}
        },
    }
}

#[test]
fn unshipped_deprecation_symbols_are_absent_from_public_source() {
    for forbidden in [
        "pub struct DeprecationWindow",
        "Deprecated { window",
        "FeatureDisabled,",
        "PolicyDisabled,",
        "Retired,",
        "DeprecationWindow,",
    ] {
        assert!(
            !TOOL_CATALOG_MANIFEST_RS.contains(forbidden),
            "tool-catalog manifest must not define unshipped symbol pattern {forbidden}"
        );
        assert!(
            !TOOL_CATALOG_LIB.contains(forbidden),
            "tool-catalog lib must not re-export unshipped symbol pattern {forbidden}"
        );
    }
    assert!(
        !TOOL_CATALOG_LIB.contains("DeprecationWindow"),
        "tool-catalog lib must not name DeprecationWindow"
    );
}

#[test]
fn semver_remains_prerelease_for_unshipped_catalog_and_sdk() {
    assert!(
        TOOL_CATALOG_MANIFEST.contains("version = \"0.1.0\""),
        "tracedecay-tool-catalog must stay at unshipped 0.1.0 while Deprecated removal is waived"
    );
    assert!(
        SDK_MANIFEST.contains("version = \"0.1.0\""),
        "tracedecay-sdk must stay at unshipped 0.1.0 while catalog source breaks remain waived"
    );
    assert!(
        SDK_MANIFEST.contains("tracedecay-tool-catalog"),
        "sdk depends on tool-catalog as operation authority"
    );
}

#[test]
fn sdk_generation_has_no_deprecated_availability_dependency() {
    assert!(
        SDK_LIB.contains("pub use tracedecay_tool_catalog as operation"),
        "sdk continues to re-export tool-catalog as operation"
    );
    for source in [SDK_GENERATE, SDK_OPERATIONS, SDK_LIB] {
        for forbidden in [
            "DeprecationWindow",
            "AvailabilityContract::Deprecated",
            "FeatureDisabled",
            "PolicyDisabled",
            "UnavailabilityReason::Retired",
        ] {
            assert!(
                !source.contains(forbidden),
                "sdk generation/source must not depend on unshipped symbol {forbidden}"
            );
        }
    }
}
