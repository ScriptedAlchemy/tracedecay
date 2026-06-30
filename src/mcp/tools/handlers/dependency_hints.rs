use serde_json::{json, Value};

use crate::dependency_imports::candidates_from_type_only_import;
use crate::errors::Result;
use crate::mcp::tools::render::{self, Md};
use crate::tracedecay::TraceDecay;

pub(super) async fn ignored_dependency_hint(
    cg: &TraceDecay,
    query: &str,
    limit: usize,
) -> Result<Option<Value>> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Ok(None);
    }
    let limit = limit.clamp(1, 20);
    let db = cg.open_project_store_db().await?;
    let query_lower = query.to_ascii_lowercase();
    let candidates = db
        .dependency_import_uses(&query, limit)
        .await?
        .into_iter()
        .flat_map(|import_use| {
            candidates_from_type_only_import(
                &import_use.signature,
                &import_use.module,
                &import_use.file_path,
                import_use.line,
            )
        })
        .filter(|candidate| {
            candidate.symbol.to_ascii_lowercase().contains(&query_lower)
                || candidate.module.to_ascii_lowercase().contains(&query_lower)
        })
        .take(limit)
        .map(|candidate| {
            json!({
                "module": candidate.module,
                "symbol": candidate.symbol,
                "import_file": candidate.import_file,
                "line": user_line(candidate.line),
            })
        })
        .collect::<Vec<_>>();
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
