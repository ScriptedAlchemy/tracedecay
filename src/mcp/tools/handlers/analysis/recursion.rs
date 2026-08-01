//! `tracedecay_recursion` — self-recursive and mutually recursive symbol detection.

use super::*;

/// Handles `tracedecay_recursion` tool calls.
///
/// Detects cycles in the call graph using iterative DFS on the calls-only
/// edge subgraph. Each cycle is a vec of node IDs forming the loop.
pub(crate) async fn handle_recursion(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    let path_prefix = effective_path(&args, scope_prefix);

    require_positive_limit(limit, "tracedecay_recursion")?;

    let call_edges = cg.get_call_edges_with_lines(path_prefix).await?;

    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
    let mut node_cache: HashMap<String, Option<crate::types::Node>> = HashMap::new();
    let mut lines_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();

    for (src, tgt, line) in &call_edges {
        if src == tgt {
            let Some(node) = cached_node(cg, &mut node_cache, src).await? else {
                continue;
            };
            if !is_direct_self_call(cg, &mut lines_cache, &node, *line) {
                continue;
            }
        }
        adj.entry(src.clone()).or_default().insert(tgt.clone());
        adj.entry(tgt.clone()).or_default();
    }

    // Collect only the cyclic SCCs, then sort smallest-first so we keep
    // shorter / more interesting cycles when the cap kicks in. We still need
    // every cyclic SCC enumerated before sorting (truncating early would bias
    // toward Tarjan emission order), but we cap the per-SCC path search.
    let mut cyclic_sccs: Vec<Vec<String>> = crate::graph::scc::tarjan_scc(&adj)
        .into_iter()
        .filter(|scc| crate::graph::scc::is_cyclic_scc(scc, &adj))
        .collect();
    cyclic_sccs.sort_by_key(Vec::len);

    let mut cycles: Vec<Vec<String>> = Vec::new();
    for mut scc in cyclic_sccs {
        if cycles.len() >= limit {
            break;
        }
        if let Some(path) = cycle_path_for_scc(&mut scc, &adj) {
            cycles.push(path);
        }
    }
    cycles.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    cycles.truncate(limit);

    // Resolve node details for each cycle
    let mut cycle_items: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for cycle in &cycles {
        let mut chain: Vec<Value> = Vec::new();
        for node_id in cycle {
            if let Some(node) = cg.get_node(node_id).await? {
                touched.push(node.file_path.clone());
                chain.push(json!({
                    "id": node.id,
                    "name": node.name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                }));
            } else {
                chain.push(json!({ "id": node_id }));
            }
        }
        cycle_items.push(json!({
            "length": cycle.len() - 1,
            "chain": chain,
        }));
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = json!({
        "cycle_count": cycle_items.len(),
        "cycles": cycle_items,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}

async fn cached_node(
    cg: &TraceDecay,
    cache: &mut HashMap<String, Option<crate::types::Node>>,
    id: &str,
) -> Result<Option<crate::types::Node>> {
    if let Some(node) = cache.get(id) {
        return Ok(node.clone());
    }
    let node = cg.get_node(id).await?;
    cache.insert(id.to_string(), node.clone());
    Ok(node)
}

fn cached_lines<'a>(
    cg: &TraceDecay,
    cache: &'a mut HashMap<String, Option<Vec<String>>>,
    file_path: &str,
) -> Option<&'a Vec<String>> {
    if !cache.contains_key(file_path) {
        let abs = cg.project_root().join(file_path);
        // Blank comments and string/char literals for Rust files so a
        // `name(` that appears only inside a comment or string is not mistaken
        // for a real self-call. Non-Rust files are scanned raw (the Rust
        // grammar would mis-tokenise them).
        let lines = std::fs::read_to_string(abs).ok().map(|content| {
            let scanned = if path_is_rust(file_path) {
                tracedecay_code_extraction::source_mask::masked_rust_source_with(
                    &content,
                    tracedecay_code_extraction::source_mask::MaskOptions::CODE_SCAN,
                )
            } else {
                content
            };
            scanned.lines().map(str::to_string).collect()
        });
        cache.insert(file_path.to_string(), lines);
    }
    cache.get(file_path).and_then(Option::as_ref)
}

fn is_direct_self_call(
    cg: &TraceDecay,
    lines_cache: &mut HashMap<String, Option<Vec<String>>>,
    node: &crate::types::Node,
    edge_line: Option<u32>,
) -> bool {
    let Some(lines) = cached_lines(cg, lines_cache, &node.file_path) else {
        return false;
    };
    if lines.is_empty() {
        return false;
    }

    let mut candidate_lines: Vec<u32> = edge_line.into_iter().collect();
    if let Some(line) = edge_line {
        candidate_lines.push(line.saturating_sub(1));
        candidate_lines.push(line.saturating_add(1));
    }
    candidate_lines.sort_unstable();
    candidate_lines.dedup();

    for line in candidate_lines {
        let Some(text) = lines.get(line as usize) else {
            continue;
        };
        if looks_like_function_declaration(text, &node.name) {
            continue;
        }
        if has_qualified_call(text, node) || has_bare_call(text, &node.name) {
            return true;
        }
    }

    false
}

fn looks_like_function_declaration(line: &str, name: &str) -> bool {
    let Some(pos) = line.find(name) else {
        return false;
    };
    let prefix = &line[..pos];
    (prefix.contains("fn ")
        || prefix.contains("function ")
        || prefix.contains("def ")
        || prefix.contains("sub "))
        && call_suffix_starts(&line[pos + name.len()..])
}

fn parent_type_name(node: &crate::types::Node) -> Option<&str> {
    let needle = format!("::{}", node.name);
    node.qualified_name
        .strip_suffix(&needle)
        .and_then(|parent| parent.rsplit("::").next())
        .filter(|parent| !parent.is_empty())
}

fn has_qualified_call(line: &str, node: &crate::types::Node) -> bool {
    let Some(parent) = parent_type_name(node) else {
        return false;
    };
    let type_call = format!("{parent}::{}", node.name);
    if line
        .match_indices(&type_call)
        .any(|(idx, _)| call_suffix_starts(&line[idx + type_call.len()..]))
    {
        return true;
    }

    let self_call = format!("Self::{}", node.name);
    if line
        .match_indices(&self_call)
        .any(|(idx, _)| call_suffix_starts(&line[idx + self_call.len()..]))
    {
        return true;
    }

    let self_method_call = format!("self.{}", node.name);
    line.match_indices(&self_method_call)
        .any(|(idx, _)| call_suffix_starts(&line[idx + self_method_call.len()..]))
}

fn has_bare_call(line: &str, name: &str) -> bool {
    // Fast path: a bare call always needs an opening paren on the same line.
    // For common short names like `new`/`get`/`len` this short-circuits the
    // expensive `match_indices + is_ident_byte` scan on lines that obviously
    // can't contain a call (assignments, comments, docstrings, …).
    if name.is_empty() || !line.contains('(') {
        return false;
    }
    let bytes = line.as_bytes();
    let name_len = name.len();
    line.match_indices(name).any(|(idx, _)| {
        // Reject substring matches inside a larger identifier on either side:
        // `name=new` should not match `newer`, `renew`, etc. Cheap byte
        // checks before the more expensive prefix-trim probe.
        let before_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        if !before_ok {
            return false;
        }
        let after_idx = idx + name_len;
        let after_ok = after_idx == bytes.len() || !is_ident_byte(bytes[after_idx]);
        if !after_ok {
            return false;
        }
        let prefix = line[..idx].trim_end();
        if prefix.ends_with('.') || prefix.ends_with(':') {
            return false;
        }
        call_suffix_starts(&line[after_idx..])
    })
}

fn call_suffix_starts(suffix: &str) -> bool {
    suffix.trim_start().starts_with('(')
}

fn cycle_path_for_scc(
    scc: &mut [String],
    adj: &HashMap<String, HashSet<String>>,
) -> Option<Vec<String>> {
    scc.sort();
    let scc_set: HashSet<&str> = scc.iter().map(std::string::String::as_str).collect();
    if scc.len() == 1 {
        let id = scc[0].clone();
        if adj
            .get(&id)
            .is_some_and(|neighbors| neighbors.contains(&id))
        {
            return Some(vec![id.clone(), id]);
        }
        return None;
    }

    for start in scc.iter() {
        // `path` and `seen` operate on borrowed ids from `scc_set`: the SCC
        // outlives this call, so we never need to allocate `String`s during
        // the DFS itself. The final result has to be `Vec<String>` because
        // it leaves the function, so we materialise once at the end.
        let start_ref: &str = start.as_str();
        let mut path: Vec<&str> = vec![start_ref];
        let mut seen: HashSet<&str> = HashSet::from([start_ref]);
        if dfs_cycle_path(start_ref, start_ref, &scc_set, adj, &mut path, &mut seen) {
            return Some(path.into_iter().map(str::to_string).collect());
        }
    }
    None
}

fn dfs_cycle_path<'a>(
    current: &'a str,
    start: &'a str,
    scc_set: &HashSet<&'a str>,
    adj: &'a HashMap<String, HashSet<String>>,
    path: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
) -> bool {
    let Some(neighbors) = adj.get(current) else {
        return false;
    };
    let mut neighbors: Vec<&'a str> = neighbors
        .iter()
        .filter_map(|n| scc_set.get(n.as_str()).copied())
        .collect();
    neighbors.sort_unstable();

    for neighbor in neighbors {
        if neighbor == start && path.len() > 1 {
            path.push(start);
            return true;
        }
        if !seen.insert(neighbor) {
            continue;
        }
        path.push(neighbor);
        if dfs_cycle_path(neighbor, start, scc_set, adj, path, seen) {
            return true;
        }
        path.pop();
        seen.remove(neighbor);
    }
    false
}
