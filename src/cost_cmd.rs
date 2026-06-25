use std::process;

use tracedecay::accounting::CostSummary;
use tracedecay::global_db::GlobalDb;

pub(crate) async fn handle_cost(
    range: String,
    by_model: bool,
    by_task: bool,
    export: Option<String>,
) -> tracedecay::errors::Result<()> {
    tracedecay::accounting::pricing::refresh_if_stale();

    let gdb = match GlobalDb::open().await {
        Some(db) => db,
        None => {
            eprintln!("Could not open global database.");
            process::exit(1);
        }
    };

    let ingest_stats = tracedecay::accounting::parser::ingest(&gdb).await;
    if ingest_stats.turns_inserted > 0 {
        eprintln!(
            "Ingested {} new turns from Claude Code sessions.",
            ingest_stats.turns_inserted
        );
    }

    let since = tracedecay::accounting::metrics::parse_range(&range);
    let tokens_saved = gdb.global_tokens_saved().await.unwrap_or(0);
    let summary = tracedecay::accounting::metrics::cost_summary(&gdb, since, tokens_saved).await;

    let Some(summary) = summary else {
        println!("No session data found. Use Claude Code and then run `tracedecay cost` to see spending.");
        return Ok(());
    };

    print_cost_summary(&gdb, &range, by_model, by_task, export.as_deref(), &summary).await;
    Ok(())
}

async fn print_cost_summary(
    gdb: &GlobalDb,
    range: &str,
    by_model: bool,
    by_task: bool,
    export: Option<&str>,
    summary: &CostSummary,
) {
    if let Some(fmt) = export {
        print_cost_export(fmt, range, by_model, by_task, summary);
    } else if by_model {
        print_model_table(summary);
    } else if by_task {
        print_task_table(summary);
    } else {
        print_default_summary(gdb, range, summary).await;
    }
}

fn print_cost_export(fmt: &str, range: &str, by_model: bool, by_task: bool, summary: &CostSummary) {
    match fmt {
        "json" => {
            let obj = serde_json::json!({
                "range": range,
                "total_cost_usd": summary.total_cost,
                "total_input_tokens": summary.total_input_tokens,
                "total_output_tokens": summary.total_output_tokens,
                "tokens_saved": summary.tokens_saved,
                "efficiency_ratio": summary.efficiency_ratio,
                "by_model": summary.by_model.iter().map(|(model, cost, tokens)| {
                    serde_json::json!({"model": model, "cost": cost, "tokens": tokens})
                }).collect::<Vec<_>>(),
                "by_category": summary.by_category.iter().map(|(category, cost, turns)| {
                    serde_json::json!({"category": category, "cost": cost, "turns": turns})
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
        }
        "csv" => print_cost_csv(summary, by_model, by_task),
        _ => eprintln!("Unknown export format '{fmt}'. Use 'json' or 'csv'."),
    }
}

fn print_cost_csv(summary: &CostSummary, by_model: bool, by_task: bool) {
    if by_model {
        println!("model,cost_usd,tokens");
        for (model, cost, tokens) in &summary.by_model {
            println!("{model},{cost:.4},{tokens}");
        }
    } else if by_task {
        println!("category,cost_usd,turns");
        for (category, cost, turns) in &summary.by_category {
            println!("{category},{cost:.4},{turns}");
        }
    } else {
        println!("total_cost_usd,input_tokens,output_tokens,tokens_saved,efficiency");
        println!(
            "{:.4},{},{},{},{:.4}",
            summary.total_cost,
            summary.total_input_tokens,
            summary.total_output_tokens,
            summary.tokens_saved,
            summary.efficiency_ratio
        );
    }
}

fn print_model_table(summary: &CostSummary) {
    let total = summary.total_cost.max(0.001);
    println!(
        "  {:<24} {:>10} {:>10} {:>6}",
        "Model", "Cost", "Tokens", "Share"
    );
    for (model, cost, tokens) in &summary.by_model {
        let share = cost / total * 100.0;
        let token_count = tracedecay::display::format_token_count(*tokens);
        println!(
            "  {:<24} {:>9} {:>10} {:>5.0}%",
            model,
            format!("${cost:.2}"),
            token_count,
            share
        );
    }
}

fn print_task_table(summary: &CostSummary) {
    println!("  {:<16} {:>10} {:>6}", "Category", "Cost", "Turns");
    for (category, cost, turns) in &summary.by_category {
        println!(
            "  {:<16} {:>9} {:>6}",
            category,
            format!("${cost:.2}"),
            turns
        );
    }
}

async fn print_default_summary(gdb: &GlobalDb, range: &str, summary: &CostSummary) {
    let today_since = tracedecay::accounting::metrics::parse_range("today");
    let today_cost = gdb.total_cost_since(today_since).await.unwrap_or(0.0);
    let today_breakdown = gdb
        .token_breakdown_since(today_since)
        .await
        .unwrap_or((0, 0, 0));

    println!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10}",
        "Period", "Cost", "Input", "Output", "Cache-hit"
    );
    print_cost_row(
        "Today",
        today_cost,
        today_breakdown.0,
        today_breakdown.1,
        today_breakdown.2,
    );
    print_cost_row(
        range,
        summary.total_cost,
        summary.total_input_tokens,
        summary.total_output_tokens,
        summary.total_cache_read_tokens,
    );

    if summary.tokens_saved > 0 {
        let saved = tracedecay::display::format_token_count(summary.tokens_saved);
        println!();
        println!(
            "  Savings  {} tokens ({:.0}% efficiency)",
            saved,
            summary.efficiency_ratio * 100.0
        );
    }
}

fn print_cost_row(label: &str, cost: f64, input: u64, output: u64, cache_read: u64) {
    let cache_pct = if input + cache_read > 0 {
        (cache_read as f64 / (input + cache_read) as f64) * 100.0
    } else {
        0.0
    };
    let input = tracedecay::display::format_token_count(input);
    let output = tracedecay::display::format_token_count(output);
    println!(
        "  {:<10} {:>9} {:>10} {:>10} {:>9.0}%",
        label,
        format!("${cost:.2}"),
        input,
        output,
        cache_pct
    );
}
