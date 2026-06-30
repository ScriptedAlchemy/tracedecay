use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::dependency_imports::candidates_from_type_only_import;
use crate::errors::Result;
use crate::mcp::tools::render::{self, Md};
use crate::tracedecay::TraceDecay;

pub(super) fn should_check_ignored_dependency_hint(result_count: usize, limit: usize) -> bool {
    result_count == 0 || result_count < limit.clamp(1, 20)
}

pub(super) async fn ignored_dependency_hint(
    cg: &TraceDecay,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
) -> Result<Option<Value>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(None);
    }
    let candidate_limit = limit.clamp(1, 20);
    let db = if cg.is_read_only() {
        cg.open_project_store_db_read_only().await?
    } else {
        cg.open_project_store_db().await?
    };
    let query_lower = query.to_ascii_lowercase();
    let imports = db
        .dependency_import_uses(query, candidate_limit, scope_prefix)
        .await?;
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for candidate in imports.into_iter().flat_map(|import_use| {
        candidates_from_type_only_import(
            &import_use.signature,
            &import_use.module,
            &import_use.file_path,
            import_use.line,
        )
    }) {
        let haystack = format!("{} {}", candidate.module, candidate.symbol).to_ascii_lowercase();
        if !haystack.contains(&query_lower) {
            continue;
        }
        if !seen.insert((
            candidate.module.clone(),
            candidate.symbol.clone(),
            candidate.import_file.clone(),
            candidate.line,
        )) {
            continue;
        }
        candidates.push(json!({
            "module": candidate.module,
            "symbol": candidate.symbol,
            "import_file": candidate.import_file,
            "line": user_line(candidate.line),
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

pub(super) fn append_ignored_dependency_hint_md(md: &mut Md, value: &Value) {
    let Some(hint) = value.get("ignored_dependency_hint") else {
        return;
    };
    let msg = hint
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Matching ignored dependency candidates were found.");
    md.blank().heading(3, "Ignored Dependency Hint").line(msg);
    if let Some(candidates) = hint.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            let module = render::field_str(candidate, "module");
            let symbol = render::field_str(candidate, "symbol");
            let file = render::field_str(candidate, "import_file");
            let line = render::field_i64(candidate, "line");
            md.bullet(&format!(
                "`{module}` exports `{symbol}` referenced at {file}:{line}"
            ));
        }
    }
}

fn user_line(line: u32) -> u32 {
    line.saturating_add(1)
}
