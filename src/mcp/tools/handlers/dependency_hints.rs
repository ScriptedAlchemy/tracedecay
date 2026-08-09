use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_usecases::tracedecay::{GraphRequestControl, GraphRuntimePort};

use crate::dependency_imports::DependencyImportCandidate;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::render::{self, Md};
use crate::tracedecay::TraceDecay;

pub(super) fn should_check_ignored_dependency_hint(result_count: usize, limit: usize) -> bool {
    result_count == 0 || result_count < limit.clamp(1, 20)
}

pub(super) fn lazy_indexing_requested(args: &Value) -> bool {
    args.get("lazy_index_ignored_dependencies")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) async fn ignored_dependency_hint(
    cg: &TraceDecay,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Option<Value>> {
    let candidates =
        ignored_dependency_candidates(cg, query, limit, scope_prefix, deadline, cancellation)
            .await?;
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "message": "No indexed symbol matched, but project imports reference matching symbols from an ignored dependency. Keep node_modules ignored for normal sync; use bounded lazy dependency indexing for the listed module if this symbol is needed.",
        "candidates": candidates.into_iter().map(|candidate| json!({
            "module": candidate.module,
            "symbol": candidate.symbol,
            "import_file": candidate.import_file,
            "line": user_line(candidate.line),
        })).collect::<Vec<_>>(),
        "suggested_action": "lazy_index_ignored_dependency",
    })))
}

pub(super) async fn lazy_index_ignored_dependency_candidates(
    cg: &TraceDecay,
    query: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Vec<String>> {
    if cg.is_read_only() {
        return Ok(Vec::new());
    }

    let candidates =
        ignored_dependency_candidates(cg, query, limit, scope_prefix, deadline, cancellation)
            .await?;
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    for candidate in candidates {
        if let Some(path) = candidate_entry_paths(cg.project_root(), &candidate.module)
            .into_iter()
            .next()
            && seen.insert(path.clone())
        {
            paths.push(path);
        }
    }
    GraphRuntimePort::lazy_index_ignored_dependency_files(
        cg,
        &paths,
        GraphRequestControl {
            deadline,
            cancellation,
        },
    )
    .await
}

async fn ignored_dependency_candidates(
    _cg: &TraceDecay,
    _query: &str,
    _limit: usize,
    _scope_prefix: Option<&str>,
    _deadline: Option<&tracedecay_application::Deadline>,
    _cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Vec<DependencyImportCandidate>> {
    Err(TraceDecayError::ProjectRoute {
        reason_code: "code-graph-import-evidence-unavailable".to_owned(),
        retryable: false,
        detail: "the verified code graph does not publish type-only import evidence".to_owned(),
    })
}

fn candidate_entry_paths(project_root: &Path, module: &str) -> Vec<String> {
    if !safe_module_path(module) {
        return Vec::new();
    }
    let base = format!("node_modules/{module}");
    [
        format!("{base}.d.ts"),
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}.js"),
        format!("{base}.jsx"),
        format!("{base}/index.d.ts"),
        format!("{base}/index.ts"),
        format!("{base}/index.tsx"),
        format!("{base}/index.js"),
        format!("{base}/index.jsx"),
    ]
    .into_iter()
    .filter(|path| project_root.join(path).is_file())
    .collect()
}

fn safe_module_path(module: &str) -> bool {
    !module.is_empty()
        && !module.starts_with('/')
        && !module.contains('\\')
        && !module
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tracedecay_application::{CancellationSignal, Deadline};
    use tracedecay_domain::UtcMicros;

    use super::*;
    use crate::tracedecay::TraceDecayOpenOptions;

    async fn indexed_import_graph() -> (TraceDecay, tempfile::TempDir) {
        let isolation = tempfile::tempdir().expect("isolated profile");
        let project = isolation.path().join("project");
        let profile = isolation.path().join("profile");
        fs::create_dir_all(project.join("src")).expect("source directory");
        fs::write(
            project.join("src/app.ts"),
            "import type { BranchOnly } from \"branch-pkg\";\nexport const value = 1;\n",
        )
        .expect("source file");
        let graph = TraceDecay::init_with_options(
            &project,
            TraceDecayOpenOptions {
                profile_root: Some(profile.clone()),
                global_db_path: Some(profile.join("global.db")),
            },
        )
        .await
        .expect("graph");
        graph.index_all().await.expect("index");
        (graph, isolation)
    }

    #[tokio::test]
    async fn dependency_hint_read_honors_transport_deadline() {
        let (graph, _isolation) = indexed_import_graph().await;
        let deadline = Deadline::new(UtcMicros(1)).expect("expired deadline");

        let error = ignored_dependency_hint(&graph, "BranchOnly", 5, None, Some(&deadline), None)
            .await
            .expect_err("expired dependency read");
        assert_eq!(
            error.project_route_context().map(|context| context.0),
            Some("dependency_hint_deadline_exceeded")
        );
    }

    #[tokio::test]
    async fn dependency_hint_read_uses_the_admitted_graph_runtime() {
        let (graph, _isolation) = indexed_import_graph().await;
        let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("live deadline");
        let cancellation =
            CancellationSignal::active("cancel.dependency-hint-active").expect("cancellation");

        let hint = ignored_dependency_hint(
            &graph,
            "BranchOnly",
            5,
            None,
            Some(&deadline),
            Some(&cancellation),
        )
        .await
        .expect("dependency read")
        .expect("dependency hint");
        assert_eq!(hint["candidates"][0]["module"], "branch-pkg");
        assert_eq!(hint["candidates"][0]["symbol"], "BranchOnly");
        assert_eq!(hint["candidates"][0]["import_file"], "src/app.ts");
    }
}
