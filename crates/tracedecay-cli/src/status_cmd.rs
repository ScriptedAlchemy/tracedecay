use std::future::Future;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::time::{Instant, timeout_at};
use tracedecay_contracts::retained_surfaces::{FactCommitOwnerV1, MemoryStatusV1};

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

/// Cache lifetimes of the two decorative worldwide-counter reads. The status
/// render always shows the cache; these only decide whether one bounded
/// refresh for the next invocation is worth starting.
const WORLDWIDE_TOTAL_MAX_AGE_SECS: i64 = 60;
const COUNTRY_FLAGS_MAX_AGE_SECS: i64 = 1800;

/// Which decorative caches have expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OnlineRefreshPlan {
    worldwide_total: bool,
    country_flags: bool,
}

impl OnlineRefreshPlan {
    fn for_cache(config: &tracedecay_session_memory::user_config::UserConfig, now: i64) -> Self {
        Self {
            worldwide_total: now - config.last_worldwide_fetch_at >= WORLDWIDE_TOTAL_MAX_AGE_SECS,
            country_flags: now - config.last_flags_fetch_at >= COUNTRY_FLAGS_MAX_AGE_SECS,
        }
    }

    fn is_needed(&self) -> bool {
        self.worldwide_total || self.country_flags
    }

    /// Runs the synchronous `ureq` reads this plan calls for. Blocking: must
    /// run on a blocking thread, never on the async executor.
    fn fetch(self) -> OnlineRefresh {
        OnlineRefresh {
            worldwide_total: self
                .worldwide_total
                .then(crate::cloud::fetch_worldwide_total)
                .flatten(),
            country_flags: if self.country_flags {
                crate::cloud::fetch_country_flags()
            } else {
                Vec::new()
            },
        }
    }
}

/// What a refresh brought back. `None` / empty means that endpoint did not
/// answer and its cache stands untouched.
#[derive(Debug, Default, PartialEq, Eq)]
struct OnlineRefresh {
    worldwide_total: Option<u64>,
    country_flags: Vec<String>,
}

impl OnlineRefresh {
    /// Writes the answered reads into the cache; returns whether anything
    /// changed and therefore needs saving.
    fn apply(
        self,
        config: &mut tracedecay_session_memory::user_config::UserConfig,
        now: i64,
    ) -> bool {
        let mut changed = false;
        if let Some(total) = self.worldwide_total {
            config.last_worldwide_total = total;
            config.last_worldwide_fetch_at = now;
            changed = true;
        }
        if !self.country_flags.is_empty() {
            config.cached_country_flags = self.country_flags;
            config.last_flags_fetch_at = now;
            changed = true;
        }
        changed
    }
}

/// Joins the refresh within the command deadline. Past the deadline the handle
/// is dropped and nothing is cached: only this caller writes the cache, and
/// the abandoned read ends on its own `ureq` timeout inside the runtime's
/// bounded shutdown rather than as a detached worker.
#[hotpath::measure(label = "cli.status.online", future = true)]
async fn await_online_refresh(
    deadline: Instant,
    refresh: tokio::task::JoinHandle<OnlineRefresh>,
) -> Option<OnlineRefresh> {
    match timeout_at(deadline, refresh).await {
        Ok(Ok(fresh)) => Some(fresh),
        Ok(Err(join_error)) => {
            tracing::debug!(error = %join_error, "worldwide counter refresh did not complete");
            None
        }
        Err(_) => None,
    }
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
        tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1,
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
    let show_online =
        should_fetch_online_status_embellishments(stdout_is_terminal) && upload_enabled;
    // The worldwide counter and country flags are decoration served from the
    // local cache: the render below never waits on the network. When a cache
    // has expired, one refresh for the next invocation starts here so its
    // round-trip overlaps the render, and is joined after it within the
    // command deadline.
    let refresh = show_online
        .then(|| OnlineRefreshPlan::for_cache(&config, now))
        .filter(OnlineRefreshPlan::is_needed)
        .map(|plan| tokio::task::spawn_blocking(move || plan.fetch()));
    let worldwide = show_online
        .then_some(config.last_worldwide_total)
        .filter(|total| *total > 0);
    let country_flags = if show_online {
        config.cached_country_flags.clone()
    } else {
        Vec::new()
    };
    hotpath::measure_block!("cli.status.render", {
        if should_print_status_logo(short, stdout_is_terminal) {
            // Tracked render of resources/logo.png; regenerate with
            // scripts/render-logo-ansi.sh when the artwork changes.
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

    if !tracedecay_configuration::is_in_gitignore(&project_path) {
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
    if let Some(refresh) = refresh
        && let Some(fresh) = await_online_refresh(deadline, refresh).await
        && fresh.apply(&mut config, now)
        && let Err(err) = config.save_if_exists()
    {
        eprintln!("warning: could not save tracedecay config: {err}");
    }
    if stdout_is_terminal {
        global::check_for_update(&mut config, false, true);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        COUNTRY_FLAGS_MAX_AGE_SECS, OnlineRefresh, OnlineRefreshPlan, WORLDWIDE_TOTAL_MAX_AGE_SECS,
        await_daemon_tool_result, await_online_refresh, reject_truncation_envelope,
        status_command_deadline_from, status_server_request_budget,
    };
    use serde_json::json;
    use std::time::Duration;
    use tokio::time::Instant;
    use tracedecay_session_memory::user_config::UserConfig;

    #[tokio::test]
    async fn hanging_online_refresh_settles_at_the_deadline_and_leaves_the_cache_alone() {
        let hang = Duration::from_millis(600);
        let budget = Duration::from_millis(100);
        let started = Instant::now();
        let refresh = tokio::task::spawn_blocking(move || {
            std::thread::sleep(hang);
            OnlineRefresh {
                worldwide_total: Some(7),
                country_flags: vec!["🇮🇸".to_owned()],
            }
        });

        let fresh = await_online_refresh(started + budget, refresh).await;
        let settled_after = started.elapsed();

        assert_eq!(
            fresh, None,
            "a read that outlives the budget yields nothing"
        );
        assert!(
            settled_after < hang,
            "settled after {settled_after:?}; the hanging read needs {hang:?}"
        );
        let mut config = UserConfig::default();
        let changed = fresh.is_some_and(|fresh| fresh.apply(&mut config, 1_000));
        assert!(!changed);
        assert_eq!(config.last_worldwide_total, 0);
        assert_eq!(config.last_worldwide_fetch_at, 0);
        assert!(config.cached_country_flags.is_empty());
    }

    #[tokio::test]
    async fn answered_online_refresh_within_budget_updates_the_cache() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let refresh = tokio::task::spawn_blocking(|| OnlineRefresh {
            worldwide_total: Some(42),
            country_flags: vec!["🇳🇴".to_owned()],
        });
        let fresh = await_online_refresh(deadline, refresh)
            .await
            .expect("answered within budget");
        let mut config = UserConfig::default();
        assert!(fresh.apply(&mut config, 1_000));
        assert_eq!(config.last_worldwide_total, 42);
        assert_eq!(config.last_worldwide_fetch_at, 1_000);
        assert_eq!(config.cached_country_flags, ["🇳🇴"]);
        assert_eq!(config.last_flags_fetch_at, 1_000);

        assert!(
            !OnlineRefresh::default().apply(&mut config, 2_000),
            "unanswered reads must not touch the cache or its timestamps"
        );
        assert_eq!(config.last_worldwide_fetch_at, 1_000);
        assert_eq!(config.last_flags_fetch_at, 1_000);
    }

    #[test]
    fn online_refresh_plan_follows_cache_age() {
        let mut config = UserConfig::default();
        let now = 10_000;
        config.last_worldwide_fetch_at = now - WORLDWIDE_TOTAL_MAX_AGE_SECS + 1;
        config.last_flags_fetch_at = now - COUNTRY_FLAGS_MAX_AGE_SECS + 1;
        let plan = OnlineRefreshPlan::for_cache(&config, now);
        assert!(!plan.is_needed(), "fresh caches start no refresh: {plan:?}");

        config.last_worldwide_fetch_at = now - WORLDWIDE_TOTAL_MAX_AGE_SECS;
        let plan = OnlineRefreshPlan::for_cache(&config, now);
        assert!(plan.worldwide_total && !plan.country_flags);

        config.last_flags_fetch_at = 0;
        let plan = OnlineRefreshPlan::for_cache(&config, now);
        assert!(plan.worldwide_total && plan.country_flags);
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
}
