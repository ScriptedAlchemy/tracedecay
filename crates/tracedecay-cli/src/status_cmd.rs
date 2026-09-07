use std::future::Future;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::time::{Instant, timeout_at};
use tracedecay_application::retained_surfaces::{FactCommitOwnerV1, MemoryStatusV1};

use crate::commands::reject_truncation_envelope;
use crate::{commands, current_unix_timestamp, global, resolve_cli_project_root};

/// Absolute wall-clock budget for one `tracedecay status` invocation, covering
/// project resolution and every daemon RPC. Override with
/// `TRACEDECAY_STATUS_DEADLINE_MS` (milliseconds) for tests. Values above 24h
/// fail closed so the budget cannot exceed the supported monotonic range.
///
/// The command stays alive after the carried server deadline so the daemon's
/// typed operation receipt wins the race against the CLI backstop.
const STATUS_RESPONSE_MARGIN: Duration = Duration::from_secs(15);
const MAX_STATUS_COMMAND_DEADLINE: Duration = Duration::from_hours(24);
const STATUS_DEADLINE_ENV: &str = "TRACEDECAY_STATUS_DEADLINE_MS";

fn default_status_command_deadline() -> Duration {
    tracedecay_daemon_protocol::DEFAULT_DAEMON_OPERATION_DEADLINE
        .saturating_add(STATUS_RESPONSE_MARGIN)
}

fn status_command_deadline_from(raw: Option<&str>) -> tracedecay_domain::errors::Result<Duration> {
    let deadline = raw
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or_else(default_status_command_deadline, Duration::from_millis);
    if deadline > MAX_STATUS_COMMAND_DEADLINE {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "{STATUS_DEADLINE_ENV} exceeds the supported monotonic deadline range"
            ),
        });
    }
    Ok(deadline)
}

fn status_command_deadline() -> tracedecay_domain::errors::Result<Duration> {
    let raw = std::env::var(STATUS_DEADLINE_ENV).ok();
    status_command_deadline_from(raw.as_deref())
}

fn status_server_request_budget(command_budget: Duration) -> Duration {
    command_budget
        .saturating_sub(STATUS_RESPONSE_MARGIN)
        .min(tracedecay_daemon_protocol::DEFAULT_DAEMON_OPERATION_DEADLINE)
}

async fn await_daemon_tool_result<T>(
    response_deadline: Instant,
    tool_name: &str,
    response: impl Future<Output = tracedecay_domain::errors::Result<T>>,
) -> tracedecay_domain::errors::Result<T> {
    timeout_at(response_deadline, response).await.map_err(|_| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "timed out waiting for daemon tool {tool_name} before status deadline"
            ),
        }
    })?
}

fn should_print_status_logo(short: bool, stdout_is_terminal: bool) -> bool {
    !short && stdout_is_terminal
}

fn should_fetch_online_status_embellishments(stdout_is_terminal: bool) -> bool {
    stdout_is_terminal
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
    response_deadline: Instant,
    request_deadline: Instant,
    project_path: &Path,
    tool_name: &str,
    arguments: Value,
) -> tracedecay_domain::errors::Result<Value> {
    // The shorter deadline rides inside the call so the daemon can settle a
    // typed terminal. The command deadline is only the response backstop.
    await_daemon_tool_result(
        response_deadline,
        tool_name,
        commands::daemon_tool_json_until(
            request_deadline,
            Some(project_path),
            tool_name,
            arguments,
        ),
    )
    .await
}

pub(crate) fn format_memory_status_report(status: &MemoryStatusV1) -> String {
    let owner = match &status.owner {
        FactCommitOwnerV1::Profile => "profile".to_owned(),
        FactCommitOwnerV1::Project { project_id } => format!("project:{}", project_id.as_str()),
    };
    format!(
        concat!(
            "Holographic memory status\n",
            "owner: {}\n",
            "facts: {}\n",
            "entities: {}\n",
            "algebra: {}\n",
            "hrr dim: {}\n",
            "estimated capacity: {}\n",
            "below recall floor: {}\n",
            "helpful feedback: {}\n",
            "unhelpful feedback: {}\n",
            "trust buckets: <0.25={}  0.25-0.50={}  0.50-0.75={}  0.75-1.00={}\n",
            "feedback funnel: retrieved={} accessed={} facts_retrieved={} facts_rated={} feedback_total={} seen:feedback={}\n"
        ),
        owner,
        status.fact_count,
        status.entity_count,
        status.algebra.name,
        status.algebra.hrr_dim,
        status.algebra.estimated_capacity,
        status.below_default_recall_threshold_count,
        status.helpful_count,
        status.unhelpful_count,
        status.trust_0_025_count,
        status.trust_025_050_count,
        status.trust_050_075_count,
        status.trust_075_100_count,
        status.feedback_funnel.retrieval_count_total,
        status.feedback_funnel.access_count_total,
        status.feedback_funnel.retrieved_fact_count,
        status.feedback_funnel.rated_fact_count,
        status.feedback_funnel.feedback_total,
        status
            .feedback_funnel
            .seen_to_feedback_ratio
            .map_or_else(|| "n/a".to_string(), |ratio| format!("{ratio}:1")),
    )
}

#[hotpath::measure(label = "cli.status.dispatch", future = true)]
pub(crate) async fn handle_status_command(
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
    json: bool,
    short: bool,
    runtime: bool,
) -> tracedecay_domain::errors::Result<()> {
    let budget = status_command_deadline()?;
    let started_at = Instant::now();
    let deadline = started_at.checked_add(budget).ok_or_else(|| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "status deadline exceeds the supported monotonic range".to_owned(),
        }
    })?;
    let server_deadline = started_at
        .checked_add(status_server_request_budget(budget))
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "status server deadline exceeds the supported monotonic range".to_owned(),
        })?;
    timeout_at(
        deadline,
        handle_status_command_within(
            deadline,
            server_deadline,
            path,
            project_id,
            project_path,
            json,
            short,
            runtime,
        ),
    )
    .await
    .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
        message: format!(
            "status did not complete within {}s; the daemon may still be \
             starting or opening this project — retry, or raise \
             {STATUS_DEADLINE_ENV}",
            budget.as_secs()
        ),
    })?
}

#[allow(clippy::too_many_arguments)]
async fn handle_status_command_within(
    deadline: Instant,
    server_deadline: Instant,
    path: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
    json: bool,
    short: bool,
    runtime: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_path = resolve_cli_project_root(path, project_id, project_path).await?;
    if runtime {
        let result = daemon_tool_json_within(
            deadline,
            server_deadline,
            &project_path,
            "tracedecay_runtime",
            serde_json::json!({ "format": "json" }),
        )
        .await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            let snapshot: tracedecay_session_memory::runtime_telemetry::RuntimeSnapshot =
                serde_json::from_value(result)?;
            print!(
                "{}",
                tracedecay_session_memory::runtime_telemetry::to_text_report(&snapshot)
            );
        }
        return Ok(());
    }
    let daemon_status = daemon_tool_json_within(
        deadline,
        server_deadline,
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
    // Decode the exact wire types the daemon route serialized. Both sides use
    // the same Rust contracts (`GenerationCensusSnapshot`,
    // `CodeIndexWorktreeFreshnessV1`), so absence or drift is a typed decode
    // failure rather than a silently defaulted table.
    let census: tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot =
        serde_json::from_value(daemon_status.get("graph_statistics").cloned().ok_or_else(
            || tracedecay_domain::errors::TraceDecayError::Config {
                message: "daemon status response omitted graph_statistics".to_string(),
            },
        )?)?;
    let freshness: Option<
        tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1,
    > = daemon_status
        .get("code_index_freshness")
        .and_then(|freshness| freshness.get("worktree"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    let accounting = daemon_tool_json_within(
        deadline,
        server_deadline,
        &project_path,
        "tracedecay_admin_project",
        serde_json::json!({ "action": "status_accounting" }),
    )
    .await?;
    reject_truncation_envelope(&accounting, "tracedecay_admin_project")?;
    let tokens_saved = accounting
        .get("tokens_saved")
        .and_then(Value::as_u64)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
            message: "daemon status accounting omitted token count".to_string(),
        })?;
    let global_tokens_saved = accounting
        .get("global_tokens_saved")
        .and_then(Value::as_u64);
    let upload_enabled = timeout_at(
        deadline,
        commands::canonical_upload_enabled(&project_path),
    )
    .await
    .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
        message:
            "timed out waiting for canonical worldwide-counter upload setting before status deadline"
                .to_string(),
    })??;
    let mut config = tracedecay_session_memory::user_config::UserConfig::load();
    let now = current_unix_timestamp();
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let stderr_is_terminal = std::io::stderr().is_terminal();
    let fetch_online = should_fetch_online_status_embellishments(stdout_is_terminal);
    // The worldwide-counter and country-flag embellishments are blocking
    // network fetches; timed apart from the daemon round-trips so a slow
    // `tracedecay status` can name which of the two it is waiting on.
    let (worldwide, country_flags) = hotpath::measure_block!("cli.status.online", {
        let worldwide = if !fetch_online || !upload_enabled {
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
        let country_flags = if !fetch_online || !upload_enabled {
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
        (worldwide, country_flags)
    });
    hotpath::measure_block!("cli.status.render", {
        if should_print_status_logo(short, stdout_is_terminal) {
            print!("{}", include_str!("resources/logo.ansi"));
        }
        let branch_info = daemon_status
            .get("serving_branch")
            .and_then(Value::as_str)
            .map(|branch| crate::display::BranchInfo {
                branch: branch.to_string(),
                parent: daemon_status
                    .get("parent_branch")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                is_fallback: false,
            });
        let cost_info = None;
        if short {
            crate::display::print_status_header(
                &census,
                freshness.as_ref(),
                tokens_saved,
                global_tokens_saved,
                worldwide,
                &country_flags,
                branch_info.as_ref(),
                cost_info.as_ref(),
            );
        } else {
            crate::display::print_status_table_with(crate::display::StatusTable {
                census: &census,
                freshness: freshness.as_ref(),
                tokens_saved,
                global_tokens_saved,
                worldwide,
                country_flags: &country_flags,
                branch_info: branch_info.as_ref(),
                cost_info: cost_info.as_ref(),
            });
        }
    });

    // A parked deterministic contract violation must be visible on the plain
    // status journey, not only inside the JSON payload: name the exact reason
    // and the operator remediation beside the "parked" staleness row.
    if let Some(parked) = freshness
        .as_ref()
        .and_then(|freshness| freshness.parked.as_ref())
    {
        if stderr_is_terminal {
            eprintln!(
                "\n\x1b[33mWarning: code-index background convergence is parked: {}\n{}\x1b[0m",
                parked.reason, parked.remediation
            );
        } else {
            eprintln!(
                "\nWarning: code-index background convergence is parked: {}\n{}",
                parked.reason, parked.remediation
            );
        }
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
        await_daemon_tool_result, compact_status_tool_args, reject_truncation_envelope,
        should_fetch_online_status_embellishments, should_print_status_logo,
        status_command_deadline_from, status_server_request_budget,
    };
    use serde_json::json;
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn status_deadline_keeps_response_margin_beyond_server_budget() {
        let default = status_command_deadline_from(None).expect("default status deadline");
        assert_eq!(default, Duration::from_secs(45));
        assert_eq!(
            status_server_request_budget(default),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn status_deadline_boundaries_preserve_override_and_maximum() {
        assert_eq!(
            status_command_deadline_from(Some("0")).expect("zero falls back"),
            Duration::from_secs(45)
        );
        assert_eq!(
            status_command_deadline_from(Some("14999")).expect("sub-margin override"),
            Duration::from_millis(14_999)
        );
        assert_eq!(
            status_server_request_budget(Duration::from_millis(14_999)),
            Duration::ZERO
        );
        assert_eq!(
            status_command_deadline_from(Some("86400000")).expect("24h maximum"),
            Duration::from_hours(24)
        );
        assert!(
            status_command_deadline_from(Some("86400001")).is_err(),
            "an override above 24h must fail closed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn typed_server_failure_beats_cli_response_timeout() {
        let started_at = Instant::now();
        let server_deadline = started_at + Duration::from_secs(30);
        let response_deadline = started_at + Duration::from_secs(45);
        let result = await_daemon_tool_result(response_deadline, "tracedecay_status", async move {
            tokio::time::sleep_until(server_deadline).await;
            Err::<(), _>(tracedecay_domain::errors::TraceDecayError::project_route(
                "status_deadline_exceeded",
                true,
                "typed server deadline receipt",
            ))
        })
        .await
        .expect_err("server deadline must be reported");

        assert_eq!(
            result
                .project_route_context()
                .map(|(reason, retryable, _)| (reason, retryable)),
            Some(("status_deadline_exceeded", true))
        );
        assert!(Instant::now() < response_deadline);
    }

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
        let err = reject_truncation_envelope(&envelope, "tracedecay_status").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("truncated JSON"));
        assert!(message.contains("20000"));
        assert!(message.contains("rh_test"));
        assert!(
            reject_truncation_envelope(&json!({ "node_count": 1 }), "tracedecay_status").is_ok()
        );
        assert!(
            reject_truncation_envelope(
                &json!({ "truncated": true, "matches": [] }),
                "tracedecay_status",
            )
            .is_ok()
        );
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
