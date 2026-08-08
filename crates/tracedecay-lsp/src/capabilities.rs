//! Capability intersection for the daemon LSP 3.17 gateway.
//!
//! The values here are an intentionally small, transport-independent model.
//! The initialize handler supplies the authoritative client, admitted-project,
//! policy, and upstream facts before it advertises any capability.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::{
    ContextProjectionKind, ContextProjectionRegistration, MAX_CONTEXT_PROJECTION_BYTES,
    MAX_CONTEXT_PROJECTION_ITEMS, MAX_CONTEXT_PROJECTION_KINDS, MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES,
    MAX_CONTEXT_SUMMARY_BYTES, TRACEDECAY_CONTEXT_REVISION,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TraceDecayClientCapabilities {
    revision: u32,
    #[serde(default)]
    opaque_expansion: bool,
    projections: Vec<ContextProjectionRegistration>,
}

/// The protocol version implemented by the gateway contract.
pub const LSP_PROTOCOL_VERSION: &str = "3.17";

/// Single source of truth for the kinds this gateway will negotiate. Baseline
/// reader projections and advisory-cycle contributions are both admitted here;
/// whether a given kind is actually mounted stays a producer-registration fact.
pub(crate) fn is_supported_context_projection(kind: &ContextProjectionKind) -> bool {
    kind.is_supported()
}

/// LSP 3.17 client position encodings. The gateway advertises only UTF-16.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum PositionEncoding {
    #[default]
    Utf16,
    Utf8,
    Utf32,
}

/// The static text-document synchronization contract advertised by the gateway.
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
    /// Internal read-only analyzer/graph evidence merge. Never advertised as
    /// an LSP rename provider.
    RenameCandidate,
}

impl SemanticCapability {
    pub const ALL: [Self; 12] = [
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
        Self::RenameCandidate,
    ];
}

/// The client facts relevant to bounded gateway negotiation.
///
/// An empty `position_encodings` set means the client omitted the field, which
/// LSP 3.17 treats as implicit UTF-16 support.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientCapabilities {
    pub position_encodings: BTreeSet<PositionEncoding>,
    /// Distinguishes an omitted field (implicit UTF-16 in LSP 3.17) from an
    /// explicitly empty or unsupported encoding list.
    pub position_encodings_declared: bool,
    pub supports_versioned_publish_diagnostics: bool,
    pub publish_diagnostics_related_information: bool,
    pub publish_diagnostics_code_description: bool,
    pub publish_diagnostics_data: bool,
    pub supports_document_diagnostics: bool,
    pub diagnostic_dynamic_registration: bool,
    pub workspace_diagnostic_refresh_support: bool,
    pub supports_workspace_folders: bool,
    pub semantic: BTreeSet<SemanticCapability>,
    pub context_projections: BTreeMap<ContextProjectionKind, u32>,
    pub supports_context_expansion: bool,
}

impl ClientCapabilities {
    pub fn supports_position_encoding(&self, encoding: PositionEncoding) -> bool {
        (!self.position_encodings_declared && self.position_encodings.is_empty())
            || self.position_encodings.contains(&encoding)
    }

    /// Parses only the LSP capability fields the gateway actually uses. Unknown
    /// fields are intentionally ignored rather than becoming accidental
    /// capability authority.
    pub fn from_initialize_capabilities(value: &Value) -> Result<Self, CapabilityParseError> {
        let Some(root) = value.as_object() else {
            return Err(CapabilityParseError::ExpectedObject);
        };
        let mut capabilities = Self::default();
        if let Some(encodings) = root
            .get("general")
            .and_then(Value::as_object)
            .and_then(|general| general.get("positionEncodings"))
        {
            capabilities.position_encodings_declared = true;
            let encodings = encodings
                .as_array()
                .ok_or(CapabilityParseError::InvalidPositionEncodings)?;
            capabilities.position_encodings = encodings
                .iter()
                .map(|encoding| {
                    encoding
                        .as_str()
                        .ok_or(CapabilityParseError::InvalidPositionEncodings)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter_map(|encoding| match encoding {
                    "utf-16" => Some(PositionEncoding::Utf16),
                    "utf-8" => Some(PositionEncoding::Utf8),
                    "utf-32" => Some(PositionEncoding::Utf32),
                    _ => None,
                })
                .collect();
        }
        let text_document = root.get("textDocument").and_then(Value::as_object);
        let publish = text_document
            .and_then(|text_document| text_document.get("publishDiagnostics"))
            .and_then(Value::as_object);
        capabilities.supports_versioned_publish_diagnostics = bool_at(publish, "versionSupport");
        capabilities.publish_diagnostics_related_information =
            bool_at(publish, "relatedInformation");
        capabilities.publish_diagnostics_code_description =
            bool_at(publish, "codeDescriptionSupport");
        capabilities.publish_diagnostics_data = bool_at(publish, "dataSupport");

        let diagnostic = text_document
            .and_then(|text_document| text_document.get("diagnostic"))
            .and_then(Value::as_object);
        capabilities.supports_document_diagnostics = diagnostic.is_some();
        capabilities.diagnostic_dynamic_registration = bool_at(diagnostic, "dynamicRegistration");
        // LSP 3.17's `textDocument.diagnostic` capability advertises pull
        // support only. Optional fields on the shared `Diagnostic` shape are
        // negotiated by `textDocument.publishDiagnostics`; accepting invented
        // fields under `diagnostic` would overstate a real client's support.
        let workspace = root.get("workspace").and_then(Value::as_object);
        capabilities.workspace_diagnostic_refresh_support = workspace
            .and_then(|workspace| workspace.get("diagnostic"))
            .and_then(Value::as_object)
            .is_some_and(|diagnostic| bool_at(Some(diagnostic), "refreshSupport"));
        capabilities.supports_workspace_folders = bool_at(workspace, "workspaceFolders");

        for (key, capability) in [
            ("declaration", SemanticCapability::Declaration),
            ("definition", SemanticCapability::Definition),
            ("typeDefinition", SemanticCapability::TypeDefinition),
            ("implementation", SemanticCapability::Implementation),
            ("references", SemanticCapability::References),
            ("hover", SemanticCapability::Hover),
            ("documentSymbol", SemanticCapability::DocumentSymbol),
            ("signatureHelp", SemanticCapability::SignatureHelp),
            ("callHierarchy", SemanticCapability::CallHierarchy),
            ("typeHierarchy", SemanticCapability::TypeHierarchy),
        ] {
            if text_document
                .and_then(|text_document| text_document.get(key))
                .is_some_and(capability_declared)
            {
                capabilities.semantic.insert(capability);
            }
        }
        if root
            .get("workspace")
            .and_then(Value::as_object)
            .and_then(|workspace| workspace.get("symbol"))
            .is_some_and(capability_declared)
        {
            capabilities
                .semantic
                .insert(SemanticCapability::WorkspaceSymbol);
        }
        if let Some(tracedecay) = root
            .get("experimental")
            .and_then(Value::as_object)
            .and_then(|experimental| experimental.get("tracedecay"))
        {
            let revision = tracedecay
                .as_object()
                .and_then(|tracedecay| tracedecay.get("revision"))
                .and_then(Value::as_u64)
                .and_then(|revision| u32::try_from(revision).ok())
                .ok_or(CapabilityParseError::InvalidTraceDecayCapabilities)?;
            if revision != TRACEDECAY_CONTEXT_REVISION {
                return Ok(capabilities);
            }
            let tracedecay: TraceDecayClientCapabilities =
                serde_json::from_value(tracedecay.clone())
                    .map_err(|_| CapabilityParseError::InvalidTraceDecayCapabilities)?;
            debug_assert_eq!(tracedecay.revision, revision);
            if tracedecay.projections.len() > MAX_CONTEXT_PROJECTION_KINDS {
                return Err(CapabilityParseError::InvalidTraceDecayCapabilities);
            }
            let mut negotiated = BTreeMap::new();
            for registration in tracedecay.projections {
                if !is_supported_context_projection(&registration.kind)
                    || registration.revision == 0
                    || negotiated
                        .insert(registration.kind, registration.revision)
                        .is_some()
                {
                    return Err(CapabilityParseError::InvalidTraceDecayCapabilities);
                }
            }
            capabilities.context_projections = negotiated;
            capabilities.supports_context_expansion = tracedecay.opaque_expansion;
        }
        Ok(capabilities)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityParseError {
    ExpectedObject,
    InvalidPositionEncodings,
    InvalidTraceDecayCapabilities,
}

/// Capabilities the daemon can safely guarantee for the admitted session.
///
/// The session constructor derives this from gateway revision,
/// project/language admission, policy, configuration, and profile state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayCapabilities {
    pub supports_publish_diagnostics: bool,
    pub supports_document_diagnostics: bool,
    /// True only when the daemon mounted exact indexed-root enumeration and
    /// canonical root diagnostic snapshot authorities.
    pub supports_workspace_diagnostics: bool,
    /// Whether the daemon can answer from canonical `TraceDecay` diagnostics
    /// when an upstream analyzer does not provide diagnostics.
    pub supports_managed_diagnostics: bool,
    /// True only when the daemon mounted an exact authorized scope set.
    pub supports_workspace_folders: bool,
    pub semantic: BTreeSet<SemanticCapability>,
    pub context_projections: BTreeMap<ContextProjectionKind, u32>,
    pub supports_context_expansion: bool,
}

impl Default for GatewayCapabilities {
    fn default() -> Self {
        Self {
            supports_publish_diagnostics: true,
            supports_document_diagnostics: true,
            supports_workspace_diagnostics: false,
            supports_managed_diagnostics: true,
            supports_workspace_folders: false,
            semantic: SemanticCapability::ALL.into_iter().collect(),
            context_projections: BTreeMap::new(),
            supports_context_expansion: true,
        }
    }
}

/// Capabilities reported by the admitted upstream analyzer set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UpstreamCapabilities {
    pub supports_diagnostics: bool,
    pub semantic: BTreeSet<SemanticCapability>,
}

/// The result of capability negotiation. Unsupported features stay false
/// regardless of client or upstream claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveCapabilities {
    pub protocol_version: &'static str,
    pub position_encoding: PositionEncoding,
    pub client_supports_utf16: bool,
    pub text_document_sync: TextDocumentSync,
    pub supports_publish_diagnostics: bool,
    pub publish_diagnostics_version: bool,
    pub publish_diagnostics_related_information: bool,
    pub publish_diagnostics_code_description: bool,
    pub publish_diagnostics_data: bool,
    pub supports_document_diagnostics: bool,
    pub document_diagnostics_related_information: bool,
    pub document_diagnostics_code_description: bool,
    pub document_diagnostics_data: bool,
    pub supports_workspace_diagnostic_refresh: bool,
    pub semantic: BTreeSet<SemanticCapability>,
    pub context_projections: BTreeMap<ContextProjectionKind, u32>,
    pub supports_context_expansion: bool,
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

    /// Exact server capability projection. Deferred features are absent,
    /// never advertised as `false` options that a client may still invoke.
    pub fn to_lsp_server_capabilities(&self) -> Value {
        let mut capabilities = serde_json::Map::new();
        capabilities.insert("positionEncoding".into(), Value::String("utf-16".into()));
        capabilities.insert(
            "textDocumentSync".into(),
            json!({
                "openClose": self.text_document_sync.open_close,
                "change": if self.text_document_sync.incremental { 2 } else { 0 },
                "save": self.text_document_sync.save,
            }),
        );
        capabilities.insert(
            "workspace".into(),
            json!({
                "workspaceFolders": {
                    "supported": self.workspace_folders_supported
                }
            }),
        );
        if self.supports_document_diagnostics {
            capabilities.insert(
                "diagnosticProvider".into(),
                json!({
                    "interFileDependencies": true,
                    "workspaceDiagnostics": self.workspace_diagnostics_supported,
                }),
            );
        }
        for (capability, key, value) in [
            (
                SemanticCapability::Declaration,
                "declarationProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::Definition,
                "definitionProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::TypeDefinition,
                "typeDefinitionProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::Implementation,
                "implementationProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::References,
                "referencesProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::Hover,
                "hoverProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::DocumentSymbol,
                "documentSymbolProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::WorkspaceSymbol,
                "workspaceSymbolProvider",
                json!({ "resolveProvider": false }),
            ),
            (
                SemanticCapability::CallHierarchy,
                "callHierarchyProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::SignatureHelp,
                "signatureHelpProvider",
                Value::Bool(true),
            ),
            (
                SemanticCapability::TypeHierarchy,
                "typeHierarchyProvider",
                Value::Bool(true),
            ),
        ] {
            if self.supports_semantic(capability) {
                capabilities.insert(key.into(), value);
            }
        }
        if !self.context_projections.is_empty() {
            capabilities.insert(
                "experimental".into(),
                json!({
                    "tracedecay": {
                        "revision": TRACEDECAY_CONTEXT_REVISION,
                        "opaqueExpansion": self.supports_context_expansion,
                        "limits": {
                            "maxItems": MAX_CONTEXT_PROJECTION_ITEMS,
                            "maxProjectionBytes": MAX_CONTEXT_PROJECTION_BYTES,
                            "maxProjectionKinds": MAX_CONTEXT_PROJECTION_KINDS,
                            "maxRetrievalHandleBytes": MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES,
                            "maxSummaryBytes": MAX_CONTEXT_SUMMARY_BYTES,
                        },
                        "projections": self.context_projections.iter().map(|(kind, revision)| {
                            json!({ "kind": kind, "revision": revision })
                        }).collect::<Vec<_>>(),
                    }
                }),
            );
        }
        Value::Object(capabilities)
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

/// Computes the bounded intersection without advertising deferred
/// capabilities such as rename, code actions, workspace diagnostics, or
/// execute-command.
pub fn negotiate_capabilities(
    client: &ClientCapabilities,
    gateway: &GatewayCapabilities,
    upstream: &UpstreamCapabilities,
) -> EffectiveCapabilities {
    let client_supports_utf16 = client.supports_position_encoding(PositionEncoding::Utf16);
    let mut semantic = if client_supports_utf16 {
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
    if client_supports_utf16
        && gateway
            .semantic
            .contains(&SemanticCapability::RenameCandidate)
        && upstream
            .semantic
            .contains(&SemanticCapability::RenameCandidate)
    {
        semantic.insert(SemanticCapability::RenameCandidate);
    }

    let diagnostics_supported = client_supports_utf16
        && (gateway.supports_managed_diagnostics || upstream.supports_diagnostics);
    // `textDocument/diagnostic` is the LSP registration method for both
    // document and workspace pull diagnostics. A dynamically capable client
    // therefore receives neither half statically when workspace readiness can
    // change after initialization; the session actor registers the complete
    // provider only while the canonical workspace authority is live.
    let dynamic_workspace_diagnostics = client.diagnostic_dynamic_registration
        && client.supports_document_diagnostics
        && gateway.supports_workspace_diagnostics;
    let context_projections: BTreeMap<ContextProjectionKind, u32> = client
        .context_projections
        .iter()
        .filter_map(|(kind, client_revision)| {
            gateway
                .context_projections
                .get(kind)
                .filter(|gateway_revision| *gateway_revision == client_revision)
                .map(|revision| (kind.clone(), *revision))
        })
        .collect();
    let supports_context_expansion = client.supports_context_expansion
        && gateway.supports_context_expansion
        && !context_projections.is_empty();
    EffectiveCapabilities {
        protocol_version: LSP_PROTOCOL_VERSION,
        position_encoding: PositionEncoding::Utf16,
        client_supports_utf16,
        text_document_sync: TextDocumentSync::default(),
        supports_publish_diagnostics: diagnostics_supported
            && client.supports_versioned_publish_diagnostics
            && gateway.supports_publish_diagnostics,
        publish_diagnostics_version: client.supports_versioned_publish_diagnostics,
        publish_diagnostics_related_information: client.publish_diagnostics_related_information,
        publish_diagnostics_code_description: client.publish_diagnostics_code_description,
        publish_diagnostics_data: client.publish_diagnostics_data,
        supports_document_diagnostics: diagnostics_supported
            && client.supports_document_diagnostics
            && gateway.supports_document_diagnostics
            && !dynamic_workspace_diagnostics,
        document_diagnostics_related_information: client.publish_diagnostics_related_information,
        document_diagnostics_code_description: client.publish_diagnostics_code_description,
        document_diagnostics_data: client.publish_diagnostics_data,
        supports_workspace_diagnostic_refresh: diagnostics_supported
            && client.supports_document_diagnostics
            && gateway.supports_document_diagnostics
            && client.workspace_diagnostic_refresh_support
            && !dynamic_workspace_diagnostics,
        semantic,
        context_projections,
        supports_context_expansion,
        workspace_folders_supported: client.supports_workspace_folders
            && gateway.supports_workspace_folders,
        workspace_diagnostics_supported: diagnostics_supported
            && client.supports_document_diagnostics
            && gateway.supports_workspace_diagnostics
            && !dynamic_workspace_diagnostics,
        rename_supported: false,
        general_code_actions_supported: false,
        execute_command_supported: false,
    }
}

fn bool_at(object: Option<&serde_json::Map<String, Value>>, key: &str) -> bool {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn capability_declared(value: &Value) -> bool {
    value.as_bool().unwrap_or_else(|| value.is_object())
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
            supports_workspace_folders: true,
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
        assert!(
            !effective.supports_semantic(SemanticCapability::RenameCandidate),
            "graph-only sessions cannot claim analyzer-derived rename evidence"
        );
    }

    #[test]
    fn workspace_folders_require_both_client_and_exact_gateway_authority() {
        let gateway = GatewayCapabilities {
            supports_workspace_folders: true,
            ..GatewayCapabilities::default()
        };
        let effective =
            negotiate_capabilities(&full_client(), &gateway, &UpstreamCapabilities::default());

        assert!(effective.workspace_folders_supported);
        assert_eq!(
            effective.to_lsp_server_capabilities()["workspace"]["workspaceFolders"]["supported"],
            true
        );
    }

    #[test]
    fn internal_rename_candidate_capability_is_never_advertised() {
        let effective = negotiate_capabilities(
            &full_client(),
            &GatewayCapabilities::default(),
            &UpstreamCapabilities {
                supports_diagnostics: true,
                semantic: SemanticCapability::ALL.into_iter().collect(),
            },
        );

        assert!(effective.supports_semantic(SemanticCapability::RenameCandidate));
        assert!(!effective.rename_supported);
        assert!(
            effective
                .to_lsp_server_capabilities()
                .get("renameProvider")
                .is_none()
        );
    }

    #[test]
    fn optional_diagnostic_fields_are_negotiated_without_disabling_diagnostics() {
        let mut client = full_client();
        client.publish_diagnostics_data = false;
        client.publish_diagnostics_related_information = false;
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        };

        let effective = negotiate_capabilities(&client, &GatewayCapabilities::default(), &upstream);
        assert!(effective.supports_publish_diagnostics);
        assert!(effective.supports_document_diagnostics);
        assert!(!effective.publish_diagnostics_data);
        assert!(!effective.publish_diagnostics_related_information);
        assert!(effective.publish_diagnostics_code_description);
        assert!(!effective.document_diagnostics_data);
        assert!(effective.document_diagnostics_code_description);
        assert!(!effective.document_diagnostics_related_information);
        assert!(effective.supports_semantic(SemanticCapability::Definition));
    }

    #[test]
    fn missing_version_support_disables_only_push_diagnostics() {
        let mut client = full_client();
        client.supports_versioned_publish_diagnostics = false;
        let effective = negotiate_capabilities(
            &client,
            &GatewayCapabilities::default(),
            &UpstreamCapabilities {
                supports_diagnostics: true,
                semantic: SemanticCapability::ALL.into_iter().collect(),
            },
        );

        assert!(!effective.supports_publish_diagnostics);
        assert!(effective.supports_document_diagnostics);
        assert!(effective.supports_semantic(SemanticCapability::Definition));
    }

    #[test]
    fn explicit_non_utf16_client_fails_closed() {
        let mut client = full_client();
        client.position_encodings = [PositionEncoding::Utf8].into_iter().collect();
        client.position_encodings_declared = true;
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

    #[test]
    fn parses_only_supported_client_fields_and_omits_deferred_server_methods() {
        let client = ClientCapabilities::from_initialize_capabilities(&json!({
            "general": { "positionEncodings": ["utf-16"] },
            "textDocument": {
                "publishDiagnostics": {
                    "versionSupport": true,
                    "relatedInformation": true,
                    "codeDescriptionSupport": true,
                    "dataSupport": true
                },
                "diagnostic": {},
                "definition": {},
                "hover": {}
            },
            "workspace": {
                "symbol": {},
                "diagnostic": { "refreshSupport": true }
            }
        }))
        .unwrap();
        let effective = negotiate_capabilities(
            &client,
            &GatewayCapabilities::default(),
            &UpstreamCapabilities {
                supports_diagnostics: true,
                semantic: [
                    SemanticCapability::Definition,
                    SemanticCapability::Hover,
                    SemanticCapability::WorkspaceSymbol,
                ]
                .into_iter()
                .collect(),
            },
        );
        let advertised = effective.to_lsp_server_capabilities();
        assert_eq!(advertised["positionEncoding"], "utf-16");
        assert_eq!(
            advertised["workspace"]["workspaceFolders"]["supported"],
            false
        );
        assert!(advertised.get("definitionProvider").is_some());
        assert!(advertised.get("renameProvider").is_none());
        assert!(advertised.get("codeActionProvider").is_none());
        assert!(advertised.get("executeCommandProvider").is_none());
        assert!(effective.document_diagnostics_related_information);
        assert!(effective.document_diagnostics_code_description);
        assert!(effective.document_diagnostics_data);
    }

    #[test]
    fn pull_diagnostics_ignore_nonstandard_field_claims() {
        let client = ClientCapabilities::from_initialize_capabilities(&json!({
            "textDocument": {
                "diagnostic": {
                    "relatedInformation": true,
                    "codeDescriptionSupport": true,
                    "dataSupport": true
                }
            }
        }))
        .expect("standard pull capability");

        assert!(client.supports_document_diagnostics);
        let effective = negotiate_capabilities(
            &client,
            &GatewayCapabilities::default(),
            &UpstreamCapabilities::default(),
        );
        assert!(!effective.document_diagnostics_related_information);
        assert!(!effective.document_diagnostics_code_description);
        assert!(!effective.document_diagnostics_data);
    }

    #[test]
    fn context_expansion_requires_explicit_client_negotiation() {
        let projection = ContextProjectionKind::diagnostics();
        let mut gateway = GatewayCapabilities::default();
        gateway
            .context_projections
            .insert(projection.clone(), TRACEDECAY_CONTEXT_REVISION);
        let client = ClientCapabilities::from_initialize_capabilities(&json!({
            "experimental": {
                "tracedecay": {
                    "revision": TRACEDECAY_CONTEXT_REVISION,
                    "opaqueExpansion": true,
                    "projections": [{
                        "kind": "diagnostics",
                        "revision": TRACEDECAY_CONTEXT_REVISION
                    }]
                }
            }
        }))
        .expect("context capability");
        let effective = negotiate_capabilities(&client, &gateway, &UpstreamCapabilities::default());
        assert!(effective.supports_context_expansion);
        assert_eq!(
            effective.to_lsp_server_capabilities()["experimental"]["tracedecay"]["opaqueExpansion"],
            true
        );
        assert_eq!(
            effective.to_lsp_server_capabilities()["experimental"]["tracedecay"]["limits"]["maxItems"],
            MAX_CONTEXT_PROJECTION_ITEMS
        );

        let mut without_expansion = client;
        without_expansion.supports_context_expansion = false;
        assert!(
            !negotiate_capabilities(
                &without_expansion,
                &gateway,
                &UpstreamCapabilities::default()
            )
            .supports_context_expansion
        );
    }

    #[test]
    fn context_capability_dto_rejects_unknown_duplicate_and_arbitrary_projections() {
        for tracedecay in [
            json!({
                "revision": TRACEDECAY_CONTEXT_REVISION,
                "projections": [],
                "unexpected": true,
            }),
            json!({
                "revision": TRACEDECAY_CONTEXT_REVISION,
                "projections": [
                    { "kind": "diagnostics", "revision": TRACEDECAY_CONTEXT_REVISION },
                    { "kind": "diagnostics", "revision": TRACEDECAY_CONTEXT_REVISION },
                ],
            }),
            json!({
                "revision": TRACEDECAY_CONTEXT_REVISION,
                "projections": [
                    { "kind": "arbitraryProviderPayload", "revision": TRACEDECAY_CONTEXT_REVISION },
                ],
            }),
        ] {
            assert_eq!(
                ClientCapabilities::from_initialize_capabilities(&json!({
                    "experimental": { "tracedecay": tracedecay }
                })),
                Err(CapabilityParseError::InvalidTraceDecayCapabilities)
            );
        }
    }

    #[test]
    fn context_capability_dto_accepts_recognized_advisory_projections() {
        let capabilities = ClientCapabilities::from_initialize_capabilities(&json!({
            "experimental": {
                "tracedecay": {
                    "revision": TRACEDECAY_CONTEXT_REVISION,
                    "projections": [
                        { "kind": "githubReview", "revision": TRACEDECAY_CONTEXT_REVISION },
                        { "kind": "ciFailureLocalization", "revision": TRACEDECAY_CONTEXT_REVISION },
                        { "kind": "agentProximity", "revision": TRACEDECAY_CONTEXT_REVISION },
                    ],
                }
            }
        }))
        .expect("recognized advisory projection capabilities");

        assert_eq!(
            capabilities
                .context_projections
                .keys()
                .map(ContextProjectionKind::as_str)
                .collect::<Vec<_>>(),
            vec!["agentProximity", "ciFailureLocalization", "githubReview"]
        );
    }

    #[test]
    fn future_context_revision_does_not_block_standard_capability_parsing() {
        let capabilities = ClientCapabilities::from_initialize_capabilities(&json!({
            "general": { "positionEncodings": ["utf-16"] },
            "experimental": {
                "tracedecay": {
                    "revision": TRACEDECAY_CONTEXT_REVISION + 1,
                    "futureShape": { "not": "a current DTO" }
                }
            }
        }))
        .expect("future extension must not block standard LSP");
        assert!(capabilities.supports_position_encoding(PositionEncoding::Utf16));
        assert!(capabilities.context_projections.is_empty());
        assert!(!capabilities.supports_context_expansion);
    }

    #[test]
    fn explicit_empty_unknown_or_malformed_position_encodings_never_gain_implicit_utf16() {
        for capabilities in [
            json!({ "general": { "positionEncodings": [] } }),
            json!({ "general": { "positionEncodings": ["future-encoding"] } }),
        ] {
            let client = ClientCapabilities::from_initialize_capabilities(&capabilities).unwrap();
            assert!(!client.supports_position_encoding(PositionEncoding::Utf16));
        }

        assert_eq!(
            ClientCapabilities::from_initialize_capabilities(&json!({
                "general": { "positionEncodings": "utf-16" }
            })),
            Err(CapabilityParseError::InvalidPositionEncodings)
        );
        assert_eq!(
            ClientCapabilities::from_initialize_capabilities(&json!({
                "general": { "positionEncodings": ["utf-16", 16] }
            })),
            Err(CapabilityParseError::InvalidPositionEncodings)
        );
    }
}
