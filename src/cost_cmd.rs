use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay::accounting::CostSummary;

pub(crate) async fn handle_cost(
    range: String,
    by_model: bool,
    by_task: bool,
    export: Option<String>,
) -> tracedecay::errors::Result<()> {
    let payload = call_cost_admin(&range).await?;
    let ingest_stats = &payload["ingest"];
    if ingest_stats["turns_inserted"].as_u64().unwrap_or(0) > 0 {
        eprintln!(
            "Ingested {} new turns from Claude Code sessions.",
            ingest_stats["turns_inserted"].as_u64().unwrap_or(0)
        );
    }
    if payload.get("summary").is_none_or(Value::is_null) {
        println!(
            "No session data found. Use Claude Code and then run `tracedecay cost` to see spending."
        );
        return Ok(());
    }
    let summary: CostSummaryPayload = serde_json::from_value(payload["summary"].clone())?;
    let summary = summary.into();

    print_cost_summary(
        &payload["today"],
        &range,
        by_model,
        by_task,
        export.as_deref(),
        &summary,
    );
    Ok(())
}

fn print_cost_summary(
    today: &Value,
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
        print_default_summary(today, range, summary);
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

fn print_default_summary(today: &Value, range: &str, summary: &CostSummary) {
    println!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10}",
        "Period", "Cost", "Input", "Output", "Cache-hit"
    );
    print_cost_row(
        "Today",
        today["cost"].as_f64().unwrap_or(0.0),
        today["input_tokens"].as_u64().unwrap_or(0),
        today["output_tokens"].as_u64().unwrap_or(0),
        today["cache_read_tokens"].as_u64().unwrap_or(0),
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

#[derive(Deserialize)]
struct CostSummaryPayload {
    total_cost: f64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    by_model: Vec<(String, f64, u64)>,
    by_category: Vec<(String, f64, u64)>,
    tokens_saved: u64,
    efficiency_ratio: f64,
}

impl From<CostSummaryPayload> for CostSummary {
    fn from(value: CostSummaryPayload) -> Self {
        Self {
            total_cost: value.total_cost,
            total_input_tokens: value.total_input_tokens,
            total_output_tokens: value.total_output_tokens,
            total_cache_read_tokens: value.total_cache_read_tokens,
            by_model: value.by_model,
            by_category: value.by_category,
            tokens_saved: value.tokens_saved,
            efficiency_ratio: value.efficiency_ratio,
        }
    }
}

async fn call_cost_admin(range: &str) -> tracedecay::errors::Result<Value> {
    let cwd = std::env::current_dir()?;
    let project_root = tracedecay::config::discover_project_root(&cwd);
    let handshake =
        tracedecay::daemon::DaemonHandshake::for_current_client(project_root, None, false, false)?;
    let result = tracedecay::daemon::call_default_tool(
        &handshake,
        "tracedecay_admin_cli",
        json!({ "action": "cost_summary", "range": range }),
    )
    .await?;
    tracedecay::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
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
