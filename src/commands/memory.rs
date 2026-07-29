use std::io::{self, Read};

use crate::cli::MemoryAction;

use super::daemon::daemon_tool_json;

pub(crate) async fn handle_memory_action(action: MemoryAction) -> tracedecay::errors::Result<()> {
    match action {
        MemoryAction::Status { .. } => unreachable!("memory status is handled in main.rs dispatch"),
        MemoryAction::Curate {
            apply,
            llm,
            llm_ops,
            max_clusters,
            min_confidence,
            path,
        } => {
            let resolved = super::scope::resolve_project_scope(
                tracedecay::config::resolve_path_with_discovery(path),
            )
            .await?;
            let llm_ops_value = match llm_ops {
                Some(source) => Some(read_llm_ops_payload(&source)?),
                None => None,
            };
            let report = daemon_tool_json(
                Some(&resolved.project_path),
                "tracedecay_admin_project",
                serde_json::json!({
                    "action": "memory_curate",
                    "apply": apply,
                    "llm": llm,
                    "llm_ops": llm_ops_value,
                    "max_clusters": max_clusters,
                    "min_confidence": min_confidence,
                }),
            )
            .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
        }
    }
    Ok(())
}

/// Reads the `--llm-ops` payload from a file path or stdin (`-`).
fn read_llm_ops_payload(source: &str) -> tracedecay::errors::Result<serde_json::Value> {
    let text = if source == "-" {
        let mut buf = String::new();
        io::stdin().lock().read_to_string(&mut buf).map_err(|e| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to read --llm-ops from stdin: {e}"),
            }
        })?;
        buf
    } else {
        std::fs::read_to_string(source).map_err(|e| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to read --llm-ops file {source}: {e}"),
            }
        })?
    };
    serde_json::from_str(&text).map_err(|e| tracedecay::errors::TraceDecayError::Config {
        message: format!("--llm-ops payload is not valid JSON: {e}"),
    })
}
