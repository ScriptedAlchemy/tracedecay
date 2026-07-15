use crate::{commands, current_unix_timestamp, global, resolve_cli_project_root};

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
            "legacy backfill complete: {}\n",
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
        if status.legacy_backfill_complete {
            "yes"
        } else {
            "no"
        },
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
    let project_path = resolve_cli_project_root(path, project_id, project_path).await?;
    if runtime {
        let result = commands::daemon_tool_json(
            Some(&project_path),
            "tracedecay_admin_project",
            serde_json::json!({ "action": "runtime_status", "json": json }),
        )
        .await?;
        let output = result
            .get("output")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "daemon runtime status response omitted output".to_string(),
            })?;
        print!("{output}");
        return Ok(());
    }
    let daemon_status = commands::daemon_tool_json(
        Some(&project_path),
        "tracedecay_status",
        serde_json::json!({ "format": "json" }),
    )
    .await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&daemon_status).unwrap_or_default()
        );
        return Ok(());
    }
    let stats: tracedecay::types::GraphStats = serde_json::from_value(daemon_status.clone())?;
    let accounting = commands::daemon_tool_json(
        Some(&project_path),
        "tracedecay_admin_project",
        serde_json::json!({ "action": "status_accounting" }),
    )
    .await?;
    let tokens_saved = accounting
        .get("tokens_saved")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon status accounting omitted token count".to_string(),
        })?;
    let global_tokens_saved = accounting
        .get("global_tokens_saved")
        .and_then(serde_json::Value::as_u64);
    let mut config = tracedecay::user_config::UserConfig::load();
    let now = current_unix_timestamp();
    let worldwide = if !config.upload_enabled {
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
    let country_flags = if !config.upload_enabled {
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
    if !short {
        print!("{}", include_str!("resources/logo.ansi"));
    }
    let branch_info = daemon_status
        .get("serving_branch")
        .and_then(serde_json::Value::as_str)
        .map(|branch| tracedecay::display::BranchInfo {
            branch: branch.to_string(),
            parent: daemon_status
                .get("parent_branch")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            is_fallback: daemon_status
                .get("branch_fallback")
                .and_then(serde_json::Value::as_bool)
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
        tracedecay::display::print_status_table(tracedecay::display::StatusTable {
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
        eprintln!(
            "\n\x1b[33mWarning: {dir_name} is not in .gitignore — \
             run `echo {dir_name} >> .gitignore` to exclude it from git.\x1b[0m"
        );
    }
    global::check_for_update(&mut config, false, true);
    Ok(())
}
