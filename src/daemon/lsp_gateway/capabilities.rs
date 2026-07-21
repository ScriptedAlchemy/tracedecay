//! Capability intersection for the daemon LSP 3.17 gateway.
//!
//! The values here are an intentionally small, transport-independent model of
//! the PR12 matrix. The eventual initialize handler supplies the authoritative
//! client, admitted-project, policy, and upstream facts before it advertises
//! any capability.

use std::collections::BTreeSet;

/// The protocol version implemented by the gateway contract.
pub const LSP_PROTOCOL_VERSION: &str = "3.17";

/// LSP 3.17 client position encodings. PR12 advertises only UTF-16.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum PositionEncoding {
    #[default]
    Utf16,
    Utf8,
    Utf32,
}

/// The static text-document synchronization contract advertised by PR12.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextDocumentSync {
    pub open_close: bool,
    pub incremental: bool,
    pub save: bool,
}

impl Default for TextDocumentSync {
    fn default() -> Self {
        Self {
            open_close: true,
            incremental: true,
            save: true,
        }
    }
}

/// A semantic LSP provider whose availability depends on capability
/// negotiation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticCapability {
    Declaration,
    Definition,
    TypeDefinition,
    Implementation,
    References,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
    CallHierarchy,
    SignatureHelp,
    TypeHierarchy,
}

impl SemanticCapability {
    pub const ALL: [Self; 11] = [
        Self::Declaration,
        Self::Definition,
        Self::TypeDefinition,
        Self::Implementation,
        Self::References,
        Self::Hover,
        Self::DocumentSymbol,
        Self::WorkspaceSymbol,
        Self::CallHierarchy,
        Self::SignatureHelp,
        Self::TypeHierarchy,
    ];
}

/// The client facts relevant to the bounded PR12 gateway negotiation.
///
/// An empty `position_encodings` set means the client omitted the field, which
/// LSP 3.17 treats as implicit UTF-16 support.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientCapabilities {
    pub position_encodings: BTreeSet<PositionEncoding>,
    pub supports_versioned_publish_diagnostics: bool,
    pub publish_diagnostics_related_information: bool,
    pub publish_diagnostics_code_description: bool,
    pub publish_diagnostics_data: bool,
    pub supports_document_diagnostics: bool,
    pub document_diagnostics_related_information: bool,
    pub document_diagnostics_code_description: bool,
    pub document_diagnostics_data: bool,
    pub semantic: BTreeSet<SemanticCapability>,
}

impl ClientCapabilities {
    pub fn supports_position_encoding(&self, encoding: PositionEncoding) -> bool {
        self.position_encodings.is_empty() || self.position_encodings.contains(&encoding)
    }
}

/// Capabilities the daemon can safely guarantee for the admitted session.
///
/// The future session constructor derives this from gateway revision,
/// project/language admission, policy, configuration, and profile state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayCapabilities {
    pub supports_publish_diagnostics: bool,
    pub supports_document_diagnostics: bool,
    /// Whether the daemon can answer from canonical `TraceDecay` diagnostics
    /// when an upstream analyzer does not provide diagnostics.
    pub supports_managed_diagnostics: bool,
    pub semantic: BTreeSet<SemanticCapability>,
}

impl Default for GatewayCapabilities {
    fn default() -> Self {
        Self {
            supports_publish_diagnostics: true,
            supports_document_diagnostics: true,
            supports_managed_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        }
    }
}

/// Capabilities reported by the admitted upstream analyzer set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpstreamCapabilities {
    pub supports_diagnostics: bool,
    pub semantic: BTreeSet<SemanticCapability>,
}

/// The result of capability negotiation. Unsupported PR12 features stay false
/// regardless of client or upstream claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveCapabilities {
    pub protocol_version: &'static str,
    pub position_encoding: PositionEncoding,
    pub client_supports_utf16: bool,
    pub text_document_sync: TextDocumentSync,
    pub supports_publish_diagnostics: bool,
    pub supports_document_diagnostics: bool,
    pub semantic: BTreeSet<SemanticCapability>,
    pub workspace_folders_supported: bool,
    pub workspace_diagnostics_supported: bool,
    pub rename_supported: bool,
    pub general_code_actions_supported: bool,
    pub execute_command_supported: bool,
}

impl EffectiveCapabilities {
    pub fn supports_semantic(&self, capability: SemanticCapability) -> bool {
        self.semantic.contains(&capability)
    }

    pub fn initialization_availability(&self) -> CapabilityAvailability {
        if self.client_supports_utf16 {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable(CapabilityUnavailable {
                capability: "general.positionEncodings",
                reason: CapabilityUnavailableReason::ClientCapabilityMissing,
            })
        }
    }
}

/// A typed capability outcome suitable for later JSON-RPC error mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityAvailability {
    Available,
    Unavailable(CapabilityUnavailable),
}

/// Why the gateway could not truthfully advertise or serve a capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityUnavailableReason {
    ExplicitlyUnavailable,
    ClientCapabilityMissing,
    GatewayCapabilityMissing,
    UpstreamCapabilityMissing,
}

/// A protocol-facing description of an unavailable capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityUnavailable {
    pub capability: &'static str,
    pub reason: CapabilityUnavailableReason,
}

/// Computes the bounded PR12 intersection without advertising future
/// capabilities such as multi-root, rename, code actions, workspace
/// diagnostics, or execute-command.
pub fn negotiate_capabilities(
    client: &ClientCapabilities,
    gateway: &GatewayCapabilities,
    upstream: &UpstreamCapabilities,
) -> EffectiveCapabilities {
    let client_supports_utf16 = client.supports_position_encoding(PositionEncoding::Utf16);
    let semantic = if client_supports_utf16 {
        client
            .semantic
            .intersection(&gateway.semantic)
            .copied()
            .collect::<BTreeSet<_>>()
            .intersection(&upstream.semantic)
            .copied()
            .collect()
    } else {
        BTreeSet::new()
    };

    let diagnostics_supported = client_supports_utf16
        && (gateway.supports_managed_diagnostics || upstream.supports_diagnostics);
    let push_client_supported = client.supports_versioned_publish_diagnostics
        && client.publish_diagnostics_related_information
        && client.publish_diagnostics_code_description
        && client.publish_diagnostics_data;
    let pull_client_supported = client.supports_document_diagnostics
        && client.document_diagnostics_related_information
        && client.document_diagnostics_code_description
        && client.document_diagnostics_data;

    EffectiveCapabilities {
        protocol_version: LSP_PROTOCOL_VERSION,
        position_encoding: PositionEncoding::Utf16,
        client_supports_utf16,
        text_document_sync: TextDocumentSync::default(),
        supports_publish_diagnostics: diagnostics_supported
            && push_client_supported
            && gateway.supports_publish_diagnostics,
        supports_document_diagnostics: diagnostics_supported
            && pull_client_supported
            && gateway.supports_document_diagnostics,
        semantic,
        workspace_folders_supported: false,
        workspace_diagnostics_supported: false,
        rename_supported: false,
        general_code_actions_supported: false,
        execute_command_supported: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_client() -> ClientCapabilities {
        ClientCapabilities {
            supports_versioned_publish_diagnostics: true,
            publish_diagnostics_related_information: true,
            publish_diagnostics_code_description: true,
            publish_diagnostics_data: true,
            supports_document_diagnostics: true,
            document_diagnostics_related_information: true,
            document_diagnostics_code_description: true,
            document_diagnostics_data: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
            ..ClientCapabilities::default()
        }
    }

    #[test]
    fn intersects_semantics_and_never_advertises_deferred_methods() {
        let client = full_client();
        let gateway = GatewayCapabilities::default();
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: [SemanticCapability::Definition, SemanticCapability::Hover]
                .into_iter()
                .collect(),
        };

        let effective = negotiate_capabilities(&client, &gateway, &upstream);
        assert_eq!(
            effective.semantic,
            [SemanticCapability::Definition, SemanticCapability::Hover]
                .into_iter()
                .collect()
        );
        assert!(effective.supports_publish_diagnostics);
        assert!(effective.supports_document_diagnostics);
        assert!(!effective.workspace_folders_supported);
        assert!(!effective.workspace_diagnostics_supported);
        assert!(!effective.rename_supported);
        assert!(!effective.general_code_actions_supported);
        assert!(!effective.execute_command_supported);
    }

    #[test]
    fn managed_diagnostics_do_not_require_an_upstream_diagnostic_provider() {
        let effective = negotiate_capabilities(
            &full_client(),
            &GatewayCapabilities::default(),
            &UpstreamCapabilities::default(),
        );

        assert!(effective.supports_publish_diagnostics);
        assert!(effective.supports_document_diagnostics);
    }

    #[test]
    fn missing_stale_data_prerequisite_disables_only_diagnostic_paths() {
        let mut client = full_client();
        client.publish_diagnostics_data = false;
        client.document_diagnostics_data = false;
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        };

        let effective = negotiate_capabilities(&client, &GatewayCapabilities::default(), &upstream);
        assert!(!effective.supports_publish_diagnostics);
        assert!(!effective.supports_document_diagnostics);
        assert!(effective.supports_semantic(SemanticCapability::Definition));
    }

    #[test]
    fn explicit_non_utf16_client_fails_closed() {
        let mut client = full_client();
        client.position_encodings = [PositionEncoding::Utf8].into_iter().collect();
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        };
        let effective = negotiate_capabilities(&client, &GatewayCapabilities::default(), &upstream);

        assert_eq!(
            effective.initialization_availability(),
            CapabilityAvailability::Unavailable(CapabilityUnavailable {
                capability: "general.positionEncodings",
                reason: CapabilityUnavailableReason::ClientCapabilityMissing,
            })
        );
        assert!(effective.semantic.is_empty());
        assert!(!effective.supports_publish_diagnostics);
    }
}
