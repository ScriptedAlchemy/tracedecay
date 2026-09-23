//! Root-free contracts for embedded host integration bundles.
//!
//! The application binary composes its checked-in plugin assets with
//! `include_bytes!` / `include_str!`, then passes the resulting evidence here.
//! This crate owns immutable manifest, receipt, and capability-evidence
//! contracts; root adapters retain CLI dispatch and filesystem mutation.

use thiserror::Error;

pub use tracedecay_domain::{
    HostCapabilityRecordV1, HostCapabilityStateV1, HostCapabilityUnavailableReasonV1,
    HostCapabilityV1, HostComponentV1, HostKindV1, stock_host_capabilities,
};

mod evidence;
mod manifest;
mod receipt;

#[cfg(test)]
pub(crate) use evidence::HOST_REGISTRATIONS;
pub use evidence::{
    ClineFamilyAdmissionV1, ClineFamilyEvidenceV1, ClineFamilyProviderV1,
    EmbeddedHostIntegrationEvidenceV1, EmbeddedNativeHostFixtureV1,
    HostEditStopConformanceEvidenceV1, HostFeedbackBoundaryEvidenceV1, HostFeedbackBoundaryV1,
    HostNativeFixtureEvidenceV1, HostRegistrationEvidenceV1, HostRegistrationRouteV1,
    cline_family_evidence_from_embedded_assets,
    host_edit_stop_conformance_evidence_from_embedded_assets,
    native_host_edit_stop_conformance_evidence_from_embedded_assets,
    stock_host_native_fixture_evidence_from_embedded_assets, stock_host_registration_evidence,
};
pub use manifest::{
    HOST_BUNDLE_SCHEMA_VERSION, HostBundleArtifactContentV1, HostBundleArtifactV1,
    HostBundleLifecycleOpV1, HostBundleManifestV1, HostBundleVerificationAdapterV1,
    MAX_ARTIFACT_CONTENT_BYTES, MAX_HOST_COMPONENTS, MAX_IDENTIFIER_BYTES, MAX_MANIFEST_ARTIFACTS,
    MAX_RELATIVE_PATH_BYTES, validate_identifier, validate_relative_install_path,
};
pub use receipt::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleInstallReceiptV1, HostBundleReceiptArtifactV1,
    HostComponentSetReceiptV1,
};

/// Builds a [`HostBundleError::StorageFailure`] tagged with the `file:line` of
/// the site that observed the failure.
///
/// Host bundle lifecycle code has roughly a hundred atomic filesystem steps that
/// all collapse to `StorageFailure`. Without a per-site tag every one of them
/// renders the same sentence, which makes an install/uninstall failure report
/// unactionable. Always construct the variant through this macro.
#[macro_export]
macro_rules! host_bundle_storage_failure {
    () => {
        $crate::HostBundleError::StorageFailure(::core::concat!(
            ::core::file!(),
            ":",
            ::core::line!()
        ))
    };
}

/// Builds a [`HostBundleError::StalePreview`] tagged with the `file:line` of the
/// site that observed the drift.
///
/// Preview/apply matching is checked at many independent layers (plan digest,
/// per-artifact digest, registration set, observed host state). They all collapse
/// to `StalePreview`, so the tag is what distinguishes real host drift from a
/// lifecycle bug. Always construct the variant through this macro.
#[macro_export]
macro_rules! host_bundle_stale_preview {
    () => {
        $crate::HostBundleError::StalePreview(::core::concat!(
            ::core::file!(),
            ":",
            ::core::line!()
        ))
    };
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HostBundleError {
    #[error("host capability is unsupported and must not be emulated")]
    UnsupportedCapability,
    #[error("host-native plugin cache update is required before this lifecycle can complete")]
    NativeUpdateRequired,
    #[error("host-native plugin removal is required before this lifecycle can complete")]
    NativeRemovalRequired,
    #[error(
        "{host:?} host CLI is unavailable; install the host CLI or add it to PATH before retrying"
    )]
    HostCliUnavailable { host: HostKindV1 },
    #[error("bundle manifest schema version is unsupported")]
    UnsupportedManifestVersion,
    #[error("bundle manifest is structurally invalid")]
    InvalidManifest,
    #[error("first-party component identity or content digest is invalid")]
    CatalogMismatch,
    #[error("bundle manifest payload cannot be canonicalized")]
    CanonicalizationFailed,
    #[error("bundle does not address the requested host/component")]
    WrongTarget,
    #[error("lifecycle mutation requires explicit confirmation")]
    ConfirmationRequired,
    /// A deploy path or registration surface is claimed by something other
    /// than this component. The payload names the conflicting path (and the
    /// observed vs expected ownership marker where one exists) so the
    /// operator can resolve the exact file instead of guessing.
    #[error("bundle ownership conflict: {0}")]
    OwnershipConflict(String),
    #[error("install target is absolute, traversing, symlinked, or otherwise unsafe")]
    UnsafeInstallPath,
    #[error(
        "Claude home configuration path ~/.claude is a symlink; replace it with a real directory before retrying"
    )]
    UnsafeClaudeHomeSymlink,
    #[error("observed installation state is incomplete or duplicated")]
    InvalidObservedState,
    #[error("Hermes must bind exactly one user TraceDecay profile")]
    InvalidHermesProfileBinding,
    #[error("bundle artifact content is missing, oversized, duplicated, or digest-mismatched")]
    ArtifactContentMismatch,
    #[error("host bundle receipt is invalid")]
    ReceiptCorrupted,
    /// A receipt was written with an older receipt schema. Nothing migrates
    /// it: the operator reinstalls, which discards the stale receipts.
    #[error(
        "host bundle receipts were written by an older TraceDecay; run `tracedecay install --yes --adopt` to replace them"
    )]
    ReinstallRequired,
    /// An atomic filesystem step failed. The payload names the source site that
    /// observed the failure so the ~100 construction sites stay distinguishable
    /// in user-facing output and bug reports; build it with
    /// [`host_bundle_storage_failure!`] rather than by hand.
    #[error("host bundle atomic filesystem operation failed at {0}")]
    StorageFailure(&'static str),
    /// Another process holds this host's lifecycle writer lock.
    #[error(
        "another TraceDecay process is changing this host's integration; retry when it finishes"
    )]
    HostWriterBusy,
    /// Apply observed drift from the confirmed preview. The payload names the
    /// matching layer that rejected, so genuine host drift is distinguishable
    /// from a lifecycle bug; build it with [`host_bundle_stale_preview!`] rather
    /// than by hand.
    #[error("confirmed host lifecycle preview is stale or does not match apply (at {0})")]
    StalePreview(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    const CLINE_PACKET: &[u8] = br#"{
        "providers": [
            {
                "provider": "cline",
                "host_hook_admission": "unavailable",
                "reason": "native_fixture_missing"
            }
        ]
    }"#;
    const TRANSCRIPT: &[u8] = br#"{"fixture": "cline"}"#;
    const CODEX_FIXTURE: &[u8] = br#"{
        "provider": "codex",
        "events": [
            {"identity":"saved_edit","support":"documented_unverified"},
            {"identity":"stop","support":"native"}
        ]
    }"#;
    const NATIVE_FIXTURES: &[EmbeddedNativeHostFixtureV1] = &[EmbeddedNativeHostFixtureV1 {
        host: HostKindV1::Codex,
        bytes: CODEX_FIXTURE,
    }];
    const ASSETS: EmbeddedHostIntegrationEvidenceV1 = EmbeddedHostIntegrationEvidenceV1 {
        cline_family_evidence_packet_path: "fixtures/cline-family.json",
        cline_family_evidence_packet: CLINE_PACKET,
        cline_family_transcript_manifest_path: "fixtures/cline-manifest.json",
        cline_family_transcript_manifest: TRANSCRIPT,
        native_fixtures: NATIVE_FIXTURES,
    };

    #[test]
    fn evidence_digests_root_composed_fixture_bytes() {
        let evidence =
            stock_host_native_fixture_evidence_from_embedded_assets(&ASSETS, HostKindV1::Codex)
                .expect("root-composed Codex fixture is present");
        let expected_digest: [u8; 32] = Sha256::digest(CODEX_FIXTURE).into();
        assert_eq!(evidence.fixture_digest, expected_digest);
        assert_eq!(
            evidence.edit,
            HostCapabilityStateV1::Unavailable(
                HostCapabilityUnavailableReasonV1::NativeFixtureLimited
            )
        );
        assert_eq!(evidence.stop, HostCapabilityStateV1::Supported);
    }

    #[test]
    fn edit_stop_conformance_does_not_promote_explicit_read_routes_to_events() {
        let evidence =
            host_edit_stop_conformance_evidence_from_embedded_assets(&ASSETS, HostKindV1::Codex);
        assert_eq!(evidence.edit.route, None);
        assert_eq!(
            evidence.edit.state,
            HostCapabilityStateV1::Unavailable(
                HostCapabilityUnavailableReasonV1::NativeFixtureLimited
            )
        );
        assert_eq!(evidence.stop.route, Some(HostRegistrationRouteV1::Hook));

        let gemini =
            host_edit_stop_conformance_evidence_from_embedded_assets(&ASSETS, HostKindV1::Gemini);
        assert_eq!(gemini.edit.route, None);
        assert_eq!(gemini.stop.route, None);
        assert_eq!(gemini.edit.native_fixture_digest, None);
    }

    /// Kimi MCP is the installer-owned session/user `mcp.json` route. The
    /// packaged plugin manifest omits `mcpServers` on purpose, so citing that
    /// file would treat omission as supported-MCP evidence.
    #[test]
    fn kimi_mcp_evidence_is_installer_user_config_route() {
        let evidence = stock_host_registration_evidence(HostKindV1::KimiCode);
        let mcp = evidence
            .iter()
            .find(|row| row.route == HostRegistrationRouteV1::Mcp)
            .expect("Kimi MCP registration row");
        let hook = evidence
            .iter()
            .find(|row| row.route == HostRegistrationRouteV1::Hook)
            .expect("Kimi hook registration row");
        assert_eq!(mcp.state, HostCapabilityStateV1::Supported);
        assert_eq!(mcp.evidence_ref, "src/agents/kimi.rs");
        assert_eq!(hook.evidence_ref, "plugin/.kimi-plugin/plugin.json");
        assert_ne!(
            mcp.evidence_ref, hook.evidence_ref,
            "Kimi MCP must not reuse the plugin-manifest hook authority"
        );
    }

    /// Every `HostKindV1` variant owns table rows: a CLI route first plus at
    /// least one daemon route, each route once, each with a non-empty evidence
    /// reference. A host-specific route (Claude/OpenCode LSP, Cursor native
    /// diagnostics) is only ever listed on a host whose canonical capability
    /// is Supported: it is a narrower registration of that capability, not a
    /// claim beyond it.
    #[test]
    fn registration_table_covers_every_stock_host() {
        for host in HostKindV1::ALL {
            let rows = HOST_REGISTRATIONS
                .iter()
                .filter(|row| row.host == host)
                .collect::<Vec<_>>();
            assert!(rows.len() >= 2, "{host:?} lists a CLI and a daemon route");
            assert_eq!(rows[0].route, HostRegistrationRouteV1::Cli, "{host:?}");

            let evidence = stock_host_registration_evidence(host);
            assert_eq!(evidence.len(), rows.len(), "{host:?}");
            let capabilities = stock_host_capabilities(host);
            for (index, (row, record)) in rows.iter().zip(&evidence).enumerate() {
                assert!(
                    !rows[..index]
                        .iter()
                        .any(|earlier| earlier.route == row.route),
                    "{host:?} repeats {:?}",
                    row.route
                );
                assert!(!row.evidence_ref.is_empty(), "{host:?} {:?}", row.route);
                assert!(!row.starts_analyzer, "{host:?} {:?}", row.route);
                assert_eq!(
                    (record.route, record.evidence_ref, record.starts_analyzer),
                    (row.route, row.evidence_ref, row.starts_analyzer),
                    "{host:?}"
                );
                assert_eq!(
                    record.state,
                    capabilities[row.route.capability().row_index()].state,
                    "{host:?} {:?} state is the canonical capability state",
                    row.route
                );
                if !matches!(
                    row.route,
                    HostRegistrationRouteV1::Hook
                        | HostRegistrationRouteV1::Mcp
                        | HostRegistrationRouteV1::Cli
                ) {
                    assert_eq!(
                        record.state,
                        HostCapabilityStateV1::Supported,
                        "{host:?} lists host-specific route {:?} without support",
                        row.route
                    );
                }
            }
        }
    }

    #[test]
    fn cline_family_hooks_stay_unverified_while_exact_hosts_support_mcp() {
        assert!(
            stock_host_registration_evidence(HostKindV1::ClineFamily)
                .iter()
                .all(|evidence| matches!(evidence.state, HostCapabilityStateV1::Unavailable(_)))
        );
        for host in [HostKindV1::Cline, HostKindV1::RooCode, HostKindV1::Kilo] {
            let evidence = stock_host_registration_evidence(host);
            assert!(evidence.iter().any(|record| {
                record.route == HostRegistrationRouteV1::Mcp
                    && record.state == HostCapabilityStateV1::Supported
            }));
            assert!(evidence.iter().any(|record| {
                record.route == HostRegistrationRouteV1::Hook
                    && matches!(record.state, HostCapabilityStateV1::Unavailable(_))
            }));
        }
    }

    #[test]
    fn cline_admission_uses_the_packet_reason_verbatim() {
        let evidence =
            cline_family_evidence_from_embedded_assets(&ASSETS, ClineFamilyProviderV1::Cline)
                .expect("Cline record is present");
        assert_eq!(evidence.admission, ClineFamilyAdmissionV1::Unavailable);
        assert_eq!(
            evidence.unavailable_reason.as_deref(),
            Some("native_fixture_missing")
        );
    }
}
