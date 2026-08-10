//! `tracedecay_signature_search` — substring search over cached function/method signatures.

use super::*;

/// Handles `tracedecay_signature_search` — substring search across the
/// cached `signature` column on every Function/Method node.
pub(crate) async fn handle_signature_search(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let returns = args.get("returns").and_then(|v| v.as_str());
    let params: Vec<String> = args
        .get("params")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let want_async = args.get("async").and_then(serde_json::Value::as_bool);
    let path_filter = args.get("path").and_then(|v| v.as_str()).or(scope_prefix);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(50, |v| v.clamp(1, 500) as usize);

    if returns.is_none() && params.is_empty() && want_async.is_none() {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: one of 'returns', 'params', or 'async'"
                .to_string(),
        });
    }
    if want_async.is_some() {
        return Err(info_graph_error(
            "verified-signature-async-state-unavailable",
            "the verified code generation does not publish async-state metadata",
        ));
    }

    let mut entries: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for node in all_symbols(graph)? {
        let (metadata, file_path) = required_symbol_parts(&node)?;
        if !matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Function | NodeKind::Method)
        ) {
            continue;
        }
        if let Some(prefix) = path_filter
            && !crate::path_scope::path_matches_scope(file_path, Some(prefix))
        {
            continue;
        }

        let Some(sig) = metadata.signature.as_deref() else {
            continue;
        };

        if let Some(ret_pat) = returns
            && !returns_substring(sig).contains(ret_pat)
        {
            continue;
        }

        if !params.is_empty() {
            let param_region = params_substring(sig);
            if !params.iter().all(|p| param_region.contains(p.as_str())) {
                continue;
            }
        }

        if !touched.iter().any(|path| path == file_path) {
            touched.push(file_path.to_owned());
        }
        entries.push(json!({
            "id": node.occurrence.as_str(),
            "name": metadata.simple_name,
            "qualified_name": metadata.qualified_name,
            "kind": metadata.kind,
            "file": file_path,
            "line": metadata.start_line.saturating_add(1),
            "signature": sig,
            "unavailable_fields": ["is_async"],
        }));
        if entries.len() >= limit {
            break;
        }
    }

    let payload = json!({
        "match_count": entries.len(),
        "matches": entries,
    });
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
    ))
}

fn returns_substring(signature: &str) -> &str {
    match signature.find("->") {
        Some(pos) => signature[pos + 2..].trim_start(),
        None => signature,
    }
}

fn params_substring(signature: &str) -> &str {
    let bytes = signature.as_bytes();
    let Some(open) = signature.find('(') else {
        return signature;
    };
    let mut depth = 0i32;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &signature[open + 1..i];
                }
            }
            _ => {}
        }
    }
    signature
}
