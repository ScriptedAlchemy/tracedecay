//! Host-neutral integration catalog contracts.
//!
//! Production host-surface capability authority is the stock
//! [`HostCapabilityStateV1`] matrix. Observation-host fixture admission
//! taxonomies live beside host-event fixtures and are not a second catalog
//! admission authority. Host artifact rendering, lifecycle operations, remote
//! transport, and host-local durable state belong to later delivery slices.

mod descriptor;

pub use descriptor::*;

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{CapabilityId, DomainError, canonical_json_bytes};

pub const HOST_INTEGRATION_CATALOG_SCHEMA_VERSION_V1: u16 = 1;
const OBSERVATION_CAPTURE_CAPABILITY_ID: &str = "capability.integration.observation.capture";

/// Canonical stock host surfaces shared by catalog, packaging, delivery, and
/// conformance consumers. A host surface is not itself evidence that the
/// native observation-capture capability is fixture-backed.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum HostKindV1 {
    ClaudeCode,
    CursorDesktop,
    CursorCloud,
    Codex,
    Hermes,
    Kiro,
    ClineFamily,
    Cline,
    RooCode,
    Kilo,
    KimiCode,
    OpenCode,
    Gemini,
}

impl HostKindV1 {
    pub const ALL: [Self; 13] = [
        Self::ClaudeCode,
        Self::CursorDesktop,
        Self::CursorCloud,
        Self::Codex,
        Self::Hermes,
        Self::Kiro,
        Self::ClineFamily,
        Self::Cline,
        Self::RooCode,
        Self::Kilo,
        Self::KimiCode,
        Self::OpenCode,
        Self::Gemini,
    ];

    /// Project a stock host surface into the bounded host observation catalog
    /// only when a checked-in native event fixture proves that integration.
    pub const fn fixture_backed_observation_integration_id(self) -> Option<HostIntegrationIdV1> {
        match self {
            Self::ClaudeCode => Some(HostIntegrationIdV1::Claude),
            Self::CursorDesktop => Some(HostIntegrationIdV1::Cursor),
            Self::Codex => Some(HostIntegrationIdV1::Codex),
            Self::Hermes => Some(HostIntegrationIdV1::Hermes),
            Self::Kiro => Some(HostIntegrationIdV1::Kiro),
            Self::CursorCloud
            | Self::ClineFamily
            | Self::Cline
            | Self::RooCode
            | Self::Kilo
            | Self::KimiCode
            | Self::OpenCode
            | Self::Gemini => None,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityV1 {
    Lsp,
    NativeDiagnostics,
    Hooks,
    Mcp,
    Cli,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostCapabilityUnavailableReasonV1 {
    HostApiAbsent,
    HostRegistrationUnsupported,
    NativeFixtureLimited,
    CheckedInEvidenceMissing,
    CompetingExtensionClaim,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum HostCapabilityStateV1 {
    Supported,
    Degraded(HostCapabilityUnavailableReasonV1),
    Unavailable(HostCapabilityUnavailableReasonV1),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostCapabilityRecordV1 {
    pub capability: HostCapabilityV1,
    pub state: HostCapabilityStateV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StockHostCapabilityViewV1 {
    host: HostKindV1,
    capabilities: [HostCapabilityRecordV1; 5],
}

impl StockHostCapabilityViewV1 {
    pub const fn host(&self) -> HostKindV1 {
        self.host
    }

    pub const fn capabilities(&self) -> &[HostCapabilityRecordV1; 5] {
        &self.capabilities
    }
}

const fn canonical_stock_host_capabilities(host: HostKindV1) -> [HostCapabilityRecordV1; 5] {
    use HostCapabilityStateV1::{Degraded, Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::{
        CheckedInEvidenceMissing, HostApiAbsent, HostRegistrationUnsupported, NativeFixtureLimited,
    };
    use HostCapabilityV1::{Cli, Hooks, Lsp, Mcp, NativeDiagnostics};

    let (lsp, native_diagnostics, hooks, mcp, cli) = match host {
        HostKindV1::ClaudeCode => (
            Supported,
            Unavailable(HostApiAbsent),
            Supported,
            Supported,
            Supported,
        ),
        HostKindV1::CursorDesktop => (
            Unavailable(HostRegistrationUnsupported),
            Supported,
            Supported,
            Supported,
            Supported,
        ),
        HostKindV1::CursorCloud => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Unavailable(CheckedInEvidenceMissing),
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostRegistrationUnsupported),
        ),
        HostKindV1::Codex | HostKindV1::Hermes => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Supported,
            Supported,
            Supported,
        ),
        HostKindV1::Kiro => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Degraded(NativeFixtureLimited),
            Supported,
            Supported,
        ),
        HostKindV1::ClineFamily | HostKindV1::Cline | HostKindV1::RooCode | HostKindV1::Kilo => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Unavailable(HostApiAbsent),
            Supported,
            Supported,
        ),
        HostKindV1::KimiCode => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Supported,
            Supported,
            Supported,
        ),
        HostKindV1::OpenCode => (Supported, Supported, Supported, Supported, Supported),
        // Gemini CLI's extension lifecycle carries exactly one registration
        // route: the `mcpServers` entry inside `gemini-extension.json`, which
        // `gemini extensions install` adopts. It exposes no LSP registration
        // and no diagnostics API. Its extension format does admit hooks, but
        // no checked-in native Gemini event fixture proves that route, and the
        // staged extension declares none — claiming Hooks here would report a
        // capability this integration cannot drive.
        HostKindV1::Gemini => (
            Unavailable(HostRegistrationUnsupported),
            Unavailable(HostApiAbsent),
            Unavailable(CheckedInEvidenceMissing),
            Supported,
            Supported,
        ),
    };
    [
        HostCapabilityRecordV1 {
            capability: Lsp,
            state: lsp,
        },
        HostCapabilityRecordV1 {
            capability: NativeDiagnostics,
            state: native_diagnostics,
        },
        HostCapabilityRecordV1 {
            capability: Hooks,
            state: hooks,
        },
        HostCapabilityRecordV1 {
            capability: Mcp,
            state: mcp,
        },
        HostCapabilityRecordV1 {
            capability: Cli,
            state: cli,
        },
    ]
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostIntegrationIdV1 {
    /// Claude Code's provider identifier remains `claude` for compatibility.
    Claude,
    Codex,
    Cursor,
    Hermes,
    Kiro,
}

impl HostIntegrationIdV1 {
    pub const ALL: [Self; 5] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Hermes,
        Self::Kiro,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Hermes => "hermes",
            Self::Kiro => "kiro",
        }
    }

    /// Stable host identifier used by hook, daemon, and telemetry adapters.
    pub const fn as_wire(self) -> &'static str {
        self.as_str()
    }

    /// Stable host identifier used by analytics dimensions.
    pub const fn as_key(self) -> &'static str {
        self.as_str()
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "hermes" => Some(Self::Hermes),
            "kiro" => Some(Self::Kiro),
            _ => None,
        }
    }

    /// Marker used to debounce this host's incremental project syncs.
    pub const fn sync_marker_file(self) -> &'static str {
        match self {
            Self::Claude => ".claude_post_tool_sync_at",
            Self::Codex => ".codex_shell_sync_at",
            Self::Cursor => ".cursor_shell_sync_at",
            Self::Hermes => ".hermes_terminal_receipt_at",
            Self::Kiro => ".kiro_post_tool_sync_at",
        }
    }
}

/// Every host integration, including every Hermes profile, binds this one
/// user-owned TraceDecay profile. Hosts never select storage or memory scope.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraceDecayProfileBindingV1 {
    User,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IntegrationEffectClassV1 {
    DaemonWrite,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IntegrationPrivacyClassV1 {
    SensitiveInputSanitizedByDaemon,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IntegrationDaemonApiV1 {
    HostAdmission,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum IntegrationDaemonActionV1 {
    CaptureObservation,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntegrationDaemonRequirementV1 {
    api: IntegrationDaemonApiV1,
    action: IntegrationDaemonActionV1,
}

impl IntegrationDaemonRequirementV1 {
    pub const fn new(api: IntegrationDaemonApiV1, action: IntegrationDaemonActionV1) -> Self {
        Self { api, action }
    }

    pub const fn api(&self) -> IntegrationDaemonApiV1 {
        self.api
    }

    pub const fn action(&self) -> IntegrationDaemonActionV1 {
        self.action
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostCapabilityViewV1 {
    integration_id: HostIntegrationIdV1,
    profile_binding: TraceDecayProfileBindingV1,
}

impl HostCapabilityViewV1 {
    pub fn new(
        integration_id: HostIntegrationIdV1,
        profile_binding: TraceDecayProfileBindingV1,
    ) -> Self {
        Self {
            integration_id,
            profile_binding,
        }
    }

    pub const fn integration_id(&self) -> HostIntegrationIdV1 {
        self.integration_id
    }

    pub const fn profile_binding(&self) -> TraceDecayProfileBindingV1 {
        self.profile_binding
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCapabilityV1 {
    capability_id: CapabilityId,
    effect_class: IntegrationEffectClassV1,
    privacy_class: IntegrationPrivacyClassV1,
    required_daemon: IntegrationDaemonRequirementV1,
    hosts: Vec<HostCapabilityViewV1>,
}

impl IntegrationCapabilityV1 {
    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub const fn effect_class(&self) -> IntegrationEffectClassV1 {
        self.effect_class
    }

    pub const fn privacy_class(&self) -> IntegrationPrivacyClassV1 {
        self.privacy_class
    }

    pub const fn required_daemon(&self) -> &IntegrationDaemonRequirementV1 {
        &self.required_daemon
    }

    pub fn hosts(&self) -> &[HostCapabilityViewV1] {
        &self.hosts
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostIntegrationCatalogV1 {
    schema_version: u16,
    capabilities: Vec<IntegrationCapabilityV1>,
}

impl HostIntegrationCatalogV1 {
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    pub fn capabilities(&self) -> &[IntegrationCapabilityV1] {
        &self.capabilities
    }

    pub fn stock_host_capabilities(&self, host: HostKindV1) -> &[HostCapabilityRecordV1; 5] {
        match host {
            HostKindV1::ClaudeCode => &STOCK_HOST_CAPABILITIES[0],
            HostKindV1::CursorDesktop => &STOCK_HOST_CAPABILITIES[1],
            HostKindV1::CursorCloud => &STOCK_HOST_CAPABILITIES[2],
            HostKindV1::Codex => &STOCK_HOST_CAPABILITIES[3],
            HostKindV1::Hermes => &STOCK_HOST_CAPABILITIES[4],
            HostKindV1::Kiro => &STOCK_HOST_CAPABILITIES[5],
            HostKindV1::ClineFamily => &STOCK_HOST_CAPABILITIES[6],
            HostKindV1::Cline => &STOCK_HOST_CAPABILITIES[7],
            HostKindV1::RooCode => &STOCK_HOST_CAPABILITIES[8],
            HostKindV1::Kilo => &STOCK_HOST_CAPABILITIES[9],
            HostKindV1::KimiCode => &STOCK_HOST_CAPABILITIES[10],
            HostKindV1::OpenCode => &STOCK_HOST_CAPABILITIES[11],
            HostKindV1::Gemini => &STOCK_HOST_CAPABILITIES[12],
        }
    }

    pub fn stock_host_capability_views(&self) -> Vec<StockHostCapabilityViewV1> {
        HostKindV1::ALL
            .into_iter()
            .map(|host| StockHostCapabilityViewV1 {
                host,
                capabilities: *self.stock_host_capabilities(host),
            })
            .collect()
    }

    /// Canonical bytes for the complete catalog authority: the observation-host
    /// matrix plus every stock host surface capability row.
    pub fn canonical_authority_bytes(&self) -> Result<Vec<u8>, DomainError> {
        canonical_json_bytes(&HostIntegrationCatalogAuthorityPayloadV1 {
            observation_catalog: self,
            stock_hosts: self.stock_host_capability_views(),
        })
    }

    pub fn canonical_authority_digest(&self) -> Result<[u8; 32], DomainError> {
        self.canonical_authority_bytes()
            .map(|bytes| Sha256::digest(bytes).into())
    }

    /// Canonical per-host projection pinned into embedded bundle manifests.
    pub fn host_capability_digest(&self, host: HostKindV1) -> Result<[u8; 32], DomainError> {
        canonical_json_bytes(&StockHostCapabilityViewV1 {
            host,
            capabilities: *self.stock_host_capabilities(host),
        })
        .map(|bytes| Sha256::digest(bytes).into())
    }

    pub fn validate(&self) -> Result<(), IntegrationCatalogError> {
        if self.schema_version != HOST_INTEGRATION_CATALOG_SCHEMA_VERSION_V1 {
            return Err(IntegrationCatalogError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.capabilities.is_empty() {
            return Err(IntegrationCatalogError::EmptyCatalog);
        }

        let required_hosts = HostIntegrationIdV1::ALL
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut capability_ids = BTreeSet::new();
        for capability in &self.capabilities {
            capability.capability_id.validate().map_err(|_| {
                IntegrationCatalogError::InvalidCapabilityId(
                    capability.capability_id.as_str().to_owned(),
                )
            })?;
            if !capability_ids.insert(capability.capability_id.as_str()) {
                return Err(IntegrationCatalogError::DuplicateCapabilityId(
                    capability.capability_id.as_str().to_owned(),
                ));
            }
            if capability.hosts.is_empty() {
                return Err(IntegrationCatalogError::EmptyHostMatrix(
                    capability.capability_id.as_str().to_owned(),
                ));
            }

            let mut integration_ids = BTreeSet::new();
            for host in &capability.hosts {
                if !integration_ids.insert(host.integration_id) {
                    return Err(IntegrationCatalogError::DuplicateHostIntegration {
                        capability_id: capability.capability_id.as_str().to_owned(),
                        integration_id: host.integration_id,
                    });
                }
            }
            if integration_ids != required_hosts {
                return Err(IntegrationCatalogError::IncompleteHostMatrix {
                    capability_id: capability.capability_id.as_str().to_owned(),
                    missing: required_hosts
                        .difference(&integration_ids)
                        .copied()
                        .collect(),
                });
            }
        }
        Ok(())
    }
}

const STOCK_HOST_CAPABILITIES: [[HostCapabilityRecordV1; 5]; 13] = [
    canonical_stock_host_capabilities(HostKindV1::ClaudeCode),
    canonical_stock_host_capabilities(HostKindV1::CursorDesktop),
    canonical_stock_host_capabilities(HostKindV1::CursorCloud),
    canonical_stock_host_capabilities(HostKindV1::Codex),
    canonical_stock_host_capabilities(HostKindV1::Hermes),
    canonical_stock_host_capabilities(HostKindV1::Kiro),
    canonical_stock_host_capabilities(HostKindV1::ClineFamily),
    canonical_stock_host_capabilities(HostKindV1::Cline),
    canonical_stock_host_capabilities(HostKindV1::RooCode),
    canonical_stock_host_capabilities(HostKindV1::Kilo),
    canonical_stock_host_capabilities(HostKindV1::KimiCode),
    canonical_stock_host_capabilities(HostKindV1::OpenCode),
    canonical_stock_host_capabilities(HostKindV1::Gemini),
];

#[derive(Serialize)]
struct HostIntegrationCatalogAuthorityPayloadV1<'a> {
    observation_catalog: &'a HostIntegrationCatalogV1,
    stock_hosts: Vec<StockHostCapabilityViewV1>,
}

pub fn stock_host_capabilities(host: HostKindV1) -> [HostCapabilityRecordV1; 5] {
    *host_integration_catalog_v1().stock_host_capabilities(host)
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntegrationCatalogError {
    #[error("unsupported host integration catalog schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("host integration catalog must define at least one capability")]
    EmptyCatalog,
    #[error("invalid capability id `{0}`")]
    InvalidCapabilityId(String),
    #[error("duplicate capability id `{0}`")]
    DuplicateCapabilityId(String),
    #[error("capability `{0}` must define a host matrix")]
    EmptyHostMatrix(String),
    #[error("capability `{capability_id}` repeats host integration `{integration_id:?}`")]
    DuplicateHostIntegration {
        capability_id: String,
        integration_id: HostIntegrationIdV1,
    },
    #[error("capability `{capability_id}` omits required host integrations {missing:?}")]
    IncompleteHostMatrix {
        capability_id: String,
        missing: Vec<HostIntegrationIdV1>,
    },
}

pub fn host_integration_catalog_v1() -> HostIntegrationCatalogV1 {
    let hosts = HostIntegrationIdV1::ALL
        .into_iter()
        .map(|integration_id| {
            HostCapabilityViewV1::new(integration_id, TraceDecayProfileBindingV1::User)
        })
        .collect();
    let capability = IntegrationCapabilityV1 {
        capability_id: CapabilityId::new(OBSERVATION_CAPTURE_CAPABILITY_ID)
            .expect("built-in integration capability id is valid"),
        effect_class: IntegrationEffectClassV1::DaemonWrite,
        privacy_class: IntegrationPrivacyClassV1::SensitiveInputSanitizedByDaemon,
        required_daemon: IntegrationDaemonRequirementV1::new(
            IntegrationDaemonApiV1::HostAdmission,
            IntegrationDaemonActionV1::CaptureObservation,
        ),
        hosts,
    };
    HostIntegrationCatalogV1 {
        schema_version: HOST_INTEGRATION_CATALOG_SCHEMA_VERSION_V1,
        capabilities: vec![capability],
    }
}
