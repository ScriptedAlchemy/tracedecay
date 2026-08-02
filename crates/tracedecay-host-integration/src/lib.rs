//! Root-free contracts for embedded host integration bundles.
//!
//! The application binary composes its checked-in plugin assets with
//! `include_bytes!` / `include_str!`, then passes the resulting evidence here.
//! This crate owns immutable manifest, receipt, journal, and capability-evidence
//! contracts; root adapters retain CLI dispatch and filesystem mutation.

use std::path::{Component, Path};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::canonical_json_bytes;
pub use tracedecay_domain::{
    HostCapabilityRecordV1, HostCapabilityStateV1, HostCapabilityUnavailableReasonV1,
    HostCapabilityV1, HostKindV1, stock_host_capabilities,
};

pub const HOST_BUNDLE_SCHEMA_VERSION: u16 = 1;
pub const HOST_BUNDLE_RECEIPT_SCHEMA_VERSION: u16 = 1;
pub const MAX_MANIFEST_ARTIFACTS: usize = 128;
pub const MAX_HOST_COMPONENTS: usize = 4;
pub const MAX_RELATIVE_PATH_BYTES: usize = 512;
pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_ARTIFACT_CONTENT_BYTES: usize = 1024 * 1024;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleComponentV1 {
    Core,
    Agent,
    ContextMcp,
    OperatorMcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleLifecycleOpV1 {
    Install,
    Update,
    Repair,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRegistrationRouteV1 {
    ClaudeConfiguredLanguageLsp,
    CursorNativeDiagnostics,
    OpenCodeCustomLsp,
    Hook,
    Mcp,
    Cli,
}

/// Evidence behind one stock-host registration route. `starts_analyzer` is
/// explicit so a projection bridge cannot silently claim or spawn a language
/// analyzer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostRegistrationEvidenceV1 {
    pub route: HostRegistrationRouteV1,
    pub state: HostCapabilityStateV1,
    pub evidence_ref: &'static str,
    pub starts_analyzer: bool,
}

/// Truthful host registration matrix used by packaging and conformance
/// consumers. Evidence references are stable repository or host-contract
/// identifiers, never inferred compatibility claims.
pub fn stock_host_registration_evidence(host: HostKindV1) -> Vec<HostRegistrationEvidenceV1> {
    use HostCapabilityStateV1::{Degraded, Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::{
        CheckedInEvidenceMissing, HostApiAbsent, HostRegistrationUnsupported, NativeFixtureLimited,
    };
    use HostRegistrationRouteV1::{
        ClaudeConfiguredLanguageLsp, Cli, CursorNativeDiagnostics, Hook, Mcp, OpenCodeCustomLsp,
    };

    let mut evidence = vec![HostRegistrationEvidenceV1 {
        route: Cli,
        state: if host == HostKindV1::CursorCloud {
            Unavailable(HostRegistrationUnsupported)
        } else {
            Supported
        },
        evidence_ref: "src/tool_command.rs",
        starts_analyzer: false,
    }];
    match host {
        HostKindV1::ClaudeCode => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: ClaudeConfiguredLanguageLsp,
                state: Supported,
                evidence_ref: "plugin/.lsp.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Supported,
                evidence_ref: "plugin/hooks/hooks-claude.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "plugin/.mcp.json",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::CursorDesktop => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: CursorNativeDiagnostics,
                state: Supported,
                evidence_ref: "plugin/cursor-native-extension/package.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Supported,
                evidence_ref: "plugin/hooks/hooks-cursor.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "plugin/mcp-cursor.json",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::CursorCloud => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Unavailable(CheckedInEvidenceMissing),
                evidence_ref: "cursor_cloud_native_hook_fixture_absent_v1",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Unavailable(HostRegistrationUnsupported),
                evidence_ref: "cursor_cloud_host_registration_absent_v1",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::Codex => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Supported,
                evidence_ref: "plugin/hooks/hooks-codex.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "plugin/.mcp.json",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::Hermes => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Supported,
                evidence_ref: "src/agents/hermes/templates.rs",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "src/agents/hermes/profile_config.rs",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::Kiro => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Degraded(NativeFixtureLimited),
                evidence_ref: "tests/fixtures/host_events/kiro/baseline.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "src/agents/kiro.rs",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::ClineFamily => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Unavailable(HostApiAbsent),
                evidence_ref: "cline_family_hook_evidence_absent_v1",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "cline_user_mcp_settings_v1",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::Cline | HostKindV1::RooCode | HostKindV1::Kilo => {
            let evidence_ref = match host {
                HostKindV1::Cline => "src/agents/cline.rs",
                HostKindV1::RooCode => "src/agents/roo_code.rs",
                HostKindV1::Kilo => "src/agents/kilo.rs",
                _ => unreachable!(),
            };
            evidence.extend([
                HostRegistrationEvidenceV1 {
                    route: Hook,
                    state: Unavailable(CheckedInEvidenceMissing),
                    evidence_ref: "tests/fixtures/transcript_golden/cline_like/manifest.json",
                    starts_analyzer: false,
                },
                HostRegistrationEvidenceV1 {
                    route: Mcp,
                    state: Supported,
                    evidence_ref,
                    starts_analyzer: false,
                },
            ]);
        }
        HostKindV1::KimiCode => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Supported,
                evidence_ref: "plugin/.kimi-plugin/plugin.json",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "plugin/.kimi-plugin/plugin.json",
                starts_analyzer: false,
            },
        ]),
        HostKindV1::OpenCode => evidence.extend([
            HostRegistrationEvidenceV1 {
                route: OpenCodeCustomLsp,
                state: Supported,
                evidence_ref: "src/agents/opencode.rs",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Hook,
                state: Supported,
                evidence_ref: "plugin/opencode/tracedecay.ts",
                starts_analyzer: false,
            },
            HostRegistrationEvidenceV1 {
                route: Mcp,
                state: Supported,
                evidence_ref: "src/agents/opencode.rs",
                starts_analyzer: false,
            },
        ]),
    }
    evidence
}

/// Bytes for one checked-in native host fixture. Root composition supplies the
/// bytes so this crate never reads a repository-relative fixture at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedNativeHostFixtureV1 {
    pub host: HostKindV1,
    pub bytes: &'static [u8],
}

/// Root-composed checked-in evidence. Host adapters retain `include_bytes!`
/// ownership; this contract only parses and digests the supplied bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedHostIntegrationEvidenceV1 {
    pub cline_family_evidence_packet_path: &'static str,
    pub cline_family_evidence_packet: &'static [u8],
    pub cline_family_transcript_manifest_path: &'static str,
    pub cline_family_transcript_manifest: &'static [u8],
    pub native_fixtures: &'static [EmbeddedNativeHostFixtureV1],
}

/// Source-backed native hook fixture evidence. The fixture digest is computed
/// from the checked-in bytes; no protocol field or event is synthesized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostNativeFixtureEvidenceV1 {
    pub host: HostKindV1,
    pub provider: &'static str,
    pub source_path: &'static str,
    pub fixture_digest: [u8; 32],
    pub evidenced_event: &'static str,
    pub edit: HostCapabilityStateV1,
    pub stop: HostCapabilityStateV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClineFamilyProviderV1 {
    Cline,
    RooCode,
    Kilo,
}

/// Admission recorded by the checked-in Cline-family evidence packet for one
/// exact provider. Only `Verified` is a packaged-route claim; a documented
/// protocol that was never captured locally stays unverified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClineFamilyAdmissionV1 {
    Verified,
    DocumentedUnverified,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClineFamilyEvidenceV1 {
    pub provider: ClineFamilyProviderV1,
    pub registration: HostRegistrationEvidenceV1,
    pub evidence_packet_path: &'static str,
    pub evidence_packet_digest: [u8; 32],
    pub transcript_manifest_path: &'static str,
    pub transcript_manifest_digest: [u8; 32],
    pub admission: ClineFamilyAdmissionV1,
    /// Verbatim reason recorded by the packet for this exact provider. It is
    /// `None` only for a verified route.
    pub unavailable_reason: Option<String>,
    pub edit: HostCapabilityStateV1,
    pub stop: HostCapabilityStateV1,
}

#[derive(Deserialize)]
struct ClineFamilyEvidencePacketV1 {
    providers: Vec<ClineFamilyPacketProviderV1>,
}

#[derive(Deserialize)]
struct ClineFamilyPacketProviderV1 {
    provider: String,
    host_hook_admission: ClineFamilyAdmissionV1,
    #[serde(default)]
    reason: Option<String>,
}

/// Read one provider's admission straight from the root-composed checked-in
/// evidence packet. Family resemblance, an adapter source file, or a shared
/// configuration shape never substitutes for the supplied packet.
pub fn cline_family_evidence_from_embedded_assets(
    assets: &EmbeddedHostIntegrationEvidenceV1,
    provider: ClineFamilyProviderV1,
) -> Option<ClineFamilyEvidenceV1> {
    use HostCapabilityStateV1::{Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::CheckedInEvidenceMissing;

    let packet_provider = match provider {
        ClineFamilyProviderV1::Cline => "cline",
        ClineFamilyProviderV1::RooCode => "roo-code",
        ClineFamilyProviderV1::Kilo => "kilo",
    };
    let packet =
        serde_json::from_slice::<ClineFamilyEvidencePacketV1>(assets.cline_family_evidence_packet)
            .ok()?;
    let entry = packet
        .providers
        .into_iter()
        .find(|entry| entry.provider == packet_provider)?;
    let verified = entry.host_hook_admission == ClineFamilyAdmissionV1::Verified;
    let route_state = if verified {
        Supported
    } else {
        Unavailable(CheckedInEvidenceMissing)
    };
    Some(ClineFamilyEvidenceV1 {
        provider,
        registration: HostRegistrationEvidenceV1 {
            route: HostRegistrationRouteV1::Mcp,
            state: route_state,
            evidence_ref: assets.cline_family_evidence_packet_path,
            starts_analyzer: false,
        },
        evidence_packet_path: assets.cline_family_evidence_packet_path,
        evidence_packet_digest: Sha256::digest(assets.cline_family_evidence_packet).into(),
        transcript_manifest_path: assets.cline_family_transcript_manifest_path,
        transcript_manifest_digest: Sha256::digest(assets.cline_family_transcript_manifest).into(),
        admission: entry.host_hook_admission,
        unavailable_reason: (!verified).then(|| {
            entry
                .reason
                .unwrap_or_else(|| "no_reason_recorded_by_evidence_packet".to_string())
        }),
        edit: route_state,
        stop: route_state,
    })
}

/// Consume root-composed authentic native fixture bytes. A documented but
/// uncaptured declaration remains degraded rather than becoming capture
/// evidence.
pub fn stock_host_native_fixture_evidence_from_embedded_assets(
    assets: &EmbeddedHostIntegrationEvidenceV1,
    host: HostKindV1,
) -> Option<HostNativeFixtureEvidenceV1> {
    use HostCapabilityStateV1::{Degraded, Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::{CheckedInEvidenceMissing, NativeFixtureLimited};

    let unavailable = Unavailable(CheckedInEvidenceMissing);
    let (provider, source_path, evidenced_event, edit, stop) = match host {
        HostKindV1::ClaudeCode => (
            "claude",
            "crates/tracedecay-hooks/fixtures/host_events/claude.json",
            "PostToolUse,Stop",
            Supported,
            Supported,
        ),
        HostKindV1::Codex => (
            "codex",
            "crates/tracedecay-hooks/fixtures/host_events/codex.json",
            "Stop",
            unavailable,
            Supported,
        ),
        HostKindV1::CursorDesktop => (
            "cursor",
            "crates/tracedecay-hooks/fixtures/host_events/cursor.json",
            "afterFileEdit",
            Supported,
            unavailable,
        ),
        HostKindV1::Hermes => (
            "hermes",
            "crates/tracedecay-hooks/fixtures/host_events/hermes.json",
            "post_tool_call,on_session_end",
            Supported,
            Supported,
        ),
        HostKindV1::Kiro => (
            "kiro",
            "crates/tracedecay-hooks/fixtures/host_events/kiro.json",
            "userPromptSubmit",
            unavailable,
            unavailable,
        ),
        HostKindV1::KimiCode => (
            "kimi_code",
            "crates/tracedecay-hooks/fixtures/host_events/kimi-code.json",
            "PostToolUse,Stop",
            Supported,
            Supported,
        ),
        HostKindV1::OpenCode => (
            "opencode",
            "crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json",
            "file.edited,session.idle",
            Degraded(NativeFixtureLimited),
            Degraded(NativeFixtureLimited),
        ),
        HostKindV1::CursorCloud
        | HostKindV1::ClineFamily
        | HostKindV1::Cline
        | HostKindV1::RooCode
        | HostKindV1::Kilo => return None,
    };
    let bytes = assets
        .native_fixtures
        .iter()
        .find(|fixture| fixture.host == host)?
        .bytes;
    Some(HostNativeFixtureEvidenceV1 {
        host,
        provider,
        source_path,
        fixture_digest: Sha256::digest(bytes).into(),
        evidenced_event,
        edit,
        stop,
    })
}

pub fn native_host_edit_stop_conformance_evidence_from_embedded_assets(
    assets: &EmbeddedHostIntegrationEvidenceV1,
) -> Vec<HostNativeFixtureEvidenceV1> {
    [
        HostKindV1::ClaudeCode,
        HostKindV1::Codex,
        HostKindV1::CursorDesktop,
        HostKindV1::Hermes,
        HostKindV1::Kiro,
        HostKindV1::KimiCode,
        HostKindV1::OpenCode,
    ]
    .into_iter()
    .filter_map(|host| stock_host_native_fixture_evidence_from_embedded_assets(assets, host))
    .collect()
}

/// One generated artifact. Contents and credentials never enter the manifest;
/// the content digest identifies bytes compiled into the first-party catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Generated first-party projection for one host/component. It references the
/// one integration/catalog authority and duplicates no workflow semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleManifestV1 {
    pub schema_version: u16,
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub integration_manifest_digest: [u8; 32],
    pub catalog_digest: [u8; 32],
    pub configuration_snapshot_id: String,
    pub effective_behavior_digest: [u8; 32],
    pub resolution_provenance_digest: [u8; 32],
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub artifacts: Vec<HostBundleArtifactV1>,
}

impl HostBundleManifestV1 {
    pub fn validate_structure(&self) -> Result<(), HostBundleError> {
        if self.schema_version != HOST_BUNDLE_SCHEMA_VERSION {
            return Err(HostBundleError::UnsupportedManifestVersion);
        }
        if self.integration_manifest_digest == [0; 32]
            || self.catalog_digest == [0; 32]
            || self.effective_behavior_digest == [0; 32]
            || self.resolution_provenance_digest == [0; 32]
            || self.protocol_min == 0
            || self.protocol_min > self.protocol_max
        {
            return Err(HostBundleError::InvalidManifest);
        }
        validate_identifier(&self.configuration_snapshot_id)?;
        if self.artifacts.is_empty() || self.artifacts.len() > MAX_MANIFEST_ARTIFACTS {
            return Err(HostBundleError::InvalidManifest);
        }
        for (index, artifact) in self.artifacts.iter().enumerate() {
            validate_relative_install_path(Path::new(&artifact.relative_path))?;
            validate_identifier(&artifact.ownership_marker)?;
            if artifact.artifact_digest == [0; 32]
                || self.artifacts[..index]
                    .iter()
                    .any(|existing| existing.relative_path == artifact.relative_path)
            {
                return Err(HostBundleError::InvalidManifest);
            }
        }
        Ok(())
    }

    /// Canonical first-party catalog bytes used for content identity.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, HostBundleError> {
        canonical_json_bytes(&HostBundleCatalogPayloadV1 {
            schema_version: self.schema_version,
            host: self.host,
            component: self.component,
            integration_manifest_digest: self.integration_manifest_digest,
            catalog_digest: self.catalog_digest,
            configuration_snapshot_id: &self.configuration_snapshot_id,
            effective_behavior_digest: self.effective_behavior_digest,
            resolution_provenance_digest: self.resolution_provenance_digest,
            protocol_min: self.protocol_min,
            protocol_max: self.protocol_max,
            artifacts: &self.artifacts,
        })
        .map_err(|_| HostBundleError::CanonicalizationFailed)
    }

    pub fn canonical_digest(&self) -> Result<[u8; 32], HostBundleError> {
        Ok(Sha256::digest(self.canonical_bytes()?).into())
    }
}

#[derive(Serialize)]
struct HostBundleCatalogPayloadV1<'a> {
    schema_version: u16,
    host: HostKindV1,
    component: HostBundleComponentV1,
    integration_manifest_digest: [u8; 32],
    catalog_digest: [u8; 32],
    configuration_snapshot_id: &'a str,
    effective_behavior_digest: [u8; 32],
    resolution_provenance_digest: [u8; 32],
    protocol_min: u16,
    protocol_max: u16,
    artifacts: &'a [HostBundleArtifactV1],
}

/// First-party catalog identity verifier.
pub trait HostBundleVerificationAdapterV1 {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError>;
}

pub fn validate_identifier(value: &str) -> Result<(), HostBundleError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(HostBundleError::InvalidManifest);
    }
    Ok(())
}

/// Lexically validate a manifest path. Absolute paths, parent traversal,
/// platform prefixes, NUL, and ambiguous `.` components are rejected.
pub fn validate_relative_install_path(path: &Path) -> Result<(), HostBundleError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_RELATIVE_PATH_BYTES
        || bytes.contains(&0)
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::ParentDir
                    | Component::CurDir
            )
        })
    {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    Ok(())
}

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

/// Builds a [`HostBundleError::RecoveryRequired`] tagged with the `file:line` of
/// the site that refused to mutate.
///
/// Dozens of journal, receipt, and rollback probes all fail closed with
/// `RecoveryRequired`. Without a per-site tag, an operator staring at "requires
/// recovery before mutation" cannot tell an genuinely interrupted operation from
/// a probe that misread clean state. Always construct the variant through this
/// macro.
#[macro_export]
macro_rules! host_bundle_recovery_required {
    () => {
        $crate::HostBundleError::RecoveryRequired(::core::concat!(
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

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HostBundleError {
    #[error("host capability is unsupported and must not be emulated")]
    UnsupportedCapability,
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
    #[error("bundle ownership marker conflicts or is ambiguous")]
    OwnershipConflict,
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
    #[error("host bundle receipt or operation journal is invalid")]
    ReceiptCorrupted,
    /// An atomic filesystem step failed. The payload names the source site that
    /// observed the failure so the ~100 construction sites stay distinguishable
    /// in user-facing output and bug reports; build it with
    /// [`host_bundle_storage_failure!`] rather than by hand.
    #[error("host bundle atomic filesystem operation failed at {0}")]
    StorageFailure(&'static str),
    /// A mutation refused because an earlier operation looks interrupted. The
    /// payload names the probe that refused, so a false positive on clean state
    /// is distinguishable from a genuine interrupted operation; build it with
    /// [`host_bundle_recovery_required!`] rather than by hand.
    #[error("host bundle interrupted operation requires recovery before mutation (at {0})")]
    RecoveryRequired(&'static str),
    #[error(
        "a backed-up host configuration directory vanished and could not be recreated safely; restore the directory or its parent and retry recovery"
    )]
    RecoveryDirectoryUnavailable,
    #[error(
        "host recovery backup format is unsupported; use the TraceDecay version that created it or restore the host configuration from backup"
    )]
    UnsupportedRecoveryFormat,
    /// Apply observed drift from the confirmed preview. The payload names the
    /// matching layer that rejected, so genuine host drift is distinguishable
    /// from a lifecycle bug; build it with [`host_bundle_stale_preview!`] rather
    /// than by hand.
    #[error("confirmed host lifecycle preview is stale or does not match apply (at {0})")]
    StalePreview(&'static str),
}

/// Bytes obtained from the verified embedded host bundle. They are checked
/// against the cataloged artifact digest before any host path is touched and
/// are never copied into receipts or journals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBundleArtifactContentV1 {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleReceiptArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Durable local receipt. It is a host-install ownership record, not a
/// product/configuration store and contains no artifact content or credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleInstallReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub operation: HostBundleLifecycleOpV1,
    pub manifest_digest: [u8; 32],
    pub artifacts: Vec<HostBundleReceiptArtifactV1>,
    pub rollback_boundary: HostBundleRollbackBoundaryV1,
    #[serde(default)]
    pub rollback_history: Vec<[u8; 16]>,
}

/// Durable, content-free inventory for an operator-requested host-component
/// backup. Artifact bytes live in the lifecycle directory; the receipt binds
/// their exact digests and the manifest needed to restore them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleBackupReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub manifest: HostBundleManifestV1,
    pub source_receipt_digest: [u8; 32],
    pub artifacts: Vec<HostBundleBackupArtifactV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleBackupArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
    pub snapshot_name: String,
}

/// Durable proof that a named backup was restored through the rollback-safe
/// lifecycle writer. The embedded install receipt remains the ownership
/// authority for the restored component.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleRestoreReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub backup_operation_id: [u8; 16],
    pub restored_receipt: HostBundleInstallReceiptV1,
}

/// Durable aggregate commit marker for a complete host component set. The root
/// adapter owns the aggregate transaction; this contract binds its receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostComponentSetReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub operation: HostBundleLifecycleOpV1,
    pub component_manifests: Vec<HostBundleManifestV1>,
    pub component_receipts: Vec<HostBundleInstallReceiptV1>,
    #[serde(default)]
    pub confirmed_plan_digest: Option<[u8; 32]>,
    #[serde(default)]
    pub base_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub current_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub artifact_state_revision: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleRollbackBoundaryV1 {
    Pending,
    Passed,
}

/// Serialized single-component recovery state. Root adapters own opening,
/// writing, and recovering this journal; this crate owns its stable schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleJournalStateV1 {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleJournalEntryV1 {
    pub relative_path: String,
    pub backup_name: Option<String>,
    pub backup_created: bool,
    pub wrote_new: bool,
    pub installed_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleJournalV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub operation: HostBundleLifecycleOpV1,
    pub manifest_digest: [u8; 32],
    pub state: HostBundleJournalStateV1,
    pub previous_receipt: Option<HostBundleInstallReceiptV1>,
    pub entries: Vec<HostBundleJournalEntryV1>,
}

/// Serialized aggregate recovery state. Its filesystem lifecycle remains a
/// root adapter responsibility, so this is intentionally only data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostComponentSetJournalStateV1 {
    Prepared,
    Staged,
    Applied,
    Verified,
    Committed,
    RolledBack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostComponentSetJournalComponentV1 {
    pub manifest: HostBundleManifestV1,
    pub previous_receipt: Option<HostBundleInstallReceiptV1>,
    pub entries: Vec<HostBundleJournalEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostComponentSetJournalV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub operation: HostBundleLifecycleOpV1,
    /// Exact operator authority admitted before any lifecycle mutation.
    ///
    /// Recovery must replay this value rather than manufacturing confirmation.
    #[serde(default)]
    pub explicit_confirmation: bool,
    /// Exact Hermes profile binding admitted with the original request.
    #[serde(default)]
    pub hermes_profile_bindings: u8,
    /// Canonical configuration/runtime preview authority, when the operation
    /// was applied through confirmed preview.
    #[serde(default)]
    pub confirmed_plan_digest: Option<[u8; 32]>,
    #[serde(default)]
    pub base_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub current_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub artifact_state_revision: Option<[u8; 32]>,
    pub state: HostComponentSetJournalStateV1,
    pub registration_staged: bool,
    pub registration_applied: bool,
    pub components: Vec<HostComponentSetJournalComponentV1>,
}

impl HostComponentSetJournalV1 {
    /// Whether the recorded phase and the two registration flags describe a
    /// combination a writer can actually produce.
    ///
    /// The writer raises each flag immediately *before* invoking the
    /// registration hook it names and advances `state` only *after* that hook
    /// returns, so every phase implies the flags of the phases behind it:
    ///
    /// - `Prepared` precedes `registration.apply`, so `registration_applied`
    ///   can never be set there.
    /// - `Staged` and later are reached only after `registration.stage` was
    ///   invoked, which requires `registration_staged`.
    /// - `Applied` and later are reached only after `registration.apply` was
    ///   invoked, which requires `registration_applied`.
    /// - `registration_applied` is never raised without `registration_staged`.
    ///
    /// `RolledBack` is deliberately unconstrained: rollback preserves whichever
    /// flags the failed attempt had reached, so every combination is authentic
    /// there. Recovery must therefore not read the flags as proof that a
    /// rolled-back journal needs no compensation - see
    /// [`Self::registration_compensation_required`].
    #[must_use]
    pub fn registration_flags_match_state(&self) -> bool {
        let staged_required = matches!(
            self.state,
            HostComponentSetJournalStateV1::Staged
                | HostComponentSetJournalStateV1::Applied
                | HostComponentSetJournalStateV1::Verified
                | HostComponentSetJournalStateV1::Committed
        );
        let applied_required = matches!(
            self.state,
            HostComponentSetJournalStateV1::Applied
                | HostComponentSetJournalStateV1::Verified
                | HostComponentSetJournalStateV1::Committed
        );
        if self.registration_applied && !self.registration_staged {
            return false;
        }
        if staged_required && !self.registration_staged {
            return false;
        }
        if applied_required && !self.registration_applied {
            return false;
        }
        !(self.state == HostComponentSetJournalStateV1::Prepared && self.registration_applied)
    }

    /// Whether recovery must attempt host-native registration compensation.
    ///
    /// Only a `Prepared` journal proves registration was never entered: its
    /// flags are raised before the hooks they name, so `Prepared` with both
    /// flags clear is the single state where skipping compensation is sound.
    /// Every other phase - including `RolledBack`, whose flags describe the
    /// interrupted attempt rather than the work still outstanding - must
    /// re-attempt rollback. That re-attempt is already load-bearing today,
    /// because a crash between `rollback_component_set` and journal cleanup
    /// replays the same compensation; the registration adapter contract is
    /// idempotent and no-ops when it finds no staged backup.
    #[must_use]
    pub fn registration_compensation_required(&self) -> bool {
        self.registration_staged
            || self.registration_applied
            || self.state != HostComponentSetJournalStateV1::Prepared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    const CODEX_FIXTURE: &[u8] = br#"{"event":"Stop"}"#;
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
                HostCapabilityUnavailableReasonV1::CheckedInEvidenceMissing
            )
        );
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

    fn component_set_journal(
        state: HostComponentSetJournalStateV1,
        registration_staged: bool,
        registration_applied: bool,
    ) -> HostComponentSetJournalV1 {
        HostComponentSetJournalV1 {
            schema_version: 1,
            operation_id: [7; 16],
            host: HostKindV1::OpenCode,
            operation: HostBundleLifecycleOpV1::Update,
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
            confirmed_plan_digest: None,
            base_registration_revision: None,
            current_registration_revision: None,
            artifact_state_revision: None,
            state,
            registration_staged,
            registration_applied,
            components: Vec::new(),
        }
    }

    /// The flags are raised before the hook they name and the phase advances
    /// after it returns, so each phase implies the flags behind it. `RolledBack`
    /// is the one state that keeps whatever the failed attempt reached.
    #[test]
    fn component_set_journal_phases_imply_their_registration_flags() {
        use HostComponentSetJournalStateV1 as State;

        for (state, staged, applied, representable) in [
            (State::Prepared, false, false, true),
            (State::Prepared, true, false, true),
            (State::Prepared, false, true, false),
            (State::Prepared, true, true, false),
            (State::Staged, true, false, true),
            (State::Staged, true, true, true),
            (State::Staged, false, false, false),
            (State::Applied, true, true, true),
            (State::Applied, true, false, false),
            (State::Verified, true, true, true),
            (State::Verified, false, true, false),
            (State::Committed, true, true, true),
            (State::Committed, false, false, false),
            (State::RolledBack, false, false, true),
            (State::RolledBack, true, false, true),
            (State::RolledBack, true, true, true),
            (State::RolledBack, false, true, false),
        ] {
            assert_eq!(
                component_set_journal(state, staged, applied).registration_flags_match_state(),
                representable,
                "{state:?} staged={staged} applied={applied}"
            );
        }
    }

    /// Only a `Prepared` journal with both flags clear proves registration was
    /// never entered. Every other journal - a rolled-back one above all - still
    /// owes an idempotent compensation attempt.
    #[test]
    fn only_an_untouched_prepared_journal_skips_registration_compensation() {
        use HostComponentSetJournalStateV1 as State;

        assert!(
            !component_set_journal(State::Prepared, false, false)
                .registration_compensation_required()
        );
        for (state, staged, applied) in [
            (State::Prepared, true, false),
            (State::Staged, true, false),
            (State::Applied, true, true),
            (State::Verified, true, true),
            (State::Committed, true, true),
            (State::RolledBack, false, false),
            (State::RolledBack, true, true),
        ] {
            assert!(
                component_set_journal(state, staged, applied).registration_compensation_required(),
                "{state:?} staged={staged} applied={applied}"
            );
        }
    }
}
