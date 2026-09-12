//! Manifest-driven host bundle lifecycle contracts.
//!
//! This module plans host-registration mutations only after the embedded
//! first-party catalog verifies manifest identity and content digests. It
//! contains no signing key, trust root, external bundle loader, credential,
//! daemon lifecycle, product semantics, or host-specific business authority.
//!
//! The lifecycle is split along its seams: [`planner`] observes and plans,
//! [`writer`] and [`component_set`] mutate under a recoverable journal,
//! [`doctor`] discovers installed state, [`control`] owns the control
//! directory layout and validators, and [`runtime`] composes injected
//! verifier and storage authorities. Every public item is re-exported here so
//! callers address one module.

use std::path::PathBuf;

pub use tracedecay_host_integration::{
    ClineFamilyAdmissionV1, ClineFamilyEvidenceV1, ClineFamilyProviderV1,
    EmbeddedHostIntegrationEvidenceV1, EmbeddedNativeHostFixtureV1,
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HOST_BUNDLE_SCHEMA_VERSION, HostBundleArtifactContentV1,
    HostBundleArtifactV1, HostBundleBackupArtifactV1, HostBundleBackupReceiptV1,
    HostBundleComponentV1, HostBundleError, HostBundleInstallReceiptV1, HostBundleJournalEntryV1,
    HostBundleJournalStateV1, HostBundleJournalV1, HostBundleLifecycleOpV1, HostBundleManifestV1,
    HostBundleReceiptArtifactV1, HostBundleRestoreReceiptV1, HostBundleRollbackBoundaryV1,
    HostBundleVerificationAdapterV1, HostCapabilityRecordV1, HostCapabilityStateV1,
    HostCapabilityUnavailableReasonV1, HostCapabilityV1, HostComponentSetJournalComponentV1,
    HostComponentSetJournalStateV1, HostComponentSetJournalV1, HostComponentSetReceiptV1,
    HostEditStopConformanceEvidenceV1, HostFeedbackBoundaryEvidenceV1, HostFeedbackBoundaryV1,
    HostKindV1, HostNativeFixtureEvidenceV1, HostRegistrationEvidenceV1, HostRegistrationRouteV1,
    MAX_ARTIFACT_CONTENT_BYTES, MAX_HOST_COMPONENTS, MAX_MANIFEST_ARTIFACTS,
    MAX_RELATIVE_PATH_BYTES, stock_host_capabilities, validate_identifier,
    validate_relative_install_path,
};
use tracedecay_host_integration::{
    cline_family_evidence_from_embedded_assets,
    host_edit_stop_conformance_evidence_from_embedded_assets,
    native_host_edit_stop_conformance_evidence_from_embedded_assets,
    stock_host_native_fixture_evidence_from_embedded_assets,
    stock_host_registration_evidence as stock_host_registration_evidence_from_contract,
};

mod capability_admission;
mod component_set;
mod control;
mod doctor;
mod model;
mod planner;
mod runtime;
#[cfg(test)]
mod tests;
mod writer;

pub use capability_admission::{require_capability, require_component_capabilities};
pub use component_set::HostComponentSetTransactionV1;
pub use control::{
    host_bundle_backup_root, latest_host_component_receipt_at, latest_host_component_set_receipt_at,
};
pub use doctor::{
    HostBundleArtifactDoctorResultV1, HostBundleComponentDoctorResultV1,
    HostBundleComponentDoctorStateV1, HostBundleDoctorReportV1, HostBundleRegistrationInspectorV1,
    HostBundleRegistrationStateV1, inspect_installed_host_bundle_components_at,
};
pub use model::{
    CompetingHostExtensionClaimV1, HostBundleExecutionRequestV1, HostBundleLifecyclePreviewV1,
    HostBundleLifecycleStorageV1, HostBundleRollbackSeamV1, HostComponentSetEntryV1,
    HostComponentSetExecutionRequestV1, HostComponentSetLifecyclePreviewV1,
    HostComponentSetLifecycleRequestV1, HostComponentSetRegistrationV1, HostComponentSetV1,
};
pub use planner::{
    HOST_BUNDLE_STAGE_ROOT_RELATIVE, HostArtifactActionV1, HostArtifactMutationV1,
    HostBundleLifecycleRequestV1, HostBundleMutationPlanV1, ObservedArtifactKindV1,
    ObservedHostArtifactV1, dry_run_host_bundle_lifecycle_at,
    dry_run_host_bundle_lifecycle_with_lifecycle_root_at,
    dry_run_host_component_set_lifecycle_with_lifecycle_root_at, inspect_install_target,
    plan_complete_lifecycle_mutation, plan_lifecycle_mutation,
    plan_verified_complete_lifecycle_mutation, plan_verified_lifecycle_mutation,
};
pub use runtime::{
    FeedbackPathRestoreReceiptV1, FeedbackPathRollbackReceiptV1, FeedbackPathRollbackSwitchV1,
    HostBundleLifecycleRuntimeV1,
};
pub use writer::HostBundleWriterV1;

/// Resolve the lifecycle authority from the active `TraceDecay` user profile.
/// Host homes contain deployed artifacts only; receipts, journals, locks, and
/// rollback backups are owned by this profile-scoped root.
pub fn resolved_host_bundle_lifecycle_root() -> tracedecay_domain::errors::Result<PathBuf> {
    Ok(tracedecay_runtime_core::storage::default_profile_root()?.join("host-components"))
}

/// Canonical stock-host enumeration shared by packaging, delivery, and
/// conformance consumers.
pub const fn stock_host_kinds() -> [HostKindV1; 18] {
    HostKindV1::ALL
}

/// Evidence references are stable repository or host-contract identifiers.
/// Their capability semantics live in the root-free host-integration crate.
pub fn stock_host_registration_evidence(host: HostKindV1) -> Vec<HostRegistrationEvidenceV1> {
    stock_host_registration_evidence_from_contract(host)
}

const CLINE_FAMILY_EVIDENCE_PACKET_PATH: &str =
    "crates/tracedecay-hooks/fixtures/host_events/cline-family.json";
const CLINE_FAMILY_EVIDENCE_PACKET: &[u8] =
    include_bytes!("../../../../../tests/fixtures/packaged_host_events/cline-family.json");
const CLINE_FAMILY_TRANSCRIPT_MANIFEST_PATH: &str =
    "tests/fixtures/transcript_golden/cline_like/manifest.json";
const CLINE_FAMILY_TRANSCRIPT_MANIFEST: &[u8] =
    include_bytes!("../../../../../tests/fixtures/transcript_golden/cline_like/manifest.json");
static EMBEDDED_NATIVE_HOST_FIXTURES: [EmbeddedNativeHostFixtureV1; 7] = [
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::ClaudeCode,
        bytes: include_bytes!("../../../../../tests/fixtures/packaged_host_events/claude.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Codex,
        bytes: include_bytes!("../../../../../tests/fixtures/packaged_host_events/codex.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::CursorDesktop,
        bytes: include_bytes!("../../../../../tests/fixtures/packaged_host_events/cursor.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Hermes,
        bytes: include_bytes!("../../../../../tests/fixtures/packaged_host_events/hermes.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Kiro,
        bytes: include_bytes!("../../../../../tests/fixtures/packaged_host_events/kiro.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::KimiCode,
        bytes: include_bytes!("../../../../../tests/fixtures/packaged_host_events/kimi-code.json"),
    },
    EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::OpenCode,
        bytes: include_bytes!(
            "../../../../../tests/fixtures/packaged_host_events/opencode/baseline.json"
        ),
    },
];

fn embedded_host_integration_evidence() -> EmbeddedHostIntegrationEvidenceV1 {
    EmbeddedHostIntegrationEvidenceV1 {
        cline_family_evidence_packet_path: CLINE_FAMILY_EVIDENCE_PACKET_PATH,
        cline_family_evidence_packet: CLINE_FAMILY_EVIDENCE_PACKET,
        cline_family_transcript_manifest_path: CLINE_FAMILY_TRANSCRIPT_MANIFEST_PATH,
        cline_family_transcript_manifest: CLINE_FAMILY_TRANSCRIPT_MANIFEST,
        native_fixtures: &EMBEDDED_NATIVE_HOST_FIXTURES,
    }
}

pub fn cline_family_evidence(provider: ClineFamilyProviderV1) -> Option<ClineFamilyEvidenceV1> {
    cline_family_evidence_from_embedded_assets(&embedded_host_integration_evidence(), provider)
}

pub fn stock_host_native_fixture_evidence(host: HostKindV1) -> Option<HostNativeFixtureEvidenceV1> {
    stock_host_native_fixture_evidence_from_embedded_assets(
        &embedded_host_integration_evidence(),
        host,
    )
}

pub fn native_host_edit_stop_conformance_evidence() -> Vec<HostNativeFixtureEvidenceV1> {
    native_host_edit_stop_conformance_evidence_from_embedded_assets(
        &embedded_host_integration_evidence(),
    )
}

/// Edit/stop ingress truth for every receipt-backed host. This is deliberately
/// separate from the native-fixture inventory: Gemini and Copilot are present
/// with typed unavailable boundaries, and Kiro remains MCP-only without a
/// fabricated edit or stop callback.
pub fn supported_host_edit_stop_conformance_evidence() -> Vec<HostEditStopConformanceEvidenceV1> {
    super::host_bundle_registry::RECEIPT_BACKED_HOST_KINDS
        .into_iter()
        .map(|host| {
            host_edit_stop_conformance_evidence_from_embedded_assets(
                &embedded_host_integration_evidence(),
                host,
            )
        })
        .collect()
}
