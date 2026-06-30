use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::errors::Result;
use crate::tracedecay::TraceDecay;

use super::support::filter_by_scope;

pub(super) fn should_check_ignored_dependency_hint(result_count: usize, limit: usize) -> bool {
    result_count == 0 || result_count < limit.clamp(1, 20)
}

pub(super) async fn ignored_dependency_hint(
    cg: &TraceDecay,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
) -> Result<Option<Value>> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Ok(None);
    }
    let candidate_limit = limit.clamp(1, 20);
    let db = if cg.is_read_only() {
        cg.open_project_store_db_read_only().await?
    } else {
        cg.open_project_store_db().await?
    };
    let refs = db
        .search_ignored_dependency_refs(&query, candidate_limit)
        .await?;
    let refs = filter_by_scope(refs, scope_prefix, |unresolved| &unresolved.file_path);
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for unresolved in refs {
        let Some((module, symbol)) = parse_ignored_dependency_candidate(&unresolved.reference_name)
        else {
            continue;
        };
        let haystack = format!("{module} {symbol}").to_ascii_lowercase();
        if !haystack.contains(&query) {
            continue;
        }
        if !seen.insert((
            module.to_string(),
            symbol.to_string(),
            unresolved.file_path.clone(),
            unresolved.line,
        )) {
            continue;
        }
        candidates.push(json!({
            "module": module,
            "symbol": symbol,
            "import_file": unresolved.file_path,
            "line": user_line(unresolved.line),
        }));
        if candidates.len() >= candidate_limit {
            break;
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "message": "No indexed symbol matched, but project imports reference matching symbols from an ignored dependency. Keep node_modules ignored for normal sync; use bounded lazy dependency indexing for the listed module if this symbol is needed.",
        "candidates": candidates,
        "suggested_action": "lazy_index_ignored_dependency",
    })))
}

fn parse_ignored_dependency_candidate(reference_name: &str) -> Option<(&str, &str)> {
    reference_name.strip_prefix("npm:")?.split_once('#')
}

fn user_line(line: u32) -> u32 {
    line.saturating_add(1)
}
