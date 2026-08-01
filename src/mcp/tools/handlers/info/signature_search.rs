//! `tracedecay_signature_search` — substring search over cached function/method signatures.

use super::*;

/// Handles `tracedecay_signature_search` — substring search across the
/// cached `signature` column on every Function/Method node.
pub(crate) async fn handle_signature_search(
    cg: &TraceDecay,
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
            message:
                "tracedecay_signature_search requires at least one of returns / params / async"
                    .to_string(),
        });
    }

    let function_nodes = cg.db().get_nodes_by_kind(NodeKind::Function).await?;
    let method_nodes = cg.db().get_nodes_by_kind(NodeKind::Method).await?;

    let mut entries: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for node in function_nodes.iter().chain(method_nodes.iter()) {
        if let Some(prefix) = path_filter
            && !crate::path_scope::path_matches_scope(&node.file_path, Some(prefix))
        {
            continue;
        }

        if let Some(want) = want_async
            && node.is_async != want
        {
            continue;
        }

        let Some(sig) = node.signature.as_deref() else {
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

        if !touched.contains(&node.file_path) {
            touched.push(node.file_path.clone());
        }
        entries.push(json!({
            "name": node.name,
            "qualified_name": node.qualified_name,
            "kind": node.kind.as_str(),
            "file": node.file_path,
            "line": node.start_line,
            "is_async": node.is_async,
            "signature": sig,
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
