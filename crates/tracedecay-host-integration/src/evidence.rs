use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    HostCapabilityStateV1, HostCapabilityUnavailableReasonV1, HostCapabilityV1, HostKindV1,
    stock_host_capabilities,
};

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

impl HostRegistrationRouteV1 {
    /// The canonical capability this route proves. Host-specific routes are
    /// deliberately narrower than their capability (Claude registers the
    /// analyzer through its configured-language LSP file, OpenCode through a
    /// custom LSP entry, Cursor exposes diagnostics through its native
    /// extension); this mapping is the only place that specialization lives.
    pub const fn capability(self) -> HostCapabilityV1 {
        match self {
            Self::ClaudeConfiguredLanguageLsp | Self::OpenCodeCustomLsp => HostCapabilityV1::Lsp,
            Self::CursorNativeDiagnostics => HostCapabilityV1::NativeDiagnostics,
            Self::Hook => HostCapabilityV1::Hooks,
            Self::Mcp => HostCapabilityV1::Mcp,
            Self::Cli => HostCapabilityV1::Cli,
        }
    }
}

/// Evidence behind one stock-host registration route. `state` is the
/// canonical [`stock_host_capabilities`] state of the capability the route
/// proves, never a second per-host decision. `starts_analyzer` is explicit so
/// a projection bridge cannot silently claim or spawn a language analyzer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostRegistrationEvidenceV1 {
    pub route: HostRegistrationRouteV1,
    pub state: HostCapabilityStateV1,
    pub evidence_ref: &'static str,
    pub starts_analyzer: bool,
}

/// One row of [`HOST_REGISTRATIONS`]: a stock host's registration route and
/// the evidence behind it. It carries no state on purpose — state is
/// projected from the canonical capability matrix when the row is read, so
/// the table can never disagree with `tracedecay-domain`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HostRegistrationDescriptor {
    pub(crate) host: HostKindV1,
    pub(crate) route: HostRegistrationRouteV1,
    pub(crate) evidence_ref: &'static str,
    pub(crate) starts_analyzer: bool,
}

/// Row literals for [`HOST_REGISTRATIONS`]. Every stock route is a projection
/// bridge over the daemon; none of them spawns a language analyzer, so
/// `starts_analyzer` is fixed here rather than repeated on every row.
macro_rules! host_registrations {
    ($($host:ident { $($route:ident => $evidence_ref:literal),+ $(,)? })+) => {
        &[$($(HostRegistrationDescriptor {
            host: HostKindV1::$host,
            route: HostRegistrationRouteV1::$route,
            evidence_ref: $evidence_ref,
            starts_analyzer: false,
        }),+),+]
    };
}

/// Every stock host's registration routes, CLI first. Evidence references are
/// stable repository or host-contract identifiers, never inferred
/// compatibility claims. Row order within a host is the order consumers see.
pub(crate) const HOST_REGISTRATIONS: &[HostRegistrationDescriptor] = host_registrations! {
    ClaudeCode {
        Cli => "src/tool_command.rs",
        ClaudeConfiguredLanguageLsp => "plugin/.lsp.json",
        Hook => "plugin/hooks/hooks-claude.json",
        Mcp => "plugin/.mcp.json",
    }
    CursorDesktop {
        Cli => "src/tool_command.rs",
        CursorNativeDiagnostics => "plugin/cursor-native-extension/package.json",
        Hook => "plugin/hooks/hooks-cursor.json",
        Mcp => "plugin/mcp-cursor.json",
    }
    CursorCloud {
        Cli => "src/tool_command.rs",
        Hook => "https://cursor.com/changelog",
        Mcp => "https://cursor.com/en-US/cloud",
    }
    Codex {
        Cli => "src/tool_command.rs",
        Hook => "crates/tracedecay-agent-hosts/src/agents/codex.rs",
        Mcp => "plugin/.mcp.json",
    }
    Devin {
        Cli => "src/tool_command.rs",
        Hook => "https://docs.devin.ai/work-with-devin/mcp",
        Mcp => "https://docs.devin.ai/cli/extensibility/mcp/configuration",
    }
    Zed {
        Cli => "src/tool_command.rs",
        Hook => "https://zed.dev/docs/ai/mcp",
        Mcp => "crates/tracedecay-agent-hosts/src/agents/zed.rs",
    }
    Antigravity {
        Cli => "src/tool_command.rs",
        Hook => "https://antigravity.google/docs/mcp",
        Mcp => "crates/tracedecay-agent-hosts/src/agents/antigravity.rs",
    }
    Vibe {
        Cli => "src/tool_command.rs",
        Hook => "https://docs.mistral.ai/vibe/code/cli/mcp-servers",
        Mcp => "crates/tracedecay-agent-hosts/src/agents/vibe.rs",
    }
    Hermes {
        Cli => "src/tool_command.rs",
        Hook => "src/agents/hermes/templates.rs",
        Mcp => "src/agents/hermes/profile_config.rs",
    }
    Kiro {
        Cli => "src/tool_command.rs",
        Hook => "tests/fixtures/host_events/kiro/baseline.json",
        Mcp => "src/agents/kiro.rs",
    }
    ClineFamily {
        Cli => "src/tool_command.rs",
        Hook => "cline_family_hook_evidence_absent_v1",
        Mcp => "crates/tracedecay-hooks/fixtures/host_events/cline-family.json",
    }
    Cline {
        Cli => "src/tool_command.rs",
        Hook => "crates/tracedecay-hooks/fixtures/host_events/cline-family.json",
        Mcp => "https://docs.cline.bot/mcp/mcp-overview",
    }
    RooCode {
        Cli => "src/tool_command.rs",
        Hook => "crates/tracedecay-hooks/fixtures/host_events/cline-family.json",
        Mcp => "https://roocodeinc.github.io/Roo-Code/features/mcp/using-mcp-in-roo/",
    }
    Kilo {
        Cli => "src/tool_command.rs",
        Hook => "crates/tracedecay-hooks/fixtures/host_events/cline-family.json",
        Mcp => "https://kilo.ai/docs/automate/mcp/using-in-kilo-code",
    }
    KimiCode {
        Cli => "src/tool_command.rs",
        Hook => "plugin/.kimi-plugin/plugin.json",
        Mcp => "plugin/.kimi-plugin/plugin.json",
    }
    OpenCode {
        Cli => "src/tool_command.rs",
        OpenCodeCustomLsp => "src/agents/opencode.rs",
        Hook => "plugin/opencode/tracedecay.ts",
        Mcp => "src/agents/opencode.rs",
    }
    // The tracedecay Gemini extension declares exactly one registration
    // route — its own `mcpServers.tracedecay` entry, adopted by
    // `gemini extensions install`. The extension format admits hooks, but
    // no checked-in native Gemini event fixture exists and the staged
    // manifest declares no hook, so the hook row names the absent fixture.
    Gemini {
        Cli => "src/tool_command.rs",
        Hook => "gemini_native_hook_fixture_absent_v1",
        Mcp => "src/agents/gemini/extension.rs",
    }
    // Copilot's adopted lifecycle carries exactly one registration route:
    // the `mcpServers.tracedecay` entry that `copilot mcp add` writes into
    // the host-owned `~/.copilot/mcp-config.json`. There is no Copilot hook
    // surface to gather a fixture for, so the hook row names the absent
    // surface — see the capability row in `tracedecay-domain`.
    Copilot {
        Cli => "src/tool_command.rs",
        Hook => "copilot_host_hook_surface_absent_v1",
        Mcp => "src/agents/copilot.rs",
    }
};

/// Registration routes and their evidence for one stock host, used by
/// packaging and conformance consumers: the host's `HOST_REGISTRATIONS` rows
/// with each row's state projected from the canonical capability matrix, so
/// changing a host's capability state is one edit in `tracedecay-domain`.
pub fn stock_host_registration_evidence(host: HostKindV1) -> Vec<HostRegistrationEvidenceV1> {
    let capabilities = stock_host_capabilities(host);
    HOST_REGISTRATIONS
        .iter()
        .filter(|row| row.host == host)
        .map(|row| HostRegistrationEvidenceV1 {
            route: row.route,
            state: capabilities[row.route.capability().row_index()].state,
            evidence_ref: row.evidence_ref,
            starts_analyzer: row.starts_analyzer,
        })
        .collect()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFeedbackBoundaryV1 {
    SavedEdit,
    Stop,
}

/// Truthful event-ingress evidence for one feedback boundary. A healthy MCP
/// or CLI read route does not make an edit/stop event exist, so `route` is
/// present only when checked-in native bytes prove that exact boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostFeedbackBoundaryEvidenceV1 {
    pub boundary: HostFeedbackBoundaryV1,
    pub state: HostCapabilityStateV1,
    pub route: Option<HostRegistrationRouteV1>,
    pub evidence_ref: &'static str,
    pub native_fixture_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HostEditStopConformanceEvidenceV1 {
    pub host: HostKindV1,
    pub edit: HostFeedbackBoundaryEvidenceV1,
    pub stop: HostFeedbackBoundaryEvidenceV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClineFamilyProviderV1 {
    Cline,
    RooCode,
    Kilo,
}

/// Admission recorded by the checked-in Cline-family evidence packet for one
/// exact provider. A documented protocol that was never captured locally
/// stays unverified; the packet currently admits no packaged route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClineFamilyAdmissionV1 {
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
#[hotpath::measure(label = "host_integration.evidence.cline_family")]
pub fn cline_family_evidence_from_embedded_assets(
    assets: &EmbeddedHostIntegrationEvidenceV1,
    provider: ClineFamilyProviderV1,
) -> Option<ClineFamilyEvidenceV1> {
    use HostCapabilityStateV1::Unavailable;
    use HostCapabilityUnavailableReasonV1::{CheckedInEvidenceMissing, NativeFixtureLimited};

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
    let route_state = match entry.host_hook_admission {
        ClineFamilyAdmissionV1::DocumentedUnverified => Unavailable(NativeFixtureLimited),
        ClineFamilyAdmissionV1::Unavailable => Unavailable(CheckedInEvidenceMissing),
    };
    Some(ClineFamilyEvidenceV1 {
        provider,
        registration: HostRegistrationEvidenceV1 {
            route: HostRegistrationRouteV1::Hook,
            state: route_state,
            evidence_ref: assets.cline_family_evidence_packet_path,
            starts_analyzer: false,
        },
        evidence_packet_path: assets.cline_family_evidence_packet_path,
        evidence_packet_digest: Sha256::digest(assets.cline_family_evidence_packet).into(),
        transcript_manifest_path: assets.cline_family_transcript_manifest_path,
        transcript_manifest_digest: Sha256::digest(assets.cline_family_transcript_manifest).into(),
        admission: entry.host_hook_admission,
        unavailable_reason: Some(
            entry
                .reason
                .unwrap_or_else(|| "no_reason_recorded_by_evidence_packet".to_string()),
        ),
        edit: route_state,
        stop: route_state,
    })
}

/// Consume root-composed authentic native fixture bytes. A documented but
/// uncaptured declaration remains unavailable rather than becoming capture
/// evidence.
#[hotpath::measure(label = "host_integration.evidence.native_fixture")]
pub fn stock_host_native_fixture_evidence_from_embedded_assets(
    assets: &EmbeddedHostIntegrationEvidenceV1,
    host: HostKindV1,
) -> Option<HostNativeFixtureEvidenceV1> {
    use HostCapabilityStateV1::{Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::NativeFixtureLimited;

    let (provider, source_path, evidenced_event, edit_identities) = match host {
        HostKindV1::ClaudeCode => (
            "claude",
            "crates/tracedecay-hooks/fixtures/host_events/claude.json",
            "PostToolUse,Stop",
            &["saved_edit", "tool_completed"][..],
        ),
        HostKindV1::Codex => (
            "codex",
            "crates/tracedecay-hooks/fixtures/host_events/codex.json",
            "Stop",
            &["saved_edit"][..],
        ),
        HostKindV1::CursorDesktop => (
            "cursor",
            "crates/tracedecay-hooks/fixtures/host_events/cursor.json",
            "afterFileEdit",
            &["saved_edit"][..],
        ),
        HostKindV1::Hermes => (
            "hermes",
            "crates/tracedecay-hooks/fixtures/host_events/hermes.json",
            "post_tool_call,on_session_end",
            &["saved_edit", "tool_completed"][..],
        ),
        HostKindV1::Kiro => (
            "kiro",
            "crates/tracedecay-hooks/fixtures/host_events/kiro.json",
            "userPromptSubmit",
            &["saved_edit"][..],
        ),
        HostKindV1::KimiCode => (
            "kimi_code",
            "crates/tracedecay-hooks/fixtures/host_events/kimi-code.json",
            "PostToolUse,Stop",
            &["saved_edit", "post_tool_use_edit"][..],
        ),
        HostKindV1::OpenCode => (
            "opencode",
            "crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json",
            "file.edited,tool.execute.after,session.idle/session.status,lsp.updated",
            &["saved_edit", "post_tool_use"][..],
        ),
        HostKindV1::CursorCloud
        | HostKindV1::Devin
        | HostKindV1::Zed
        | HostKindV1::Antigravity
        | HostKindV1::Vibe
        | HostKindV1::ClineFamily
        | HostKindV1::Cline
        | HostKindV1::RooCode
        | HostKindV1::Kilo
        | HostKindV1::Gemini
        | HostKindV1::Copilot => return None,
    };
    let bytes = assets
        .native_fixtures
        .iter()
        .find(|fixture| fixture.host == host)?
        .bytes;
    let event_state = |identities: &[&str]| {
        if fixture_has_native_event(bytes, identities) {
            Supported
        } else {
            Unavailable(NativeFixtureLimited)
        }
    };
    Some(HostNativeFixtureEvidenceV1 {
        host,
        provider,
        source_path,
        fixture_digest: Sha256::digest(bytes).into(),
        evidenced_event,
        edit: event_state(edit_identities),
        stop: event_state(&["stop"]),
    })
}

fn fixture_has_native_event(bytes: &[u8], identities: &[&str]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|document| {
            document
                .get("events")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .is_some_and(|events| {
            events.iter().any(|event| {
                event
                    .get("identity")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|identity| {
                        identities.contains(&identity)
                            && event.get("support").and_then(serde_json::Value::as_str)
                                == Some("native")
                    })
            })
        })
}

/// Resolve edit and stop ingress independently. Explicit feedback reads remain
/// described by [`stock_host_registration_evidence`]; they never upgrade an
/// absent native boundary into an event the daemon can receive.
#[hotpath::measure(label = "host_integration.evidence.edit_stop")]
pub fn host_edit_stop_conformance_evidence_from_embedded_assets(
    assets: &EmbeddedHostIntegrationEvidenceV1,
    host: HostKindV1,
) -> HostEditStopConformanceEvidenceV1 {
    use HostCapabilityStateV1::{Supported, Unavailable};
    use HostCapabilityUnavailableReasonV1::CheckedInEvidenceMissing;

    let native = stock_host_native_fixture_evidence_from_embedded_assets(assets, host);
    let boundary = |boundary, state, absent_ref| {
        let supported = state == Supported;
        HostFeedbackBoundaryEvidenceV1 {
            boundary,
            state,
            route: supported.then_some(HostRegistrationRouteV1::Hook),
            evidence_ref: native
                .as_ref()
                .map_or(absent_ref, |evidence| evidence.source_path),
            native_fixture_digest: native.as_ref().map(|evidence| evidence.fixture_digest),
        }
    };
    let edit_state = native
        .as_ref()
        .map_or(Unavailable(CheckedInEvidenceMissing), |evidence| {
            evidence.edit
        });
    let stop_state = native
        .as_ref()
        .map_or(Unavailable(CheckedInEvidenceMissing), |evidence| {
            evidence.stop
        });
    let absent_ref = match host {
        HostKindV1::Gemini => "gemini_native_edit_stop_fixture_absent_v1",
        HostKindV1::Copilot => "copilot_native_event_surface_absent_v1",
        _ => "native_edit_stop_fixture_absent_v1",
    };
    HostEditStopConformanceEvidenceV1 {
        host,
        edit: boundary(HostFeedbackBoundaryV1::SavedEdit, edit_state, absent_ref),
        stop: boundary(HostFeedbackBoundaryV1::Stop, stop_state, absent_ref),
    }
}

#[hotpath::measure(label = "host_integration.evidence.native_hosts")]
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
