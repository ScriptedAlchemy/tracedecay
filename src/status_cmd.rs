use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use tracedecay::tracedecay::TraceDecay;

use crate::{commands, current_unix_timestamp, global, resolve_cli_project_root};

pub(crate) async fn largest_memory_bank_fact_count_at(
    db_path: &Path,
) -> tracedecay::errors::Result<usize> {
    let db = libsql::Builder::new_local(db_path).build().await?;
    let conn = db.connect()?;
    let mut rows = conn
        .query("SELECT COALESCE(MAX(fact_count), 0) FROM memory_banks", ())
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(0);
    };
    Ok(row.get::<i64>(0).unwrap_or(0).max(0) as usize)
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
            "legacy backfill complete: {}\n",
            "repair: missing_vectors_repaired={}  banks_rebuilt={}\n"
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
    let cg = if TraceDecay::has_initialized_store(&project_path).await {
        match TraceDecay::open(&project_path).await {
            Ok(cg) => cg,
            Err(_) => TraceDecay::open_read_only(&project_path).await?,
        }
    } else if !io::stdin().is_terminal() {
        eprintln!(
            "No TraceDecay index found at '{}'. Non-interactive: skipping index creation (run `tracedecay init`).",
            project_path.display()
        );
        return Ok(());
    } else {
        eprint!(
            "No TraceDecay index found at '{}'. Create one now? [Y/n] ",
            project_path.display()
        );
        io::stderr().flush().ok();
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer).map_err(|e| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to read stdin: {e}"),
            }
        })?;
        let answer = answer.trim();
        if answer.is_empty() || answer.eq_ignore_ascii_case("y") {
            commands::init_and_index(&project_path, &[], &[], false).await?
        } else {
            return Ok(());
        }
    };
    if runtime {
        let snap = tracedecay::runtime_telemetry::collect(&cg).await?;
        if json {
            println!("{}", tracedecay::runtime_telemetry::to_pretty_json(&snap));
        } else {
            print!("{}", tracedecay::runtime_telemetry::to_text_report(&snap));
        }
        return Ok(());
    }
    let stats = cg.get_stats().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&stats).unwrap_or_default()
        );
        return Ok(());
    }

    let tokens_saved = cg.get_tokens_saved().await.unwrap_or(0);
    let gdb = tracedecay::global_db::GlobalDb::open().await;
    let global_tokens_saved = match &gdb {
        Some(db) => {
            db.upsert(&project_path, tokens_saved).await;
            db.global_tokens_saved()
                .await
                .map(|total| total.saturating_sub(tokens_saved))
                .filter(|&other| other > 0)
        }
        None => None,
    };
    let mut config = tracedecay::user_config::UserConfig::load();
    let now = current_unix_timestamp();
    let worldwide = if now - config.last_worldwide_fetch_at < 60 {
        (config.last_worldwide_total > 0).then_some(config.last_worldwide_total)
    } else if let Some(total) = tracedecay::cloud::fetch_worldwide_total() {
        config.last_worldwide_total = total;
        config.last_worldwide_fetch_at = now;
        config.save_if_exists();
        Some(total)
    } else {
        (config.last_worldwide_total > 0).then_some(config.last_worldwide_total)
    };
    let country_flags = if now - config.last_flags_fetch_at < 1800 {
        config.cached_country_flags.clone()
    } else {
        let fresh = tracedecay::cloud::fetch_country_flags();
        if !fresh.is_empty() {
            config.cached_country_flags = fresh.clone();
            config.last_flags_fetch_at = now;
            config.save_if_exists();
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
    let branch_info = cg.active_branch().map(|_| {
        let ts_dir = tracedecay::config::get_tracedecay_dir(&project_path);
        let meta = tracedecay::branch_meta::load_branch_meta(&ts_dir);
        let has_tracking = meta.as_ref().is_some_and(|m| !m.branches.is_empty());
        let display_branch = if has_tracking {
            cg.serving_branch().unwrap_or("[single-db]").to_string()
        } else {
            "[single-db]".to_string()
        };
        let parent = meta.and_then(|m| m.branches.get(cg.serving_branch()?)?.parent.clone());
        tracedecay::display::BranchInfo {
            branch: display_branch,
            parent,
            is_fallback: cg.is_fallback(),
        }
    });
    if let Some(ref db) = gdb {
        tracedecay::accounting::parser::ingest(db).await;
    }
    let cost_info = match &gdb {
        Some(db) => {
            tracedecay::accounting::quick_cost_summary(
                db,
                tokens_saved,
                global_tokens_saved.unwrap_or(0),
            )
            .await
        }
        None => None,
    };
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
        tracedecay::display::print_status_table(
            &stats,
            tokens_saved,
            global_tokens_saved,
            worldwide,
            &country_flags,
            branch_info.as_ref(),
            cost_info.as_ref(),
            details,
        );
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
