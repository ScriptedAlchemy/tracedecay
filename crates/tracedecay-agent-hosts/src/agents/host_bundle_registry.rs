//! Versioned catalog of first-party host components compiled into `TraceDecay`.
//!
//! No external or third-party bundle loading is supported by PR13. SHA-256 digests provide only
//! corruption detection and idempotent content identity.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    HostCapabilityUnavailableReasonV1, TraceDecayProfileBindingV1, canonical_json_bytes,
    host_integration_catalog_v1,
};

use super::host_bundle_v2::{
    HostBundleArtifactContentV1, HostBundleArtifactV1, HostBundleComponentV1, HostBundleError,
    HostBundleManifestV1, HostBundleVerificationAdapterV1, HostComponentSetEntryV1,
    HostComponentSetV1, HostKindV1, require_component_capabilities, validate_identifier,
};

pub const FIRST_PARTY_COMPONENT_CATALOG_VERSION: u64 = 1;
const FIRST_PARTY_COMPONENT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEmbeddedHostBundleV1 {
    pub registry_version: u64,
    pub manifest: HostBundleManifestV1,
    pub contents: Vec<HostBundleArtifactContentV1>,
    manifest_digest: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct VerifiedEmbeddedHostBundleVerifierV1 {
    manifest_digest: [u8; 32],
}

impl HostBundleVerificationAdapterV1 for VerifiedEmbeddedHostBundleVerifierV1 {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
        manifest.validate_structure()?;
        if manifest.canonical_digest()? == self.manifest_digest {
            Ok(())
        } else {
            Err(HostBundleError::CatalogMismatch)
        }
    }
}

impl VerifiedEmbeddedHostBundleV1 {
    pub fn verifier(&self) -> VerifiedEmbeddedHostBundleVerifierV1 {
        VerifiedEmbeddedHostBundleVerifierV1 {
            manifest_digest: self.manifest_digest,
        }
    }
}

/// Canonical, host-specific component set assembled solely from the compiled
/// first-party catalog. One verifier accepts each manifest in this set and
/// rejects manifests from every other catalog projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEmbeddedHostComponentSetV1 {
    pub registry_version: u64,
    pub component_set: HostComponentSetV1,
    manifest_digests: BTreeSet<[u8; 32]>,
}

impl HostBundleVerificationAdapterV1 for VerifiedEmbeddedHostComponentSetV1 {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
        manifest.validate_structure()?;
        self.manifest_digests
            .contains(&manifest.canonical_digest()?)
            .then_some(())
            .ok_or(HostBundleError::CatalogMismatch)
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum HostBundleRegistryError {
    #[error("first-party host component is incompatible")]
    Incompatible,
    #[error("{host:?} has no installable first-party component set: {reason:?}")]
    HostComponentSetUnavailable {
        host: HostKindV1,
        reason: HostCapabilityUnavailableReasonV1,
    },
}

/// Why a stock host has no installable first-party component set. Returning
/// `None` is the only claim of host support: an empty component list is a
/// typed unavailable result, never a successfully empty install.
pub fn unsupported_host_component_set_reason(
    host: HostKindV1,
) -> Option<HostCapabilityUnavailableReasonV1> {
    match host {
        HostKindV1::ClaudeCode
        | HostKindV1::Codex
        | HostKindV1::CursorDesktop
        | HostKindV1::Hermes
        | HostKindV1::Kiro
        | HostKindV1::KimiCode
        | HostKindV1::OpenCode => None,
        // Cursor cloud exposes no host registration API to install into. Its
        // presence in the host enum and capability catalog is not support
        // evidence, so it stays typed unavailable until a real component set
        // is packaged.
        HostKindV1::CursorCloud => {
            Some(HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported)
        }
        // The Cline family has no checked-in evidence admitting a packaged route.
        HostKindV1::ClineFamily | HostKindV1::Cline | HostKindV1::RooCode | HostKindV1::Kilo => {
            Some(HostCapabilityUnavailableReasonV1::CheckedInEvidenceMissing)
        }
    }
}

/// Canonical default install set. Each native MCP registration has one
/// component owner. Kimi's plugin manifest carries its MCP route inside Core;
/// hosts with a separable route use Context MCP. The match is exhaustive so a
/// newly admitted host cannot fall through to a silently empty set.
pub fn default_components(host: HostKindV1) -> Vec<HostBundleComponentV1> {
    match host {
        HostKindV1::ClaudeCode | HostKindV1::Codex => vec![
            HostBundleComponentV1::Core,
            HostBundleComponentV1::ContextMcp,
        ],
        HostKindV1::CursorDesktop | HostKindV1::OpenCode => vec![
            HostBundleComponentV1::Core,
            HostBundleComponentV1::Agent,
            HostBundleComponentV1::ContextMcp,
        ],
        HostKindV1::Hermes | HostKindV1::KimiCode => {
            vec![HostBundleComponentV1::Core]
        }
        // Kiro's production integration owns a supported MCP registration.
        // Its hook route stays degraded and therefore is not part of Core.
        HostKindV1::Kiro => vec![HostBundleComponentV1::ContextMcp],
        HostKindV1::CursorCloud
        | HostKindV1::ClineFamily
        | HostKindV1::Cline
        | HostKindV1::RooCode
        | HostKindV1::Kilo => Vec::new(),
    }
}

/// Build the canonical default set for a host. Unsupported hosts have no
/// fabricated component set and remain on their compatibility migration path.
pub fn verified_embedded_default_host_component_set(
    host: HostKindV1,
    now_unix: u64,
) -> Result<VerifiedEmbeddedHostComponentSetV1, HostBundleRegistryError> {
    if let Some(reason) = unsupported_host_component_set_reason(host) {
        return Err(HostBundleRegistryError::HostComponentSetUnavailable { host, reason });
    }
    verified_embedded_host_component_set(host, &default_components(host), now_unix)
}

/// Project-local canonical set. Host-owned project configuration is handled by
/// the registration adapter; these receipt markers give every selected
/// component an exact project-local ownership path under one transaction.
pub fn verified_embedded_project_host_component_set(
    host: HostKindV1,
    agent_id: &str,
    _now_unix: u64,
) -> Result<VerifiedEmbeddedHostComponentSetV1, HostBundleRegistryError> {
    validate_identifier(agent_id).map_err(|_| HostBundleRegistryError::Incompatible)?;
    let project_components = match host {
        // Roo and Kilo have documented project-local MCP files but no
        // first-party global bundle. Their local transaction therefore owns
        // only the registration delegate plus its project-scoped Core receipt.
        HostKindV1::RooCode | HostKindV1::Kilo => vec![HostBundleComponentV1::Core],
        _ => default_components(host),
    };
    if project_components.is_empty() {
        return Err(HostBundleRegistryError::HostComponentSetUnavailable {
            host,
            reason: unsupported_host_component_set_reason(host)
                .unwrap_or(HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported),
        });
    }
    let catalog = host_integration_catalog_v1();
    let integration_manifest_digest = catalog
        .host_capability_digest(host)
        .map_err(|_| HostBundleRegistryError::Incompatible)?;
    let catalog_digest = catalog
        .canonical_authority_digest()
        .map_err(|_| HostBundleRegistryError::Incompatible)?;
    let mut manifest_digests = BTreeSet::new();
    let mut entries = Vec::with_capacity(project_components.len());
    for component in project_components {
        let component_name = component_name(component);
        let relative_path =
            format!(".tracedecay/host-components/{agent_id}/{component_name}.v1.json");
        let bytes = canonical_json_bytes(&EmbeddedProjectRegistrationMarkerV1 {
            schema_version: FIRST_PARTY_COMPONENT_SCHEMA_VERSION,
            registry_version: FIRST_PARTY_COMPONENT_CATALOG_VERSION,
            agent: agent_id,
            component: component_name,
            scope: "project",
            profile_binding: TraceDecayProfileBindingV1::User,
        })
        .map_err(|_| HostBundleRegistryError::Incompatible)?;
        let artifacts = vec![HostBundleArtifactV1 {
            relative_path: relative_path.clone(),
            artifact_digest: Sha256::digest(&bytes).into(),
            ownership_marker: format!(
                "tracedecay.{}.{component_name}.v1",
                host.descriptor().slug()
            ),
        }];
        let manifest = HostBundleManifestV1 {
            schema_version: FIRST_PARTY_COMPONENT_SCHEMA_VERSION,
            host,
            component,
            integration_manifest_digest,
            catalog_digest,
            configuration_snapshot_id: format!("first-party.{}", crate::PRODUCT_VERSION),
            effective_behavior_digest: embedded_bundle_identity(
                "project_effective_behavior",
                host,
                component,
                &artifacts,
            )?,
            resolution_provenance_digest: embedded_bundle_identity(
                "project_resolution_provenance",
                host,
                component,
                &artifacts,
            )?,
            protocol_min: 1,
            protocol_max: 1,
            artifacts,
        };
        manifest_digests.insert(
            manifest
                .canonical_digest()
                .map_err(|_| HostBundleRegistryError::Incompatible)?,
        );
        entries.push(HostComponentSetEntryV1 {
            manifest,
            contents: vec![HostBundleArtifactContentV1 {
                relative_path,
                bytes,
            }],
        });
    }
    Ok(VerifiedEmbeddedHostComponentSetV1 {
        registry_version: FIRST_PARTY_COMPONENT_CATALOG_VERSION,
        component_set: HostComponentSetV1 {
            host,
            components: entries,
        },
        manifest_digests,
    })
}

#[derive(serde::Serialize)]
struct EmbeddedProjectRegistrationMarkerV1<'a> {
    schema_version: u16,
    registry_version: u64,
    agent: &'a str,
    component: &'a str,
    scope: &'static str,
    profile_binding: TraceDecayProfileBindingV1,
}

/// Build a canonical one-or-more component set. A caller selecting
/// `--component` supplies a one-element slice, while ordinary lifecycle
/// commands pass the host's default set through the same transaction input.
pub fn verified_embedded_host_component_set(
    host: HostKindV1,
    requested_components: &[HostBundleComponentV1],
    now_unix: u64,
) -> Result<VerifiedEmbeddedHostComponentSetV1, HostBundleRegistryError> {
    let tracedecay_bin = super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
    verified_embedded_host_component_set_with_tracedecay_bin(
        host,
        requested_components,
        now_unix,
        &tracedecay_bin,
    )
}

pub fn verified_embedded_host_component_set_with_tracedecay_bin(
    host: HostKindV1,
    requested_components: &[HostBundleComponentV1],
    now_unix: u64,
    tracedecay_bin: &str,
) -> Result<VerifiedEmbeddedHostComponentSetV1, HostBundleRegistryError> {
    if let Some(reason) = unsupported_host_component_set_reason(host) {
        return Err(HostBundleRegistryError::HostComponentSetUnavailable { host, reason });
    }
    if requested_components.is_empty() {
        return Err(HostBundleRegistryError::Incompatible);
    }
    let mut components = requested_components.to_vec();
    components.sort_unstable();
    if components
        .windows(2)
        .any(|components| components[0] == components[1])
    {
        return Err(HostBundleRegistryError::Incompatible);
    }

    let mut paths = BTreeSet::new();
    let mut manifest_digests = BTreeSet::new();
    let mut entries = Vec::with_capacity(components.len());
    for component in components {
        let bundle = verified_embedded_host_bundle_with_tracedecay_bin(
            host,
            component,
            now_unix,
            tracedecay_bin,
        )?;
        if !bundle
            .manifest
            .artifacts
            .iter()
            .all(|artifact| paths.insert(artifact.relative_path.clone()))
        {
            return Err(HostBundleRegistryError::Incompatible);
        }
        manifest_digests.insert(bundle.manifest_digest);
        entries.push(HostComponentSetEntryV1 {
            manifest: bundle.manifest,
            contents: bundle.contents,
        });
    }

    Ok(VerifiedEmbeddedHostComponentSetV1 {
        registry_version: FIRST_PARTY_COMPONENT_CATALOG_VERSION,
        component_set: HostComponentSetV1 {
            host,
            components: entries,
        },
        manifest_digests,
    })
}

pub fn verified_embedded_host_bundle(
    host: HostKindV1,
    component: HostBundleComponentV1,
    now_unix: u64,
) -> Result<VerifiedEmbeddedHostBundleV1, HostBundleRegistryError> {
    let tracedecay_bin = super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
    verified_embedded_host_bundle_with_tracedecay_bin(host, component, now_unix, &tracedecay_bin)
}

fn verified_embedded_host_bundle_with_tracedecay_bin(
    host: HostKindV1,
    component: HostBundleComponentV1,
    _now_unix: u64,
    tracedecay_bin: &str,
) -> Result<VerifiedEmbeddedHostBundleV1, HostBundleRegistryError> {
    require_component_capabilities(host, component)
        .map_err(|_| HostBundleRegistryError::Incompatible)?;
    let host_descriptor = host.descriptor();
    let host_name = host_descriptor.slug();
    let component_name = component_name(component);
    let assets = component_assets(host, component, tracedecay_bin)?;
    let artifacts = assets
        .iter()
        .map(|(path, bytes)| HostBundleArtifactV1 {
            relative_path: path.clone(),
            artifact_digest: Sha256::digest(bytes).into(),
            ownership_marker: format!("tracedecay.{host_name}.{component_name}.v1"),
        })
        .collect::<Vec<_>>();
    let catalog = host_integration_catalog_v1();
    let manifest = HostBundleManifestV1 {
        schema_version: FIRST_PARTY_COMPONENT_SCHEMA_VERSION,
        host,
        component,
        integration_manifest_digest: catalog
            .host_capability_digest(host)
            .map_err(|_| HostBundleRegistryError::Incompatible)?,
        catalog_digest: catalog
            .canonical_authority_digest()
            .map_err(|_| HostBundleRegistryError::Incompatible)?,
        configuration_snapshot_id: format!("first-party.{}", crate::PRODUCT_VERSION),
        effective_behavior_digest: embedded_bundle_identity(
            "effective_behavior",
            host,
            component,
            &artifacts,
        )?,
        resolution_provenance_digest: embedded_bundle_identity(
            "resolution_provenance",
            host,
            component,
            &artifacts,
        )?,
        protocol_min: 1,
        protocol_max: 1,
        artifacts,
    };
    let manifest_digest = manifest
        .canonical_digest()
        .map_err(|_| HostBundleRegistryError::Incompatible)?;
    Ok(VerifiedEmbeddedHostBundleV1 {
        registry_version: FIRST_PARTY_COMPONENT_CATALOG_VERSION,
        manifest,
        contents: assets
            .into_iter()
            .map(|(relative_path, bytes)| HostBundleArtifactContentV1 {
                relative_path,
                bytes,
            })
            .collect(),
        manifest_digest,
    })
}

#[derive(serde::Serialize)]
struct EmbeddedBundleIdentityV1<'a> {
    schema_version: u16,
    registry_version: u64,
    purpose: &'a str,
    host: HostKindV1,
    component: HostBundleComponentV1,
    artifacts: Vec<HostBundleArtifactV1>,
}

fn embedded_bundle_identity(
    purpose: &str,
    host: HostKindV1,
    component: HostBundleComponentV1,
    artifacts: &[HostBundleArtifactV1],
) -> Result<[u8; 32], HostBundleRegistryError> {
    let mut artifacts = artifacts.to_vec();
    artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    canonical_json_bytes(&EmbeddedBundleIdentityV1 {
        schema_version: FIRST_PARTY_COMPONENT_SCHEMA_VERSION,
        registry_version: FIRST_PARTY_COMPONENT_CATALOG_VERSION,
        purpose,
        host,
        component,
        artifacts,
    })
    .map(|bytes| Sha256::digest(bytes).into())
    .map_err(|_| HostBundleRegistryError::Incompatible)
}

fn component_assets(
    host: HostKindV1,
    component: HostBundleComponentV1,
    tracedecay_bin: &str,
) -> Result<Vec<(String, Vec<u8>)>, HostBundleRegistryError> {
    // The managed Hermes plugin package is also written by the legacy installer
    // that the compatibility registration adapter re-runs during apply, and the
    // component-set transaction verifies installed digests afterwards. Deploy
    // the installer's own rendered inventory (same bin resolution as
    // `InstallContext::tracedecay_bin`) so both writers produce identical
    // bytes. Rendering the inventory with the `"__TRACEDECAY_BIN__"` sentinel
    // and then substituting it through `render_compiled_asset` resolved the
    // binary from `std::env::current_exe()` instead, which disagrees with
    // `which_tracedecay()` whenever the running binary lives outside the
    // installed path (for example `./target/release/tracedecay reinstall`) and
    // corrupted every Hermes transaction.
    if (host, component) == (HostKindV1::Hermes, HostBundleComponentV1::Core) {
        let files = super::hermes::rendered_plugin_files(tracedecay_bin)
            .map_err(|_| HostBundleRegistryError::Incompatible)?;
        return Ok(files
            .into_iter()
            .map(|(relative, body)| {
                (
                    format!(".hermes/plugins/tracedecay/{relative}"),
                    body.into_bytes(),
                )
            })
            .collect());
    }

    // The Cursor plugin directory is also written by the legacy installer the
    // compatibility registration adapter re-runs during apply, and the
    // component-set transaction verifies installed digests afterwards. Use the
    // installer's own rendered inventory (same bin resolution as
    // `InstallContext::tracedecay_bin`) so both writers produce identical
    // bytes. The native-extension Agent component keeps the compiled-asset
    // path below: the legacy installer never writes it.
    if host == HostKindV1::CursorDesktop
        && matches!(
            component,
            HostBundleComponentV1::Core
                | HostBundleComponentV1::ContextMcp
                | HostBundleComponentV1::OperatorMcp
        )
    {
        let files = super::cursor::rendered_plugin_files(tracedecay_bin)
            .map_err(|_| HostBundleRegistryError::Incompatible)?;
        let mcp_only = component != HostBundleComponentV1::Core;
        return Ok(files
            .into_iter()
            .filter(|(relative, _)| (*relative == "mcp.json") == mcp_only)
            .map(|(relative, body)| {
                (
                    format!(".cursor/plugins/local/tracedecay/{relative}"),
                    body.into_bytes(),
                )
            })
            .collect());
    }

    // The Codex registration probe requires the managed lifecycle hooks in
    // the deployed `hooks/hooks.json`; the raw template is an empty scaffold
    // rendered only at install time, so deploy the installer's rendered
    // global inventory here too.
    if host == HostKindV1::Codex
        && matches!(
            component,
            HostBundleComponentV1::Core
                | HostBundleComponentV1::ContextMcp
                | HostBundleComponentV1::OperatorMcp
        )
    {
        let files = super::codex::rendered_global_plugin_files(tracedecay_bin)
            .map_err(|_| HostBundleRegistryError::Incompatible)?;
        let mcp_only = component != HostBundleComponentV1::Core;
        return Ok(files
            .into_iter()
            .filter(|(relative, _)| (*relative == ".mcp.json") == mcp_only)
            .map(|(relative, body)| {
                (
                    format!(".codex/plugins/tracedecay/{relative}"),
                    body.into_bytes(),
                )
            })
            .collect());
    }

    // The Claude marketplace deploy dir is also rewritten by the legacy
    // installer during the compatibility apply, and installed digests are
    // verified afterwards — deploy the installer's rendered inventory so both
    // writers produce identical bytes.
    if host == HostKindV1::ClaudeCode
        && matches!(
            component,
            HostBundleComponentV1::Core
                | HostBundleComponentV1::ContextMcp
                | HostBundleComponentV1::OperatorMcp
        )
    {
        let files = super::claude::rendered_plugin_files(tracedecay_bin)
            .map_err(|_| HostBundleRegistryError::Incompatible)?;
        let mcp_only = component != HostBundleComponentV1::Core;
        return Ok(files
            .into_iter()
            .filter(|(relative, _)| (*relative == ".mcp.json") == mcp_only)
            .map(|(relative, body)| {
                (
                    format!(".claude/plugins/marketplaces/tracedecay/{relative}"),
                    body.into_bytes(),
                )
            })
            .collect());
    }

    // Kimi's Core bundle owns its manifest-declared hooks and MCP route as one
    // native plugin. Render the complete managed inventory with the installed
    // binary path; companion MCP components would duplicate that ownership.
    if (host, component) == (HostKindV1::KimiCode, HostBundleComponentV1::Core) {
        let files = super::kimi::rendered_plugin_files(tracedecay_bin)
            .map_err(|_| HostBundleRegistryError::Incompatible)?;
        return Ok(files
            .into_iter()
            .map(|(relative, body)| {
                (
                    format!(".kimi-code/plugins/managed/tracedecay/{relative}"),
                    body.into_bytes(),
                )
            })
            .collect());
    }

    // Render OpenCode Core with the installed binary path. The generic renderer
    // uses `std::env::current_exe()`, which can differ from the installed path
    // during an in-tree reinstall. Context MCP and Agent remain disjoint
    // compiled assets.
    if (host, component) == (HostKindV1::OpenCode, HostBundleComponentV1::Core) {
        let files = super::opencode::rendered_plugin_files(tracedecay_bin)
            .map_err(|_| HostBundleRegistryError::Incompatible)?;
        return Ok(files
            .into_iter()
            .map(|(relative, body)| (format!(".config/opencode/{relative}"), body.into_bytes()))
            .collect());
    }

    let (prefix, files) = match (host, component) {
        (HostKindV1::CursorDesktop, HostBundleComponentV1::Agent) => (
            ".cursor/extensions/tracedecay.cursor-native-0.0.0",
            super::plugin_bundle::cursor_native_extension_files(),
        ),
        // Kiro's `settings/mcp.json` is a shared user document: the native
        // registration adapter merges the TraceDecay server into it (preserving
        // third-party entries) and re-reads it for this component's confirmed
        // registration revision. A managed artifact must therefore never claim
        // that path. Owning it here made the transaction's own artifact write
        // land between preview confirmation and `registration.apply`, so the
        // adapter's revision recheck read TraceDecay's own bytes as third-party
        // drift and every apply rolled back with `StalePreview`. Own a
        // descriptor under `.kiro/tracedecay` instead, exactly like Kiro Core.
        (HostKindV1::Kiro, HostBundleComponentV1::ContextMcp) => (
            ".kiro/tracedecay",
            vec![(
                "context-mcp.json",
                r#"{"host":"kiro","registration":"settings/mcp.json","route":"mcp","server":{"command":"__TRACEDECAY_BIN__","args":["serve"]}}"#,
            )],
        ),
        (HostKindV1::Kiro, HostBundleComponentV1::Core) => (
            ".kiro/tracedecay",
            vec![(
                "component.json",
                r#"{"host":"kiro","registration":"settings/mcp.json+agents/tracedecay.json","route":"hook+mcp","native_events":"userPromptSubmit,preToolUse,postToolUse","version_disposition":"session_workspace_prompt_boundaries_only"}"#,
            )],
        ),
        (HostKindV1::Cline, HostBundleComponentV1::Core) => (
            ".cline/data/settings/tracedecay",
            vec![(
                "component.json",
                r#"{"host":"cline","registration":"settings/cline_mcp_settings.json","route":"mcp","version_disposition":"current_cli_and_ide_data_dir;legacy_vscode_path_migration_only"}"#,
            )],
        ),
        (HostKindV1::RooCode, HostBundleComponentV1::Core) => (
            ".roo/tracedecay",
            vec![(
                "component.json",
                r#"{"host":"roo-code","registration":"settings/cline_mcp_settings.json","route":"mcp","version_disposition":"documented_roo_extension_storage_and_project_.roo/mcp.json"}"#,
            )],
        ),
        (HostKindV1::Kilo, HostBundleComponentV1::Core) => (
            ".config/kilo/tracedecay",
            vec![(
                "component.json",
                r#"{"host":"kilo","registration":"kilo.jsonc","route":"mcp","version_disposition":"current_kilo_cli_jsonc_and_project_kilo.json"}"#,
            )],
        ),
        (HostKindV1::OpenCode, HostBundleComponentV1::Agent) => (
            ".config/opencode",
            super::plugin_bundle::opencode_agent_files(),
        ),
        (HostKindV1::OpenCode, HostBundleComponentV1::ContextMcp) => (
            ".config/opencode",
            vec![
                (
                    "plugins/tracedecay-mcp.ts",
                    include_str!("../../../../plugin/opencode/tracedecay-mcp.ts"),
                ),
                (
                    "tracedecay/opencode.registration.json",
                    include_str!("../../../../plugin/opencode/opencode.registration.json"),
                ),
            ],
        ),
        _ => return Err(HostBundleRegistryError::Incompatible),
    };
    Ok(files
        .into_iter()
        .map(|(path, body)| {
            (
                format!("{prefix}/{path}"),
                render_compiled_asset(body, tracedecay_bin).into_bytes(),
            )
        })
        .collect())
}

fn render_compiled_asset(body: &str, tracedecay_bin: &str) -> String {
    let encoded =
        serde_json::to_string(tracedecay_bin).unwrap_or_else(|_| "\"tracedecay\"".to_string());
    let sync = serde_json::to_string(&super::hook_command(tracedecay_bin, "hook-kimi-event"))
        .unwrap_or_else(|_| encoded.clone());
    let stop = serde_json::to_string(&super::hook_command(tracedecay_bin, "hook-kimi-event"))
        .unwrap_or_else(|_| encoded.clone());
    body.replace("\"__TRACEDECAY_BIN__\"", &encoded)
        .replace("\"__TRACEDECAY_SYNC__\"", &sync)
        .replace("\"__TRACEDECAY_STOP__\"", &stop)
}

fn component_name(component: HostBundleComponentV1) -> &'static str {
    match component {
        HostBundleComponentV1::Core => "core",
        HostBundleComponentV1::Agent => "agent",
        HostBundleComponentV1::ContextMcp => "context-mcp",
        HostBundleComponentV1::OperatorMcp => "operator-mcp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_native_components_have_real_assets() {
        for (host, component, expected) in [
            (
                HostKindV1::CursorDesktop,
                HostBundleComponentV1::Agent,
                ".cursor/extensions/tracedecay.cursor-native-0.0.0/dist/extension.js",
            ),
            (
                HostKindV1::KimiCode,
                HostBundleComponentV1::Core,
                ".kimi-code/",
            ),
            (
                HostKindV1::Hermes,
                HostBundleComponentV1::Core,
                ".hermes/plugins/tracedecay/plugin.yaml",
            ),
            (
                HostKindV1::OpenCode,
                HostBundleComponentV1::Core,
                "plugins/tracedecay.ts",
            ),
            (
                HostKindV1::OpenCode,
                HostBundleComponentV1::Agent,
                ".config/opencode/skills/",
            ),
            (
                HostKindV1::OpenCode,
                HostBundleComponentV1::ContextMcp,
                "tracedecay/opencode.registration.json",
            ),
        ] {
            let bundle = verified_embedded_host_bundle(host, component, 0).unwrap();
            assert!(!bundle.contents.is_empty());
            assert!(
                bundle
                    .contents
                    .iter()
                    .any(|asset| asset.relative_path.contains(expected))
            );
            assert!(
                bundle
                    .contents
                    .iter()
                    .all(|asset| !asset.relative_path.contains("host-components"))
            );
        }
    }

    #[test]
    fn cursor_native_diagnostics_render_the_resolved_installed_binary() {
        let installed = "/opt/tracedecay-distinct/bin/tracedecay";
        let assets = component_assets(
            HostKindV1::CursorDesktop,
            HostBundleComponentV1::Agent,
            installed,
        )
        .unwrap();
        let extension = assets
            .iter()
            .find(|(path, _)| path.ends_with("/dist/extension.js"))
            .expect("Cursor native diagnostics extension is packaged");
        let body = String::from_utf8(extension.1.clone()).unwrap();

        assert!(body.contains(&serde_json::to_string(installed).unwrap()));
        assert!(!body.contains("__TRACEDECAY_BIN__"));
        if let Ok(running) = std::env::current_exe() {
            let running = running.to_string_lossy();
            assert!(
                !body.contains(running.as_ref()),
                "compiled Cursor assets must not embed the running test executable"
            );
        }
    }

    #[test]
    fn opencode_context_mcp_renders_the_resolved_installed_binary() {
        let installed = "/opt/tracedecay-distinct/bin/tracedecay";
        let assets = component_assets(
            HostKindV1::OpenCode,
            HostBundleComponentV1::ContextMcp,
            installed,
        )
        .unwrap();
        let registration = assets
            .iter()
            .find(|(path, _)| path.ends_with("opencode.registration.json"))
            .expect("OpenCode Context MCP registration is packaged");
        let body = String::from_utf8(registration.1.clone()).unwrap();

        assert!(body.contains(&serde_json::to_string(installed).unwrap()));
        assert!(!body.contains("__TRACEDECAY_BIN__"));
        if let Ok(running) = std::env::current_exe() {
            let running = running.to_string_lossy();
            assert!(
                !body.contains(running.as_ref()),
                "compiled OpenCode assets must not embed the running test executable"
            );
        }
    }

    #[test]
    fn kimi_component_renderer_eliminates_every_hook_v2_placeholder() {
        let bundle =
            verified_embedded_host_bundle(HostKindV1::KimiCode, HostBundleComponentV1::Core, 0)
                .unwrap();
        for content in bundle.contents {
            let body = String::from_utf8(content.bytes).unwrap();
            for placeholder in [
                "__TRACEDECAY_BIN__",
                "__TRACEDECAY_SYNC__",
                "__TRACEDECAY_STOP__",
            ] {
                assert!(
                    !body.contains(placeholder),
                    "{} retained {placeholder}",
                    content.relative_path
                );
            }
        }
    }

    /// The compatibility registration adapter re-runs the legacy Kimi
    /// installer during apply and the component-set transaction verifies
    /// installed digests afterwards, so the two writers must agree byte for
    /// byte. Rendering the raw template here instead would leave the manifest
    /// version unstamped and fail every install with `ArtifactContentMismatch`.
    #[test]
    fn kimi_catalog_assets_match_the_legacy_installer_rendering() {
        let bin = super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let rendered = super::super::kimi::rendered_plugin_files(&bin).unwrap();
        let bundle = verified_embedded_host_bundle_with_tracedecay_bin(
            HostKindV1::KimiCode,
            HostBundleComponentV1::Core,
            0,
            &bin,
        )
        .unwrap();

        assert_eq!(bundle.contents.len(), rendered.len());
        for (relative, body) in rendered {
            let path = format!(".kimi-code/plugins/managed/tracedecay/{relative}");
            let content = bundle
                .contents
                .iter()
                .find(|content| content.relative_path == path)
                .unwrap_or_else(|| panic!("catalog is missing the deployed path {path}"));
            assert_eq!(
                content.bytes,
                body.into_bytes(),
                "{path} must match the legacy installer rendering"
            );
        }
    }

    /// Safety premise for per-host component-set journal isolation in
    /// `host_bundle_v2`: a transaction for host X may proceed while host Y's
    /// journal awaits recovery only because the two hosts never mutate the same
    /// artifact path. Pin that here so a future host which shares a deployed
    /// path fails this test instead of silently widening the blast radius of an
    /// interrupted install.
    #[test]
    fn first_party_host_artifact_paths_are_disjoint_across_hosts() {
        let mut owner_by_path: std::collections::BTreeMap<String, HostKindV1> =
            std::collections::BTreeMap::new();
        for host in HostKindV1::ALL {
            for component in default_components(host) {
                let bundle = verified_embedded_host_bundle(host, component, 0).unwrap();
                for artifact in &bundle.manifest.artifacts {
                    if let Some(other) = owner_by_path.insert(artifact.relative_path.clone(), host)
                        && other != host
                    {
                        panic!(
                            "{} is deployed by both {other:?} and {host:?}; per-host lifecycle \
                             journal isolation assumes disjoint artifact path spaces",
                            artifact.relative_path
                        );
                    }
                }
            }
        }
        assert!(!owner_by_path.is_empty());
    }

    #[test]
    fn opencode_catalog_assets_match_the_legacy_installer_rendering() {
        let bin = super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let rendered = super::super::opencode::rendered_plugin_files(&bin).unwrap();
        let bundle = verified_embedded_host_bundle_with_tracedecay_bin(
            HostKindV1::OpenCode,
            HostBundleComponentV1::Core,
            0,
            &bin,
        )
        .unwrap();

        assert_eq!(bundle.contents.len(), rendered.len());
        for (relative, body) in rendered {
            let path = format!(".config/opencode/{relative}");
            let content = bundle
                .contents
                .iter()
                .find(|content| content.relative_path == path)
                .unwrap_or_else(|| panic!("catalog is missing the deployed path {path}"));
            assert_eq!(
                content.bytes,
                body.into_bytes(),
                "{path} must match the legacy installer rendering"
            );
        }
    }

    /// Both writers must agree even when the running binary is not the
    /// installed one — `./target/release/tracedecay reinstall` is exactly the
    /// case that corrupted the `OpenCode` transaction and wedged the shared
    /// component-set journal.
    #[test]
    fn opencode_core_assets_do_not_depend_on_the_running_executable_path() {
        let running = std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let installed =
            super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let assets = component_assets(
            HostKindV1::OpenCode,
            HostBundleComponentV1::Core,
            &installed,
        )
        .unwrap();
        assert_eq!(assets.len(), 1);
        let body = String::from_utf8(assets[0].1.clone()).unwrap();
        assert!(
            body.contains(&serde_json::to_string(&installed).unwrap()),
            "the catalog must render the installer's resolved binary"
        );
        if running != installed && !running.is_empty() {
            assert!(
                !body.contains(&running),
                "the catalog must not render the running executable path"
            );
        }
    }

    #[test]
    fn hermes_catalog_assets_match_the_legacy_installer_rendering() {
        let bin = super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let rendered = super::super::hermes::rendered_plugin_files(&bin).unwrap();
        let bundle = verified_embedded_host_bundle_with_tracedecay_bin(
            HostKindV1::Hermes,
            HostBundleComponentV1::Core,
            0,
            &bin,
        )
        .unwrap();

        assert_eq!(bundle.contents.len(), rendered.len());
        for (relative, body) in rendered {
            let path = format!(".hermes/plugins/tracedecay/{relative}");
            let content = bundle
                .contents
                .iter()
                .find(|content| content.relative_path == path)
                .unwrap_or_else(|| panic!("catalog is missing the deployed path {path}"));
            assert_eq!(
                content.bytes,
                body.into_bytes(),
                "{path} must match the legacy installer rendering"
            );
        }
    }

    /// Both writers must agree even when the running binary is not the
    /// installed one — `./target/release/tracedecay reinstall` is exactly the
    /// case that corrupted the Hermes transaction and left a pending
    /// `component-set-journal.hermes.v1.json` behind.
    #[test]
    fn hermes_core_assets_do_not_depend_on_the_running_executable_path() {
        let running = std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let installed =
            super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let assets =
            component_assets(HostKindV1::Hermes, HostBundleComponentV1::Core, &installed).unwrap();
        let tools = assets
            .iter()
            .find(|(path, _)| path == ".hermes/plugins/tracedecay/tools.py")
            .expect("the Hermes core component deploys tools.py");
        let body = String::from_utf8(tools.1.clone()).unwrap();
        assert!(
            body.contains(&serde_json::to_string(&installed).unwrap()),
            "the catalog must render the installer's resolved binary"
        );
        assert!(
            !body.contains("__TRACEDECAY_BIN__"),
            "the catalog must not leave the bin sentinel unrendered"
        );
        if running != installed && !running.is_empty() {
            assert!(
                !body.contains(&running),
                "the catalog must not render the running executable path"
            );
        }
    }

    #[test]
    fn embedded_manifests_pin_catalog_capabilities_and_real_artifact_content() {
        let catalog = host_integration_catalog_v1();
        let marker: [u8; 32] = Sha256::digest(b"tracedecay.first-party-host-components.v1").into();
        let expected_catalog_digest = catalog.canonical_authority_digest().unwrap();
        for host in HostKindV1::ALL {
            for component in default_components(host) {
                let bundle = verified_embedded_host_bundle(host, component, 0).unwrap();
                assert_eq!(bundle.manifest.catalog_digest, expected_catalog_digest);
                assert_eq!(
                    bundle.manifest.integration_manifest_digest,
                    catalog.host_capability_digest(host).unwrap()
                );
                for artifact in &bundle.manifest.artifacts {
                    let content = bundle
                        .contents
                        .iter()
                        .find(|content| content.relative_path == artifact.relative_path)
                        .expect("every manifest artifact has compiled content");
                    assert_eq!(
                        artifact.artifact_digest,
                        <[u8; 32]>::from(Sha256::digest(&content.bytes))
                    );
                }
                let identities = [
                    bundle.manifest.integration_manifest_digest,
                    bundle.manifest.catalog_digest,
                    bundle.manifest.effective_behavior_digest,
                    bundle.manifest.resolution_provenance_digest,
                ];
                assert!(identities.iter().all(|identity| *identity != marker));
                assert_eq!(
                    identities.into_iter().collect::<BTreeSet<_>>().len(),
                    identities.len(),
                    "catalog, capability, behavior, and provenance digests stay distinct"
                );
            }
        }
    }

    #[test]
    fn default_component_sets_are_supported_and_path_disjoint() {
        for host in [
            HostKindV1::ClaudeCode,
            HostKindV1::CursorDesktop,
            HostKindV1::Codex,
            HostKindV1::Hermes,
            HostKindV1::Kiro,
            HostKindV1::KimiCode,
            HostKindV1::OpenCode,
        ] {
            let bundles = default_components(host)
                .into_iter()
                .map(|component| verified_embedded_host_bundle(host, component, 0).unwrap())
                .collect::<Vec<_>>();
            let mut paths = std::collections::BTreeSet::new();
            for bundle in bundles {
                for artifact in bundle.manifest.artifacts {
                    assert!(paths.insert(artifact.relative_path));
                }
            }
        }
    }

    #[test]
    fn canonical_component_set_uses_one_verifier_for_default_and_explicit_selection() {
        let default_set = verified_embedded_default_host_component_set(HostKindV1::OpenCode, 0)
            .expect("OpenCode has a compiled default set");
        assert_eq!(
            default_set
                .component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>(),
            default_components(HostKindV1::OpenCode)
        );
        for component in &default_set.component_set.components {
            default_set
                .verify_manifest(&component.manifest)
                .expect("set verifier accepts every compiled component");
        }

        let single = verified_embedded_host_component_set(
            HostKindV1::OpenCode,
            &[HostBundleComponentV1::ContextMcp],
            0,
        )
        .expect("explicit component uses the same set transaction input");
        assert_eq!(single.component_set.components.len(), 1);
        assert_eq!(
            single.component_set.components[0].manifest.component,
            HostBundleComponentV1::ContextMcp
        );
    }

    #[test]
    fn shared_mcp_manifests_have_one_canonical_component_owner() {
        for (host, unsupported) in [
            (HostKindV1::OpenCode, HostBundleComponentV1::OperatorMcp),
            (HostKindV1::KimiCode, HostBundleComponentV1::ContextMcp),
            (HostKindV1::KimiCode, HostBundleComponentV1::OperatorMcp),
        ] {
            assert_eq!(
                verified_embedded_host_bundle(host, unsupported, 0),
                Err(HostBundleRegistryError::Incompatible)
            );
        }
    }

    #[test]
    fn cline_roo_and_kilo_refuse_components_without_native_evidence() {
        for host in [HostKindV1::Cline, HostKindV1::RooCode, HostKindV1::Kilo] {
            assert!(default_components(host).is_empty());
            assert_eq!(
                verified_embedded_host_component_set(host, &[HostBundleComponentV1::Core], 0),
                Err(HostBundleRegistryError::HostComponentSetUnavailable {
                    host,
                    reason: HostCapabilityUnavailableReasonV1::CheckedInEvidenceMissing,
                })
            );
        }
    }

    #[test]
    fn kiro_packages_only_its_supported_mcp_registration() {
        assert_eq!(
            unsupported_host_component_set_reason(HostKindV1::Kiro),
            None
        );
        assert_eq!(
            default_components(HostKindV1::Kiro),
            vec![HostBundleComponentV1::ContextMcp]
        );
        assert_eq!(
            tracedecay_domain::integration::host_descriptor_v1(HostKindV1::Kiro).components(),
            &[tracedecay_domain::integration::HostComponentV1::ContextMcp]
        );
        let bundle = verified_embedded_host_bundle_with_tracedecay_bin(
            HostKindV1::Kiro,
            HostBundleComponentV1::ContextMcp,
            0,
            "/opt/tracedecay-distinct/bin/tracedecay",
        )
        .unwrap();
        assert_eq!(bundle.contents.len(), 1);
        // The managed artifact is a TraceDecay-owned descriptor, never Kiro's
        // shared `settings/mcp.json`: that document belongs to the native
        // registration adapter, which merges into it and hashes it for the
        // confirmed registration revision.
        assert_eq!(
            bundle.contents[0].relative_path,
            ".kiro/tracedecay/context-mcp.json"
        );
        let descriptor: serde_json::Value =
            serde_json::from_slice(&bundle.contents[0].bytes).unwrap();
        assert_eq!(descriptor["registration"], "settings/mcp.json");
        assert_eq!(
            descriptor["server"]["command"],
            "/opt/tracedecay-distinct/bin/tracedecay"
        );
        assert_eq!(
            verified_embedded_host_component_set(
                HostKindV1::Kiro,
                &[HostBundleComponentV1::Core],
                0
            ),
            Err(HostBundleRegistryError::Incompatible)
        );
    }

    #[test]
    fn cursor_cloud_reports_a_typed_unavailable_set_instead_of_an_empty_default() {
        assert_eq!(
            unsupported_host_component_set_reason(HostKindV1::CursorCloud),
            Some(HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported)
        );
        for outcome in [
            verified_embedded_default_host_component_set(HostKindV1::CursorCloud, 0),
            verified_embedded_host_component_set(
                HostKindV1::CursorCloud,
                &[HostBundleComponentV1::Core],
                0,
            ),
            verified_embedded_project_host_component_set(HostKindV1::CursorCloud, "cursor", 0),
        ] {
            assert_eq!(
                outcome,
                Err(HostBundleRegistryError::HostComponentSetUnavailable {
                    host: HostKindV1::CursorCloud,
                    reason: HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported,
                }),
                "cursor cloud must never fall through to an installable empty set"
            );
        }
    }

    #[test]
    fn every_host_with_a_default_set_is_reported_supported() {
        for host in HostKindV1::ALL {
            assert_eq!(
                default_components(host).is_empty(),
                unsupported_host_component_set_reason(host).is_some(),
                "{host:?} must not disagree between its default set and its typed availability"
            );
        }
    }

    #[test]
    fn opencode_component_set_carries_lsp_policy_and_hook_guidance_delivery() {
        let component_set =
            verified_embedded_default_host_component_set(HostKindV1::OpenCode, 0).unwrap();
        let registration = component_set
            .component_set
            .components
            .iter()
            .flat_map(|component| &component.contents)
            .find(|asset| asset.relative_path.ends_with("opencode.registration.json"))
            .expect("OpenCode set includes the registration projection");
        let registration: serde_json::Value = serde_json::from_slice(&registration.bytes).unwrap();
        assert_eq!(
            registration
                .pointer("/lsp/tracedecay/initialization/tracedecay/competingAnalyzerPolicy")
                .and_then(serde_json::Value::as_str),
            Some("preflight-and-refuse-ambiguous")
        );

        let plugin = component_set
            .component_set
            .components
            .iter()
            .flat_map(|component| &component.contents)
            .find(|asset| asset.relative_path.ends_with("plugins/tracedecay.ts"))
            .map(|asset| String::from_utf8(asset.bytes.clone()).unwrap())
            .expect("OpenCode set includes Hook V2 plugin");
        assert!(plugin.contains("stdout: \"pipe\""));
        assert!(plugin.contains("client.tui.showToast"));
        assert!(!plugin.contains("stdout: \"ignore\""));
    }

    #[test]
    fn kimi_and_opencode_rendered_bundles_queue_native_scout_lifecycle_events() {
        let kimi = verified_embedded_default_host_component_set(HostKindV1::KimiCode, 0).unwrap();
        let manifest = kimi
            .component_set
            .components
            .iter()
            .flat_map(|component| &component.contents)
            .find(|asset| asset.relative_path.ends_with(".kimi-plugin/plugin.json"))
            .map(|asset| String::from_utf8(asset.bytes.clone()).unwrap())
            .unwrap();
        assert!(manifest.contains("hook-kimi-event"));
        assert!(manifest.contains("\"PostToolUse\""));

        let opencode =
            verified_embedded_default_host_component_set(HostKindV1::OpenCode, 0).unwrap();
        let plugin = opencode
            .component_set
            .components
            .iter()
            .flat_map(|component| &component.contents)
            .find(|asset| asset.relative_path.ends_with("plugins/tracedecay.ts"))
            .map(|asset| String::from_utf8(asset.bytes.clone()).unwrap())
            .unwrap();
        assert!(plugin.contains("await dispatch(\"hook-opencode-tool-after\", { input, output })"));
        assert!(plugin.contains("await dispatch(\"hook-opencode-event\", event)"));
        assert!(plugin.contains("event.type === \"lsp.updated\""));
    }

    #[test]
    fn project_local_sets_have_only_project_scoped_receipt_paths() {
        let component_set =
            verified_embedded_project_host_component_set(HostKindV1::OpenCode, "opencode", 0)
                .unwrap();
        assert!(
            component_set
                .component_set
                .components
                .iter()
                .all(|component| {
                    component.manifest.artifacts.iter().all(|artifact| {
                        artifact
                            .relative_path
                            .starts_with(".tracedecay/host-components/opencode/")
                    })
                })
        );
        for component in &component_set.component_set.components {
            component_set.verify_manifest(&component.manifest).unwrap();
            let marker: serde_json::Value =
                serde_json::from_slice(&component.contents[0].bytes).unwrap();
            assert_eq!(marker["schema_version"], 1);
            assert_eq!(marker["registry_version"], 1);
            assert_eq!(marker["profile_binding"], "user");
            assert_eq!(
                component.manifest.effective_behavior_digest,
                embedded_bundle_identity(
                    "project_effective_behavior",
                    component.manifest.host,
                    component.manifest.component,
                    &component.manifest.artifacts,
                )
                .unwrap()
            );
            assert_eq!(
                component.manifest.resolution_provenance_digest,
                embedded_bundle_identity(
                    "project_resolution_provenance",
                    component.manifest.host,
                    component.manifest.component,
                    &component.manifest.artifacts,
                )
                .unwrap()
            );
        }
    }

    #[test]
    fn project_local_sets_reject_unsafe_agent_ids() {
        for agent_id in ["", "../opencode", "opencode/other", "opencode\""] {
            assert_eq!(
                verified_embedded_project_host_component_set(HostKindV1::OpenCode, agent_id, 0),
                Err(HostBundleRegistryError::Incompatible)
            );
        }
    }
}
