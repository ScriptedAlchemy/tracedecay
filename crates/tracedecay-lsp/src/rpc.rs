//! JSON-RPC 2.0 framing helpers, parameter parsing, and gateway response encoding.

use serde_json::{Map, Value, json};

use crate::capabilities::EffectiveCapabilities;
use crate::diagnostics::{
    DiagnosticSeverity, DocumentDiagnosticReport, GatewayDiagnostic, GatewayDiagnosticCoverage,
    GatewayDiagnosticData, GatewayDiagnosticLifecycle, GatewayDiagnosticProviderState, LspPosition,
    LspRange, MAX_DIAGNOSTIC_RELATED_INFORMATION, MAX_DIAGNOSTIC_RELATED_MESSAGE_BYTES,
    TRACEDECAY_DIAGNOSTIC_DATA_REVISION, safe_code_description_uri, safe_related_uri,
    truncate_utf8,
};
use crate::gateway::{
    CallHierarchyItem, DocumentSymbol, GatewayResponse, Hover, IncomingCall, LspLocation,
    MethodUnavailableReason, OutgoingCall, RenameCandidateResult, RenameCandidateUnavailableReason,
    SemanticResponse, SignatureHelp, TypeHierarchyItem, WorkspaceSymbol,
};
use crate::overlay::{OverlayChange, OverlayError};
use crate::session::{LspRequestFailure, LspRequestId};

#[derive(Clone, Debug)]
pub(crate) struct RpcFailure {
    pub code: i64,
    pub message: &'static str,
    pub data: Value,
}

impl RpcFailure {
    pub fn invalid_params(message: &'static str) -> Self {
        Self {
            code: -32602,
            message: "Invalid params",
            data: json!({ "detail": message }),
        }
    }

    pub fn unavailable(method: &str, reason: MethodUnavailableReason) -> Self {
        Self {
            code: super::gateway::MethodUnavailable::JSON_RPC_METHOD_NOT_FOUND,
            message: "Method unavailable",
            data: json!({
                "method": method,
                "reason": unavailable_reason(reason),
            }),
        }
    }

    pub fn request_failure(failure: LspRequestFailure) -> Self {
        let data = match failure {
            LspRequestFailure::RequestCancelled => json!({ "retriggerRequest": false }),
            LspRequestFailure::ContentModified => json!({ "retriggerRequest": true }),
            LspRequestFailure::ServerCancelled { retrigger_request } => {
                json!({ "retriggerRequest": retrigger_request })
            }
        };
        Self {
            code: failure.code(),
            message: match failure {
                LspRequestFailure::RequestCancelled => "Request cancelled",
                LspRequestFailure::ContentModified => "Content modified",
                LspRequestFailure::ServerCancelled { .. } => "Server cancelled request",
            },
            data,
        }
    }
}

pub(crate) fn response_value<T>(
    response: GatewayResponse<T>,
    project: impl FnOnce(T) -> Value,
) -> Result<Value, RpcFailure> {
    match response {
        GatewayResponse::Value(value) => Ok(project(value)),
        GatewayResponse::Partial {
            coverage, detail, ..
        } => Err(RpcFailure {
            code: -32802,
            message: "Server cancelled request",
            data: partial_failure_data(coverage, detail),
        }),
        GatewayResponse::Pending => Err(RpcFailure {
            code: -32802,
            message: "Server cancelled request",
            data: json!({
                "retriggerRequest": true,
                "coverage": "semantic-pending-not-polled",
            }),
        }),
        GatewayResponse::Unavailable(unavailable) => Err(RpcFailure::unavailable(
            unavailable.method.as_lsp_method(),
            unavailable.reason,
        )),
        GatewayResponse::RequestFailed(failure) => Err(RpcFailure::request_failure(failure)),
    }
}

/// JSON-RPC error `data` for a partial semantic outcome: the stable coverage
/// token, plus the bounded human-readable `detail` when the provider supplied
/// one (omitted, never null, when absent).
pub(crate) fn partial_failure_data(coverage: String, detail: Option<String>) -> Value {
    let mut data = json!({ "retriggerRequest": true, "coverage": coverage });
    if let Some(detail) = detail {
        data["detail"] = Value::String(detail);
    }
    data
}

pub(crate) fn semantic_response_value(response: SemanticResponse) -> Value {
    match response {
        SemanticResponse::Locations(value) => locations_value(value),
        SemanticResponse::Hover(value) => hover_value(value),
        SemanticResponse::DocumentSymbols(value) => document_symbols_value(value),
        SemanticResponse::WorkspaceSymbols(value) => workspace_symbols_value(value),
        SemanticResponse::CallHierarchyItems(value) => call_items_value(value),
        SemanticResponse::IncomingCalls(value) => incoming_calls_value(value),
        SemanticResponse::OutgoingCalls(value) => outgoing_calls_value(value),
        SemanticResponse::SignatureHelp(value) => signature_help_value(value),
        SemanticResponse::TypeHierarchyItems(value) => type_items_value(value),
        SemanticResponse::RenameCandidate(value) => rename_candidate_value(value),
    }
}

fn rename_candidate_value(value: RenameCandidateResult) -> Value {
    match value {
        RenameCandidateResult::Available(candidate) => json!({
            "status": "available",
            "documentUri": candidate.document_uri,
            "range": range_value(candidate.range),
            "placeholder": candidate.placeholder,
        }),
        RenameCandidateResult::Unavailable { reason } => json!({
            "status": "unavailable",
            "reason": match reason {
                RenameCandidateUnavailableReason::AnalyzerUnavailable => "analyzerUnavailable",
                RenameCandidateUnavailableReason::GraphUnavailable => "graphUnavailable",
                RenameCandidateUnavailableReason::EvidenceAbsent => "evidenceAbsent",
                RenameCandidateUnavailableReason::StaleEvidence => "staleEvidence",
                RenameCandidateUnavailableReason::AmbiguousEvidence => "ambiguousEvidence",
            },
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DiagnosticSerializationCapabilities {
    related_information: bool,
    code_description: bool,
    data: bool,
}

impl DiagnosticSerializationCapabilities {
    pub(crate) fn push(capabilities: &EffectiveCapabilities) -> Self {
        Self {
            related_information: capabilities.publish_diagnostics_related_information,
            code_description: capabilities.publish_diagnostics_code_description,
            data: capabilities.publish_diagnostics_data,
        }
    }

    pub(crate) fn pull(capabilities: &EffectiveCapabilities) -> Self {
        Self {
            related_information: capabilities.document_diagnostics_related_information,
            code_description: capabilities.document_diagnostics_code_description,
            data: capabilities.document_diagnostics_data,
        }
    }

    #[cfg(test)]
    const fn all() -> Self {
        Self {
            related_information: true,
            code_description: true,
            data: true,
        }
    }
}

pub(crate) fn document_diagnostic_report_value(
    report: DocumentDiagnosticReport,
    capabilities: DiagnosticSerializationCapabilities,
) -> Value {
    match report {
        DocumentDiagnosticReport::Full { result_id, items } => json!({
            "kind": "full",
            "resultId": result_id,
            "items": items
                .into_iter()
                .map(|item| diagnostic_value(item, capabilities))
                .collect::<Vec<_>>(),
        }),
        DocumentDiagnosticReport::Unchanged { result_id } => json!({
            "kind": "unchanged",
            "resultId": result_id,
        }),
    }
}

pub(crate) fn diagnostic_value(
    diagnostic: GatewayDiagnostic,
    capabilities: DiagnosticSerializationCapabilities,
) -> Value {
    let mut value = json!({
        "range": range_value(diagnostic.range),
        "severity": diagnostic.severity.map(severity_value),
        "code": diagnostic.code,
        "source": diagnostic.source.wire_name(),
        "message": diagnostic.message,
    });
    if capabilities.code_description
        && let Some(uri) = diagnostic
            .code_description_uri
            .filter(|uri| safe_code_description_uri(uri))
    {
        value["codeDescription"] = json!({ "href": uri });
    }
    if capabilities.related_information {
        let related = diagnostic
            .related_information
            .into_iter()
            .filter(|related| {
                safe_related_uri(&related.uri) && related.range.start <= related.range.end
            })
            .take(MAX_DIAGNOSTIC_RELATED_INFORMATION)
            .map(|mut related| {
                truncate_utf8(&mut related.message, MAX_DIAGNOSTIC_RELATED_MESSAGE_BYTES);
                json!({
                    "location": {
                        "uri": related.uri,
                        "range": range_value(related.range),
                    },
                    "message": related.message,
                })
            })
            .collect::<Vec<_>>();
        if !related.is_empty() {
            value["relatedInformation"] = Value::Array(related);
        }
    }
    if capabilities.data
        && let Some(data) = diagnostic.data
    {
        value["data"] = diagnostic_data_value(data);
    }
    value
}

fn diagnostic_data_value(data: GatewayDiagnosticData) -> Value {
    json!({
        "tracedecay": {
            "revision": TRACEDECAY_DIAGNOSTIC_DATA_REVISION,
            "identity": {
                "findingId": data.identity.finding_id,
                "anchorId": data.identity.anchor_id,
                "generation": data.identity.generation,
                "headCommitId": data.identity.head_commit_id,
                "codeGenerationId": data.identity.code_generation_id,
                "snapshotDigest": data.identity.snapshot_digest,
                "invalidationDigest": data.identity.invalidation_digest,
                "snapshotContentDigest": data.identity.snapshot_content_digest,
                "documentContentDigest": data.identity.document_content_digest,
            },
            "lifecycle": diagnostic_lifecycle_value(data.lifecycle),
            "providerState": diagnostic_provider_state_value(data.provider_state),
            "coverage": diagnostic_coverage_value(data.coverage),
            "expansionHandle": data.expansion_handle,
        },
    })
}

fn diagnostic_lifecycle_value(lifecycle: GatewayDiagnosticLifecycle) -> &'static str {
    match lifecycle {
        GatewayDiagnosticLifecycle::Active => "active",
        GatewayDiagnosticLifecycle::Superseded => "superseded",
        GatewayDiagnosticLifecycle::Resolved => "resolved",
        GatewayDiagnosticLifecycle::Cleared => "cleared",
    }
}

fn diagnostic_provider_state_value(state: GatewayDiagnosticProviderState) -> &'static str {
    match state {
        GatewayDiagnosticProviderState::SupportedCompletedComplete => "supportedCompletedComplete",
        GatewayDiagnosticProviderState::Unsupported => "unsupported",
        GatewayDiagnosticProviderState::Absent => "absent",
        GatewayDiagnosticProviderState::Indexing => "indexing",
        GatewayDiagnosticProviderState::Stale => "stale",
        GatewayDiagnosticProviderState::Cancelled => "cancelled",
        GatewayDiagnosticProviderState::TimedOut => "timedOut",
        GatewayDiagnosticProviderState::Failed => "failed",
        GatewayDiagnosticProviderState::Partial => "partial",
        GatewayDiagnosticProviderState::Unavailable => "unavailable",
    }
}

fn diagnostic_coverage_value(coverage: GatewayDiagnosticCoverage) -> &'static str {
    match coverage {
        GatewayDiagnosticCoverage::Complete => "complete",
        GatewayDiagnosticCoverage::Partial => "partial",
        GatewayDiagnosticCoverage::Unavailable => "unavailable",
        GatewayDiagnosticCoverage::Failed => "failed",
    }
}

fn severity_value(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 1,
        DiagnosticSeverity::Warning => 2,
        DiagnosticSeverity::Information => 3,
        DiagnosticSeverity::Hint => 4,
    }
}

pub(crate) fn locations_value(locations: Vec<LspLocation>) -> Value {
    Value::Array(locations.into_iter().map(location_value).collect())
}

pub(crate) fn location_value(location: LspLocation) -> Value {
    json!({ "uri": location.uri, "range": range_value(location.range) })
}

pub(crate) fn hover_value(hover: Option<Hover>) -> Value {
    hover.map_or(Value::Null, |hover| {
        json!({
            "contents": hover.contents,
            "range": hover.range.map(range_value),
        })
    })
}

pub(crate) fn document_symbols_value(symbols: Vec<DocumentSymbol>) -> Value {
    Value::Array(symbols.into_iter().map(document_symbol_value).collect())
}

fn document_symbol_value(symbol: DocumentSymbol) -> Value {
    json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "range": range_value(symbol.range),
        "selectionRange": range_value(symbol.selection_range),
        "children": symbol.children.into_iter().map(document_symbol_value).collect::<Vec<_>>(),
    })
}

pub(crate) fn workspace_symbols_value(symbols: Vec<WorkspaceSymbol>) -> Value {
    Value::Array(
        symbols
            .into_iter()
            .map(|symbol| {
                json!({
                    "name": symbol.name,
                    "kind": symbol.kind,
                    "location": location_value(symbol.location),
                })
            })
            .collect(),
    )
}

pub(crate) fn call_items_value(items: Vec<CallHierarchyItem>) -> Value {
    Value::Array(items.into_iter().map(call_item_value).collect())
}

pub(crate) fn call_item_value(item: CallHierarchyItem) -> Value {
    json!({
        "name": item.name,
        "kind": item.kind,
        "uri": item.uri,
        "range": range_value(item.range),
        "selectionRange": range_value(item.selection_range),
    })
}

pub(crate) fn incoming_calls_value(calls: Vec<IncomingCall>) -> Value {
    Value::Array(
        calls
            .into_iter()
            .map(|call| {
                json!({
                    "from": call_item_value(call.from),
                    "fromRanges": call.from_ranges.into_iter().map(range_value).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

pub(crate) fn outgoing_calls_value(calls: Vec<OutgoingCall>) -> Value {
    Value::Array(
        calls
            .into_iter()
            .map(|call| {
                json!({
                    "to": call_item_value(call.to),
                    "fromRanges": call.from_ranges.into_iter().map(range_value).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

pub(crate) fn signature_help_value(help: Option<SignatureHelp>) -> Value {
    help.map_or(Value::Null, |help| {
        json!({
            "signatures": help
                .signatures
                .into_iter()
                .map(|label| json!({ "label": label }))
                .collect::<Vec<_>>(),
            "activeSignature": help.active_signature,
            "activeParameter": help.active_parameter,
        })
    })
}

pub(crate) fn type_items_value(items: Vec<TypeHierarchyItem>) -> Value {
    Value::Array(
        items
            .into_iter()
            .map(|item| {
                json!({
                    "name": item.name,
                    "kind": item.kind,
                    "uri": item.uri,
                    "range": range_value(item.range),
                    "selectionRange": range_value(item.selection_range),
                })
            })
            .collect(),
    )
}

pub(crate) fn range_value(range: LspRange) -> Value {
    json!({ "start": position_value(range.start), "end": position_value(range.end) })
}

pub(crate) fn position_value(position: LspPosition) -> Value {
    json!({ "line": position.line, "character": position.character })
}

pub(crate) fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(crate) fn error_response(id: Value, failure: RpcFailure) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": failure.code,
            "message": failure.message,
            "data": failure.data,
        },
    })
}

pub(crate) fn request_id(value: &Value) -> Option<LspRequestId> {
    value.as_i64().map(LspRequestId::Number).or_else(|| {
        value
            .as_str()
            .map(|value| LspRequestId::String(value.to_owned()))
    })
}

pub(crate) fn request_id_value(id: LspRequestId) -> Value {
    match id {
        LspRequestId::Number(value) => json!(value),
        LspRequestId::String(value) => json!(value),
    }
}

pub(crate) fn unavailable_reason(reason: MethodUnavailableReason) -> &'static str {
    match reason {
        MethodUnavailableReason::ExplicitlyUnavailable => "explicitlyUnavailable",
        MethodUnavailableReason::CapabilityNotNegotiated => "capabilityNotNegotiated",
        MethodUnavailableReason::OutsideAdmittedRoot => "outsideAdmittedRoot",
        MethodUnavailableReason::ProviderUnavailable => "providerUnavailable",
    }
}

pub(crate) fn deferred_method_reason(method: &str) -> MethodUnavailableReason {
    match method {
        "textDocument/prepareRename"
        | "textDocument/rename"
        | "textDocument/codeAction"
        | "workspace/diagnostic"
        | "workspace/executeCommand"
        | "workspace/didChangeWorkspaceFolders" => MethodUnavailableReason::ExplicitlyUnavailable,
        _ => MethodUnavailableReason::CapabilityNotNegotiated,
    }
}

pub(crate) fn initialized_root_uri(params: &Value) -> Result<String, RpcFailure> {
    let folders = params.get("workspaceFolders");
    let folder_uri = match folders {
        Some(Value::Array(folders)) if folders.len() > 1 => {
            return Err(RpcFailure::invalid_params(
                "multiple workspace folders are unsupported",
            ));
        }
        Some(Value::Array(folders)) if folders.len() == 1 => folders[0]
            .get("uri")
            .and_then(Value::as_str)
            .filter(|uri| !uri.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| RpcFailure::invalid_params("workspace folder uri is required"))?,
        Some(Value::Array(_) | Value::Null) | None => String::new(),
        Some(_) => {
            return Err(RpcFailure::invalid_params(
                "workspaceFolders must be an array",
            ));
        }
    };
    let root_uri = match params.get("rootUri") {
        Some(Value::String(root_uri)) if !root_uri.is_empty() => Some(root_uri.clone()),
        Some(Value::Null) | None => None,
        Some(_) => {
            return Err(RpcFailure::invalid_params(
                "rootUri must be a non-empty string",
            ));
        }
    };
    if folder_uri.is_empty() {
        root_uri.ok_or_else(|| RpcFailure::invalid_params("one explicit rootUri is required"))
    } else {
        if let Some(root_uri) = root_uri
            && root_uri != folder_uri
        {
            return Err(RpcFailure::invalid_params(
                "rootUri and workspace folder differ",
            ));
        }
        Ok(folder_uri)
    }
}

pub(crate) fn text_document(params: &Value) -> Result<&Map<String, Value>, RpcFailure> {
    params
        .get("textDocument")
        .and_then(Value::as_object)
        .ok_or_else(|| RpcFailure::invalid_params("textDocument is required"))
}

pub(crate) fn required_string(
    object: &Map<String, Value>,
    key: &str,
) -> Result<String, RpcFailure> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcFailure::invalid_params("required string is missing"))
}

pub(crate) fn required_nonempty_string(
    object: &Map<String, Value>,
    key: &str,
) -> Result<String, RpcFailure> {
    required_string(object, key).and_then(|value| {
        (!value.is_empty())
            .then_some(value)
            .ok_or_else(|| RpcFailure::invalid_params("required string is missing"))
    })
}

pub(crate) fn required_i64(object: &Map<String, Value>, key: &str) -> Result<i64, RpcFailure> {
    object
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcFailure::invalid_params("required integer is missing"))
}

pub(crate) fn required_u32(object: &Map<String, Value>, key: &str) -> Result<u32, RpcFailure> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RpcFailure::invalid_params("required unsigned integer is missing"))
}

pub(crate) fn document_uri(params: &Value) -> Result<String, RpcFailure> {
    required_nonempty_string(text_document(params)?, "uri")
}

pub(crate) fn document_position(params: &Value) -> Result<(String, LspPosition), RpcFailure> {
    let uri = document_uri(params)?;
    let position = parse_position(
        params
            .get("position")
            .ok_or_else(|| RpcFailure::invalid_params("position is required"))?,
    )?;
    Ok((uri, position))
}

pub(crate) fn parse_position(value: &Value) -> Result<LspPosition, RpcFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| RpcFailure::invalid_params("position must be an object"))?;
    let line = object
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|line| u32::try_from(line).ok())
        .ok_or_else(|| RpcFailure::invalid_params("position line is invalid"))?;
    let character = object
        .get("character")
        .and_then(Value::as_u64)
        .and_then(|character| u32::try_from(character).ok())
        .ok_or_else(|| RpcFailure::invalid_params("position character is invalid"))?;
    Ok(LspPosition { line, character })
}

pub(crate) fn parse_range(value: &Value) -> Result<LspRange, RpcFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| RpcFailure::invalid_params("range must be an object"))?;
    let start = parse_position(
        object
            .get("start")
            .ok_or_else(|| RpcFailure::invalid_params("range start is required"))?,
    )?;
    let end = parse_position(
        object
            .get("end")
            .ok_or_else(|| RpcFailure::invalid_params("range end is required"))?,
    )?;
    Ok(LspRange { start, end })
}

pub(crate) fn parse_overlay_change(value: &Value) -> Result<OverlayChange, RpcFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| RpcFailure::invalid_params("content change must be an object"))?;
    let range = object.get("range").map(parse_range).transpose()?;
    let range_length = object
        .get("rangeLength")
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| RpcFailure::invalid_params("rangeLength is invalid"))
        })
        .transpose()?;
    let text = required_string(object, "text")?;
    Ok(OverlayChange {
        range,
        range_length,
        text,
    })
}

pub(crate) fn parse_call_item(value: &Value) -> Result<CallHierarchyItem, RpcFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| RpcFailure::invalid_params("call hierarchy item must be an object"))?;
    Ok(CallHierarchyItem {
        name: required_nonempty_string(object, "name")?,
        kind: required_u32(object, "kind")?,
        uri: required_nonempty_string(object, "uri")?,
        range: parse_range(
            object
                .get("range")
                .ok_or_else(|| RpcFailure::invalid_params("range is required"))?,
        )?,
        selection_range: parse_range(
            object
                .get("selectionRange")
                .ok_or_else(|| RpcFailure::invalid_params("selectionRange is required"))?,
        )?,
    })
}

pub(crate) fn parse_type_item(value: &Value) -> Result<TypeHierarchyItem, RpcFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| RpcFailure::invalid_params("type hierarchy item must be an object"))?;
    Ok(TypeHierarchyItem {
        name: required_nonempty_string(object, "name")?,
        kind: required_u32(object, "kind")?,
        uri: required_nonempty_string(object, "uri")?,
        range: parse_range(
            object
                .get("range")
                .ok_or_else(|| RpcFailure::invalid_params("range is required"))?,
        )?,
        selection_range: parse_range(
            object
                .get("selectionRange")
                .ok_or_else(|| RpcFailure::invalid_params("selectionRange is required"))?,
        )?,
    })
}

pub(crate) fn overlay_failure(error: OverlayError) -> RpcFailure {
    RpcFailure {
        code: -32602,
        message: "Invalid params",
        data: json!({ "overlay": format!("{error:?}") }),
    }
}

pub(crate) fn diagnostic_result_id(generation: u64, version: i64) -> String {
    format!("generation:{generation}:version:{version}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{
        GatewayDiagnosticCoverage, GatewayDiagnosticData, GatewayDiagnosticIdentity,
        GatewayDiagnosticLifecycle, GatewayDiagnosticProviderState,
        GatewayDiagnosticRelatedInformation,
    };

    /// A partial semantic outcome must stay retriggerable and must echo the
    /// caller-supplied detail verbatim or not at all. Adapters rely on this to
    /// keep analyzer text out of the wire: whatever they hand in is exactly
    /// what a client sees, so their own allowlist is the only filter.
    #[test]
    fn partial_semantic_responses_stay_retriggerable_and_carry_only_supplied_detail() {
        let failure = response_value(
            GatewayResponse::Partial {
                value: SemanticResponse::Hover(None),
                coverage: "analyzer-start-failed".to_owned(),
                detail: Some("Analyzer failed to start.".to_owned()),
            },
            semantic_response_value,
        )
        .expect_err("a partial semantic response is a retriggerable failure");
        assert_eq!(failure.code, -32802);
        assert_eq!(failure.data["retriggerRequest"], true);
        assert_eq!(failure.data["coverage"], "analyzer-start-failed");
        assert_eq!(failure.data["detail"], "Analyzer failed to start.");

        let failure = response_value(
            GatewayResponse::Partial {
                value: SemanticResponse::Hover(None),
                coverage: "analyzer-start-failed".to_owned(),
                detail: None,
            },
            semantic_response_value,
        )
        .expect_err("a partial semantic response is a retriggerable failure");
        assert_eq!(failure.code, -32802);
        assert_eq!(failure.data["retriggerRequest"], true);
        assert!(
            failure.data.get("detail").is_none(),
            "absent detail must be omitted, never serialized as null"
        );
    }

    #[test]
    fn pending_semantic_responses_are_retriggerable_under_a_stable_coverage_token() {
        let failure = response_value(
            GatewayResponse::<SemanticResponse>::Pending,
            semantic_response_value,
        )
        .expect_err("a pending semantic response is a retriggerable failure");
        assert_eq!(failure.code, -32802);
        assert_eq!(failure.data["retriggerRequest"], true);
        assert_eq!(failure.data["coverage"], "semantic-pending-not-polled");
        assert!(failure.data.get("detail").is_none());
    }

    #[test]
    fn tracedecay_diagnostic_data_projects_exact_identity_and_expansion_handle() {
        let value = diagnostic_value(GatewayDiagnostic {
            uri: "file:///root/a.rs".to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 2,
                    character: 3,
                },
                end: LspPosition {
                    line: 2,
                    character: 8,
                },
            },
            severity: Some(DiagnosticSeverity::Warning),
            code: Some("clippy::needless_borrow".to_owned()),
            code_description_uri: None,
            message: "needless borrow".to_owned(),
            source: super::super::diagnostics::DiagnosticSource::TraceDecay,
            related_information: Vec::new(),
            data: Some(GatewayDiagnosticData {
                identity: GatewayDiagnosticIdentity {
                    finding_id: "feedback.finding.v1.abc".to_owned(),
                    anchor_id: "anchor.v1.abc".to_owned(),
                    generation: 17,
                    head_commit_id: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    code_generation_id: "codegen.v1.abc".to_owned(),
                    snapshot_digest:
                        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .to_owned(),
                    invalidation_digest:
                        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                            .to_owned(),
                    snapshot_content_digest:
                        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                            .to_owned(),
                    document_content_digest:
                        "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                            .to_owned(),
                },
                lifecycle: GatewayDiagnosticLifecycle::Active,
                provider_state: GatewayDiagnosticProviderState::SupportedCompletedComplete,
                coverage: GatewayDiagnosticCoverage::Complete,
                expansion_handle: "rh_0123456789abcdef01234567".to_owned(),
            }),
        }, DiagnosticSerializationCapabilities::all());

        assert_eq!(
            value,
            json!({
                "range": {
                    "start": { "line": 2, "character": 3 },
                    "end": { "line": 2, "character": 8 },
                },
                "severity": 2,
                "code": "clippy::needless_borrow",
                "source": "tracedecay",
                "message": "needless borrow",
                "data": {
                    "tracedecay": {
                        "revision": 1,
                        "identity": {
                            "findingId": "feedback.finding.v1.abc",
                            "anchorId": "anchor.v1.abc",
                            "generation": 17,
                            "headCommitId": "0123456789abcdef0123456789abcdef01234567",
                            "codeGenerationId": "codegen.v1.abc",
                            "snapshotDigest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "invalidationDigest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "snapshotContentDigest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                            "documentContentDigest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                        },
                        "lifecycle": "active",
                        "providerState": "supportedCompletedComplete",
                        "coverage": "complete",
                        "expansionHandle": "rh_0123456789abcdef01234567",
                    },
                },
            })
        );
    }

    #[test]
    fn diagnostic_data_is_omitted_when_canonical_identity_is_unavailable() {
        let value = diagnostic_value(
            GatewayDiagnostic {
                uri: "file:///root/a.rs".to_owned(),
                range: LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 1,
                    },
                },
                severity: None,
                code: None,
                code_description_uri: None,
                message: "upstream".to_owned(),
                source: super::super::diagnostics::DiagnosticSource::Upstream,
                related_information: Vec::new(),
                data: None,
            },
            DiagnosticSerializationCapabilities::all(),
        );

        assert!(value.get("data").is_none());
    }

    #[test]
    fn diagnostic_optional_fields_require_negotiated_capabilities_and_safe_urls() {
        let diagnostic = GatewayDiagnostic {
            uri: "file:///root/a.rs".to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 1,
                    character: 2,
                },
                end: LspPosition {
                    line: 1,
                    character: 3,
                },
            },
            severity: Some(DiagnosticSeverity::Information),
            code: Some("github-review".to_owned()),
            code_description_uri: Some(
                "https://github.com/acme/repo/pull/7#discussion_r42".to_owned(),
            ),
            message: "Unresolved GitHub review comment".to_owned(),
            source: super::super::diagnostics::DiagnosticSource::TraceDecayGitHub,
            related_information: vec![
                GatewayDiagnosticRelatedInformation {
                    uri: "file:///root/caller.rs".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 8,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 8,
                            character: 6,
                        },
                    },
                    message: "Affected caller".to_owned(),
                },
                GatewayDiagnosticRelatedInformation {
                    uri: "https://user:secret@example.com/private".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 0,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 0,
                            character: 1,
                        },
                    },
                    message: "must be redacted".to_owned(),
                },
            ],
            data: None,
        };

        let enabled = diagnostic_value(
            diagnostic.clone(),
            DiagnosticSerializationCapabilities::all(),
        );
        assert_eq!(
            enabled["codeDescription"]["href"],
            "https://github.com/acme/repo/pull/7#discussion_r42"
        );
        assert_eq!(enabled["relatedInformation"].as_array().unwrap().len(), 1);
        assert_eq!(
            enabled["relatedInformation"][0]["location"]["uri"],
            "file:///root/caller.rs"
        );

        let mut unsafe_url = diagnostic.clone();
        unsafe_url.code_description_uri =
            Some("https://user:secret@github.com/acme/repo/pull/7".to_owned());
        assert!(
            diagnostic_value(unsafe_url, DiagnosticSerializationCapabilities::all())
                .get("codeDescription")
                .is_none()
        );

        let disabled = diagnostic_value(
            diagnostic,
            DiagnosticSerializationCapabilities {
                related_information: false,
                code_description: false,
                data: false,
            },
        );
        assert!(disabled.get("codeDescription").is_none());
        assert!(disabled.get("relatedInformation").is_none());
    }
}
