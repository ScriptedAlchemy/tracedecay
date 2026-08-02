use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::time::{Instant, timeout_at};

use crate::{commands, current_unix_timestamp, global, resolve_cli_project_root};

/// Absolute wall-clock budget for one `tracedecay status` invocation, covering
/// project resolution and every daemon RPC. Override with
/// `TRACEDECAY_STATUS_DEADLINE_MS` (milliseconds) for tests and dogfood.
const DEFAULT_STATUS_COMMAND_DEADLINE: Duration = Duration::from_secs(30);

fn status_command_deadline() -> Duration {
    std::env::var("TRACEDECAY_STATUS_DEADLINE_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_STATUS_COMMAND_DEADLINE)
}

fn should_print_status_logo(short: bool, stdout_is_terminal: bool) -> bool {
    !short && stdout_is_terminal
}

fn should_fetch_online_status_embellishments(stdout_is_terminal: bool) -> bool {
    stdout_is_terminal
}

fn is_truncation_envelope(value: &Value) -> bool {
    value.get("truncated").and_then(Value::as_bool) == Some(true)
        && value
            .get("original_chars")
            .and_then(Value::as_u64)
            .is_some()
        && value.get("preview").and_then(Value::as_str).is_some()
}

fn reject_truncation_envelope(value: &Value, tool_name: &str) -> tracedecay::errors::Result<()> {
    if !is_truncation_envelope(value) {
        return Ok(());
    }
    let original_chars = value.get("original_chars").and_then(Value::as_u64);
    let handle = value.get("handle").and_then(Value::as_str);
    let message = match (original_chars, handle) {
        (Some(chars), Some(handle)) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars); \
             recover with tracedecay_retrieve handle={handle}"
        ),
        (Some(chars), None) => format!(
            "daemon tool {tool_name} returned truncated JSON ({chars} chars) \
             without a retrieval handle"
        ),
        _ => format!("daemon tool {tool_name} returned truncated JSON"),
    };
    Err(tracedecay::errors::TraceDecayError::Config { message })
}

/// Compact CLI status args: keep graph identity fields while skipping the
/// expensive optional diagnostics that commonly push responses over the
/// semantic truncation envelope.
fn compact_status_tool_args() -> Value {
    serde_json::json!({
        "format": "json",
        "include_branch_diagnostics": false,
        "include_storage_health": false,
        "include_session_ingest": false,
        "include_staleness": false,
    })
}

async fn daemon_tool_json_within(
    deadline: Instant,
    project_path: &Path,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<Value> {
    timeout_at(
        deadline,
        commands::daemon_tool_json(Some(project_path), tool_name, arguments),
    )
    .await
    .map_err(|_| tracedecay::errors::TraceDecayError::Config {
        message: format!("timed out waiting for daemon tool {tool_name} before status deadline"),
    })?
}

pub(crate) fn format_memory_status_report(
    status: &tracedecay::memory::types::MemoryStatus,
    largest_bank_facts: usize,
) -> String {
    let capacity = status.estimated_capacity.max(1);
    let utilization_pct = largest_bank_facts as f64 / capacity as f64 * 100.0;
    format!(
        concat!(
            "Holographic memory status\n",
            "facts: {}\n",
            "entities: {}\n",
            "banks: {}\n",
            "algebra: {}\n",
            "hrr dim: {}\n",
            "capacity / bank: {}\n",
            "largest bank utilization: {}/{} ({:.1}%)\n",
            "below recall floor: {}\n",
            "missing vectors: {}\n",
            "helpful feedback: {}\n",
            "unhelpful feedback: {}\n",
            "trust buckets: <0.25={}  0.25-0.50={}  0.50-0.75={}  0.75-1.00={}\n",
            "repair: missing_vectors_repaired={}  banks_rebuilt={}\n",
            "feedback funnel: retrieved={} accessed={} facts_retrieved={} facts_rated={} feedback_total={} seen:feedback={}\n"
        ),
        status.fact_count,
        status.entity_count,
        status.bank_count,
        status.algebra_name,
        status.hrr_dim,
        status.estimated_capacity,
        largest_bank_facts,
        status.estimated_capacity,
        utilization_pct,
        status.below_default_recall_threshold_count,
        status.missing_vector_count,
        status.helpful_count,
        status.unhelpful_count,
        status.trust_0_025_count,
        status.trust_025_050_count,
        status.trust_050_075_count,
        status.trust_075_100_count,
        status.repair.missing_vectors_repaired,
        status.repair.banks_rebuilt,
        status.feedback_funnel.retrieval_count_total,
        status.feedback_funnel.access_count_total,
        status.feedback_funnel.retrieved_fact_count,
        status.feedback_funnel.rated_fact_count,
        status.feedback_funnel.feedback_total,
        status
            .feedback_funnel
            .seen_to_feedback_ratio
            .map_or_else(|| "dead".to_string(), |ratio| format!("{ratio}:1")),
    )
}

pub(crate) async fn handle_status_command(
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
    json: bool,
    short: bool,
    details: bool,
    runtime: bool,
) -> tracedecay::errors::Result<()> {
    let deadline = Instant::now() + status_command_deadline();
    timeout_at(
        deadline,
        handle_status_command_within(
            deadline,
            path,
            project_id,
            project_path,
            json,
            short,
            details,
            runtime,
        ),
    )
    .await
    .map_err(|_| tracedecay::errors::TraceDecayError::Config {
        message: "timed out waiting for status before deadline".to_string(),
    })?
}

#[allow(clippy::too_many_arguments)]
async fn handle_status_command_within(
    deadline: Instant,
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
    json: bool,
    short: bool,
    details: bool,
    runtime: bool,
) -> tracedecay::errors::Result<()> {
    let project_path = resolve_cli_project_root(path, project_id, project_path).await?;
    if runtime {
        let result = daemon_tool_json_within(
            deadline,
            &project_path,
            "tracedecay_admin_project",
            serde_json::json!({ "action": "runtime_status", "json": json }),
        )
        .await?;
        let output = result
            .get("output")
            .and_then(Value::as_str)
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "daemon runtime status response omitted output".to_string(),
            })?;
        print!("{output}");
        return Ok(());
    }
    let daemon_status = daemon_tool_json_within(
        deadline,
        &project_path,
        "tracedecay_status",
        compact_status_tool_args(),
    )
    .await?;
    reject_truncation_envelope(&daemon_status, "tracedecay_status")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&daemon_status)?);
        return Ok(());
    }
    let stats: tracedecay::types::GraphStats = serde_json::from_value(daemon_status.clone())?;
    let accounting = daemon_tool_json_within(
        deadline,
        &project_path,
        "tracedecay_admin_project",
        serde_json::json!({ "action": "status_accounting" }),
    )
    .await?;
    reject_truncation_envelope(&accounting, "tracedecay_admin_project")?;
    let tokens_saved = accounting
        .get("tokens_saved")
        .and_then(Value::as_u64)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon status accounting omitted token count".to_string(),
        })?;
    let global_tokens_saved = accounting
        .get("global_tokens_saved")
        .and_then(Value::as_u64);
    let mut config = tracedecay::user_config::UserConfig::load();
    let now = current_unix_timestamp();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let stderr_is_terminal = std::io::stderr().is_terminal();
    let fetch_online = should_fetch_online_status_embellishments(stdout_is_terminal);
    let worldwide = if !fetch_online || !config.upload_enabled {
        None
    } else if now - config.last_worldwide_fetch_at < 60 {
        (config.last_worldwide_total > 0).then_some(config.last_worldwide_total)
    } else if let Some(total) = tracedecay::cloud::fetch_worldwide_total() {
        config.last_worldwide_total = total;
        config.last_worldwide_fetch_at = now;
        if let Err(err) = config.save_if_exists() {
            eprintln!("warning: could not save tracedecay config: {err}");
        }
        Some(total)
    } else {
        (config.last_worldwide_total > 0).then_some(config.last_worldwide_total)
    };
    let country_flags = if !fetch_online || !config.upload_enabled {
        Vec::new()
    } else if now - config.last_flags_fetch_at < 1800 {
        config.cached_country_flags.clone()
    } else {
        let fresh = tracedecay::cloud::fetch_country_flags();
        if !fresh.is_empty() {
            config.cached_country_flags = fresh.clone();
            config.last_flags_fetch_at = now;
            if let Err(err) = config.save_if_exists() {
                eprintln!("warning: could not save tracedecay config: {err}");
            }
        }
        if fresh.is_empty() && !config.cached_country_flags.is_empty() {
            config.cached_country_flags.clone()
        } else {
            fresh
        }
    };
    if should_print_status_logo(short, stdout_is_terminal) {
        print!("{}", include_str!("resources/logo.ansi"));
    }
    let branch_info = daemon_status
        .get("serving_branch")
        .and_then(Value::as_str)
        .map(|branch| tracedecay::display::BranchInfo {
            branch: branch.to_string(),
            parent: daemon_status
                .get("parent_branch")
                .and_then(Value::as_str)
                .map(str::to_string),
            is_fallback: daemon_status
                .get("branch_fallback")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    let cost_info = None;
    if short {
        tracedecay::display::print_status_header(
            &stats,
            tokens_saved,
            global_tokens_saved,
            worldwide,
            &country_flags,
            branch_info.as_ref(),
            cost_info.as_ref(),
        );
    } else {
        tracedecay::display::print_status_table_with(tracedecay::display::StatusTable {
            stats: &stats,
            tokens_saved,
            global_tokens_saved,
            worldwide,
            country_flags: &country_flags,
            branch_info: branch_info.as_ref(),
            cost_info: cost_info.as_ref(),
            details,
        });
    }

    if !tracedecay::config::is_in_gitignore(&project_path) {
        let dir_name = tracedecay::config::active_data_dir_name(&project_path);
        if stderr_is_terminal {
            eprintln!(
                "\n\x1b[33mWarning: {dir_name} is not in .gitignore — \
                 run `echo {dir_name} >> .gitignore` to exclude it from git.\x1b[0m"
            );
        } else {
            eprintln!(
                "\nWarning: {dir_name} is not in .gitignore — \
                 run `echo {dir_name} >> .gitignore` to exclude it from git."
            );
        }
    }
    if stdout_is_terminal {
        global::check_for_update(&mut config, false, true);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        compact_status_tool_args, is_truncation_envelope, reject_truncation_envelope,
        should_fetch_online_status_embellishments, should_print_status_logo,
    };
    use serde_json::json;

    #[test]
    fn status_logo_requires_interactive_stdout() {
        assert!(should_print_status_logo(false, true));
        assert!(!should_print_status_logo(true, true));
        assert!(!should_print_status_logo(false, false));
    }

    #[test]
    fn online_embellishments_require_interactive_stdout() {
        assert!(should_fetch_online_status_embellishments(true));
        assert!(!should_fetch_online_status_embellishments(false));
    }

    #[test]
    fn truncation_envelope_is_detected_and_rejected() {
        let envelope = json!({
            "truncated": true,
            "original_chars": 20_000,
            "preview": "{}",
            "handle": "rh_test",
        });
        assert!(is_truncation_envelope(&envelope));
        let err = reject_truncation_envelope(&envelope, "tracedecay_status").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("truncated JSON"));
        assert!(message.contains("20000"));
        assert!(message.contains("rh_test"));
        assert!(!is_truncation_envelope(&json!({ "node_count": 1 })));
        assert!(!is_truncation_envelope(
            &json!({ "truncated": true, "matches": [] })
        ));
    }

    #[test]
    fn compact_status_args_disable_expensive_diagnostics() {
        let args = compact_status_tool_args();
        assert_eq!(args["format"], "json");
        assert_eq!(args["include_branch_diagnostics"], false);
        assert_eq!(args["include_storage_health"], false);
        assert_eq!(args["include_session_ingest"], false);
        assert_eq!(args["include_staleness"], false);
    }
}
