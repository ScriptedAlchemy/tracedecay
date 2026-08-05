//! `tracedecay_diagnostics` — compiler and LSP diagnostics mapped to enclosing symbols.

use super::*;

fn diagnostics_scope_arg(args: &Value) -> Result<(&str, crate::diagnostics::Scope)> {
    use crate::diagnostics::Scope;

    let scope_str = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("workspace");

    let scope = match scope_str {
        "workspace" => Scope::Workspace,
        "package" => Scope::Package {
            name: required_diagnostics_scope_value(args, "package", "name")?,
        },
        "file" => Scope::File {
            path: required_diagnostics_scope_value(args, "file", "path")?,
        },
        other => {
            return Err(TraceDecayError::Config {
                message: format!("unknown scope '{other}'; expected workspace, package, or file"),
            });
        }
    };

    Ok((scope_str, scope))
}

fn required_diagnostics_scope_value(args: &Value, scope: &str, name: &str) -> Result<String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("scope='{scope}' requires a '{name}' argument"),
        })
        .map(str::to_string)
}

async fn enclosing_diagnostic_node(
    cg: &TraceDecay,
    spans_by_file: &mut HashMap<String, Vec<NodeSpan>>,
    file: &str,
    line_start: u32,
) -> Result<Option<String>> {
    if !spans_by_file.contains_key(file) {
        let spans = cg
            .get_nodes_by_file(file)
            .await?
            .into_iter()
            .map(|n| NodeSpan {
                start_line: n.start_line,
                end_line: n.end_line,
                qualified_name: n.qualified_name,
            })
            .collect();
        spans_by_file.insert(file.to_string(), spans);
    }

    Ok(spans_by_file
        .get(file)
        .and_then(|spans| enclosing_node_for_line(spans, line_start)))
}

/// Whether the diagnostics prewarm behaviour is enabled. Off by default: the
/// first `tracedecay_diagnostics` call on a cold Rust tree otherwise blocks for
/// minutes while cargo builds every dependency, which agents rationally avoid.
/// When enabled by the pinned resolved configuration snapshot, a cold tree
/// instead kicks a detached `cargo check` and returns a `warming` status
/// immediately. Legacy environment precedence is resolved before the snapshot
/// is published, never in this request path.
fn diagnostics_prewarm_enabled(config_flag: bool) -> bool {
    config_flag
}

/// Build the early-return `warming` payload for a cold prewarm. Factored out so
/// the warming path is unit-testable without spawning cargo.
fn diagnostics_warming_result(project_root: &std::path::Path, args: &Value) -> ToolResult {
    let target_dir = crate::diagnostics::rust_diagnostics_target_dir(project_root);
    let payload = json!({
        "status": "warming",
        "message": format!(
            "dependency build started (~minutes); re-call tracedecay_diagnostics after \
             it finishes, or run `cargo check` in your shell meanwhile. Build target: {}",
            target_dir.display()
        ),
        "target_dir": target_dir.display().to_string(),
        "diagnostic_count": 0,
    });
    generic_tool_result(Some(project_root), args, &payload, vec![])
}

/// Best-effort per-project session↔git correlation index health for the
/// diagnostics payload. Read-only and fail-open: a missing or unopenable store,
/// or absent correlation tables, is reported as an explicitly *empty* index
/// (with a remediation notice) rather than omitted — so an unpopulated
/// `session_git_spans` (which makes `tracedecay_sessions_for` silently return
/// nothing) is always visible here.
async fn session_correlation_health_json(
    session_db: Option<&crate::global_db::RegisteredGlobalDb>,
) -> Value {
    let health = match session_db {
        Some(db) => crate::store::GlobalDbGitCorrelationStore::new(db)
            .correlation_index_health()
            .await
            .ok(),
        None => None,
    };
    match health {
        Some(health) if health.tables_present => {
            let empty = health.is_empty();
            json!({
                "tables_present": true,
                "span_count": health.span_count,
                "commit_count": health.commit_count,
                "last_span_write": health.last_span_write,
                "backfill_watermark": health.backfill_watermark,
                "index_empty": empty,
                "notice": if empty {
                    "correlation index empty — `tracedecay_sessions_for` will return nothing until it is populated; bounded convergence runs on daemon startup, or run `tracedecay sessions git-sync` to schedule it now"
                } else {
                    "correlation index populated"
                },
            })
        }
        _ => json!({
            "tables_present": false,
            "span_count": 0,
            "commit_count": 0,
            "last_span_write": Value::Null,
            "backfill_watermark": Value::Null,
            "index_empty": true,
            "notice": "correlation index not yet created — `tracedecay_sessions_for` will return nothing until it is populated; bounded convergence runs on daemon startup, or run `tracedecay sessions git-sync` to schedule it now",
        }),
    }
}

pub(crate) async fn handle_diagnostics(
    cg: &TraceDecay,
    args: Value,
    diagnostics_cache: Option<&crate::diagnostics::DiagnosticsCache>,
    diagnostics_lsp: Option<&tokio::sync::Mutex<DiagnosticBroker>>,
    session_db: Option<&crate::global_db::RegisteredGlobalDb>,
) -> Result<ToolResult> {
    use crate::diagnostics::run_all;

    let (scope_str, scope) = diagnostics_scope_arg(&args)?;
    let project_root = cg.project_root().to_path_buf();

    // Cold-start avoidance: on a fresh tree the first cargo check builds every
    // dependency and blocks for minutes. When prewarm is enabled, spawn that
    // build detached and return a `warming` status immediately so the agent can
    // keep working and re-call once it is warm. Default-off preserves the
    // original blocking behaviour for callers who want the answer inline.
    if diagnostics_prewarm_enabled(cg.get_config().diagnostics_prewarm)
        && crate::diagnostics::is_rust_diagnostics_cold(&project_root)
    {
        crate::diagnostics::spawn_rust_diagnostics_prewarm(&project_root)?;
        return Ok(diagnostics_warming_result(&project_root, &args));
    }

    let mut diagnostics =
        if let Some(lsp_diagnostics) = lsp_file_diagnostics(cg, &scope, diagnostics_lsp).await? {
            lsp_diagnostics
        } else if let Some(cache) = diagnostics_cache {
            cache.run(&project_root, &scope).await?
        } else {
            run_all(&project_root, &scope).await?
        };

    if let crate::diagnostics::Scope::File { path } = &scope {
        diagnostics.retain(|d| d.file == *path);
    }

    let mut entries: Vec<Value> = Vec::with_capacity(diagnostics.len());
    let mut error_count = 0u64;
    let mut warning_count = 0u64;
    let mut spans_by_file: HashMap<String, Vec<NodeSpan>> = HashMap::new();

    for diag in &diagnostics {
        match diag.level.as_str() {
            "error" => error_count += 1,
            "warning" => warning_count += 1,
            _ => {}
        }

        let enclosing =
            enclosing_diagnostic_node(cg, &mut spans_by_file, &diag.file, diag.line_start).await?;

        entries.push(json!({
            "file": diag.file,
            "line_start": diag.line_start,
            "line_end": diag.line_end,
            "level": diag.level,
            "code": diag.code,
            "message": diag.message,
            "driver": diag.driver,
            "enclosing": enclosing,
        }));
    }

    let payload = json!({
        "scope": scope_str,
        "diagnostic_count": entries.len(),
        "error_count": error_count,
        "warning_count": warning_count,
        "diagnostics": entries,
        "session_correlation": session_correlation_health_json(session_db).await,
    });
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        unique_file_paths(diagnostics.iter().map(|d| d.file.as_str())),
    ))
}

async fn lsp_file_diagnostics(
    cg: &TraceDecay,
    scope: &crate::diagnostics::Scope,
    diagnostics_lsp: Option<&tokio::sync::Mutex<DiagnosticBroker>>,
) -> Result<Option<Vec<crate::diagnostics::Diagnostic>>> {
    let crate::diagnostics::Scope::File { path } = scope else {
        return Ok(None);
    };
    let Some(diagnostics_lsp) = diagnostics_lsp else {
        return Ok(None);
    };

    let adapter = {
        let broker = diagnostics_lsp.lock().await;
        broker
            .snapshot()
            .engines
            .into_iter()
            .filter_map(|engine| broker.adapter_for(&engine.language))
            .find(|adapter| {
                active_languages_for_files(
                    cg.project_root(),
                    std::slice::from_ref(adapter),
                    std::slice::from_ref(path),
                )
                .contains(&adapter.language)
            })
    };
    let Some(adapter) = adapter else {
        return Ok(None);
    };
    let language = adapter.language.clone();
    let documents = documents_for_adapter(cg.project_root(), &adapter, vec![path.clone()]).await?;
    if documents.is_empty() {
        return Ok(None);
    }

    let snapshot = {
        let mut broker = diagnostics_lsp.lock().await;
        if broker
            .refresh_documents(&language, documents, Duration::from_secs(2))
            .await
            .is_err()
        {
            return Ok(None);
        }
        broker.snapshot()
    };

    Ok(Some(
        snapshot
            .diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.file == *path)
            .map(lsp_diagnostic_to_compiler_diagnostic)
            .collect(),
    ))
}

fn lsp_diagnostic_to_compiler_diagnostic(
    diagnostic: CodeDiagnostic,
) -> crate::diagnostics::Diagnostic {
    crate::diagnostics::Diagnostic {
        file: diagnostic.file,
        line_start: diagnostic.line_start,
        line_end: diagnostic.line_end,
        level: match diagnostic.severity {
            BrokerDiagnosticSeverity::Error => "error",
            BrokerDiagnosticSeverity::Warning => "warning",
            BrokerDiagnosticSeverity::Information => "information",
            BrokerDiagnosticSeverity::Hint => "hint",
        }
        .to_string(),
        code: diagnostic.code.unwrap_or_default(),
        message: diagnostic.message,
        driver: "lsp",
    }
}

// ---------------------------------------------------------------------------
// tracedecay_constructors
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod diagnostics_warming_tests {
    use super::{diagnostics_prewarm_enabled, diagnostics_warming_result};
    use serde_json::{Value, json};
    use std::path::Path;

    #[test]
    fn prewarm_follows_resolved_config_snapshot() {
        assert!(!diagnostics_prewarm_enabled(false));
        assert!(diagnostics_prewarm_enabled(true));
    }

    #[test]
    fn warming_result_reports_status_and_target_dir() {
        let root = Path::new("/tmp/tracedecay-warming-proj");
        let result = diagnostics_warming_result(root, &json!({}));
        let text = result.value["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("warming"),
            "status should be surfaced: {text}"
        );
        assert!(
            text.to_lowercase().contains("re-call"),
            "should tell the agent to re-call: {text}"
        );
        // The private diagnostics target dir is namespaced by project id, so the
        // message must not leak a repo-local `target/`.
        assert!(
            text.contains("tracedecay-target"),
            "should point at the private diagnostics target dir: {text}"
        );
        assert!(
            !text.trim_start().starts_with('{'),
            "default output should be Markdown: {text}"
        );

        let json_result = diagnostics_warming_result(root, &json!({ "format": "json" }));
        let Some(json_text) = json_result.value["content"][0]["text"].as_str() else {
            panic!("format=json should include text content");
        };
        let json_payload: Value = serde_json::from_str(json_text)
            .unwrap_or_else(|err| panic!("format=json should stay parseable JSON: {err}"));
        assert_eq!(json_payload["status"], "warming");
        assert_eq!(json_payload["diagnostic_count"], 0);
    }
}
