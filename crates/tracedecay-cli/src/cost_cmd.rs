use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_usecases::provider_usage::{ProviderUsageCostSummaryV1, ProviderUsageCoverageV1};

pub(crate) async fn handle_cost(
    range: String,
    by_model: bool,
    by_task: bool,
    export: Option<String>,
) -> tracedecay::errors::Result<()> {
    let payload = call_cost_admin(&range).await?;
    if payload.get("summary").is_none_or(Value::is_null) {
        println!("Provider usage accounting is unavailable.");
        return Ok(());
    }
    let summary: CostSummaryPayload = serde_json::from_value(payload["summary"].clone())?;
    let today: TodayCostPayload = serde_json::from_value(payload["today"].clone())?;
    if summary.provider_usage.coverage == ProviderUsageCoverageV1::Unavailable {
        println!("No canonical provider usage is available for this profile.");
        return Ok(());
    }

    print_cost_summary(
        &today.provider_usage,
        &range,
        by_model,
        by_task,
        export.as_deref(),
        &summary,
    )?;
    Ok(())
}

fn print_cost_summary(
    today: &ProviderUsageCostSummaryV1,
    range: &str,
    by_model: bool,
    by_task: bool,
    export: Option<&str>,
    summary: &CostSummaryPayload,
) -> tracedecay::errors::Result<()> {
    if let Some(fmt) = export {
        print_cost_export(fmt, range, by_model, by_task, summary)?;
    } else if by_model {
        print_model_table(summary);
    } else if by_task {
        print_task_table(summary);
    } else {
        print_default_summary(today, range, summary);
    }
    Ok(())
}

fn print_cost_export(
    fmt: &str,
    range: &str,
    by_model: bool,
    by_task: bool,
    summary: &CostSummaryPayload,
) -> tracedecay::errors::Result<()> {
    let usage = &summary.provider_usage;
    match fmt {
        "json" => {
            let obj = serde_json::json!({
                "range": range,
                "coverage": usage.coverage,
                "pricing_revision": usage.pricing_revision,
                "total_cost_usd": usage.total_cost_usd,
                "total_input_tokens": usage.total_input_tokens,
                "total_output_tokens": usage.total_output_tokens,
                "tokens_saved": summary.tokens_saved,
                "efficiency_ratio": summary.efficiency_ratio,
                "by_model": usage.by_model,
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
        }
        "csv" => print_cost_csv(summary, by_model, by_task),
        _ => eprintln!("Unknown export format '{fmt}'. Use 'json' or 'csv'."),
    }
    Ok(())
}

fn print_cost_csv(summary: &CostSummaryPayload, by_model: bool, by_task: bool) {
    let usage = &summary.provider_usage;
    if by_model {
        println!("provider,model,cost_usd,tokens");
        for model in &usage.by_model {
            let cost = model
                .cost_usd
                .map(|cost| format!("{cost:.4}"))
                .unwrap_or_else(|| "unavailable".to_owned());
            let tokens = model
                .total_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "unavailable".to_owned());
            println!("{},{},{cost},{tokens}", model.provider, model.model);
        }
    } else if by_task {
        println!("status");
        println!("task_attribution_unavailable");
    } else {
        println!("total_cost_usd,input_tokens,output_tokens,tokens_saved,efficiency");
        let total_cost = usage
            .total_cost_usd
            .map(|cost| format!("{cost:.4}"))
            .unwrap_or_else(|| "unavailable".to_owned());
        let input = usage
            .total_input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unavailable".to_owned());
        let output = usage
            .total_output_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unavailable".to_owned());
        let efficiency = summary
            .efficiency_ratio
            .map(|ratio| format!("{ratio:.4}"))
            .unwrap_or_else(|| "unavailable".to_owned());
        println!(
            "{total_cost},{input},{output},{},{efficiency}",
            summary.tokens_saved
        );
    }
}

fn print_model_table(summary: &CostSummaryPayload) {
    let usage = &summary.provider_usage;
    println!(
        "  {:<12} {:<24} {:>10} {:>10} {:>6}",
        "Provider", "Model", "Cost", "Tokens", "Share"
    );
    for model in &usage.by_model {
        let share = usage
            .total_cost_usd
            .zip(model.cost_usd)
            .filter(|(total, _)| *total > 0.0)
            .map(|(total, cost)| format!("{:.0}%", cost / total * 100.0))
            .unwrap_or_else(|| "n/a".to_owned());
        let token_count = model
            .total_tokens
            .map(tracedecay::display::format_token_count)
            .unwrap_or_else(|| "unknown".to_owned());
        let cost = model
            .cost_usd
            .map(|cost| format!("${cost:.2}"))
            .unwrap_or_else(|| "unavailable".to_owned());
        println!(
            "  {:<12} {:<24} {:>10} {:>10} {:>6}",
            model.provider, model.model, cost, token_count, share
        );
    }
}

fn print_task_table(_summary: &CostSummaryPayload) {
    println!("Task cost attribution is unavailable from canonical provider usage.");
}

fn print_default_summary(
    today: &ProviderUsageCostSummaryV1,
    range: &str,
    summary: &CostSummaryPayload,
) {
    let usage = &summary.provider_usage;
    println!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10}",
        "Period", "Cost", "Input", "Output", "Cache-hit"
    );
    print_cost_row(
        "Today",
        today.total_cost_usd,
        today.total_input_tokens,
        today.total_output_tokens,
        today.total_cache_read_tokens,
    );
    print_cost_row(
        range,
        usage.total_cost_usd,
        usage.total_input_tokens,
        usage.total_output_tokens,
        usage.total_cache_read_tokens,
    );

    if summary.tokens_saved > 0 {
        let saved = tracedecay::display::format_token_count(summary.tokens_saved);
        println!();
        match summary.efficiency_ratio {
            Some(ratio) => {
                println!(
                    "  Savings  {saved} tokens ({:.0}% efficiency)",
                    ratio * 100.0
                );
            }
            None => println!("  Savings  {saved} tokens (efficiency unavailable)"),
        }
    }
}

#[derive(Deserialize)]
struct CostSummaryPayload {
    provider_usage: ProviderUsageCostSummaryV1,
    tokens_saved: u64,
    efficiency_ratio: Option<f64>,
}

#[derive(Deserialize)]
struct TodayCostPayload {
    provider_usage: ProviderUsageCostSummaryV1,
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

fn print_cost_row(
    label: &str,
    cost: Option<f64>,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
) {
    let cache_pct = input.zip(cache_read).and_then(|(input, cache_read)| {
        let denominator = input.checked_add(cache_read)?;
        (denominator > 0).then_some((cache_read as f64 / denominator as f64) * 100.0)
    });
    let input = input
        .map(tracedecay::display::format_token_count)
        .unwrap_or_else(|| "unknown".to_owned());
    let output = output
        .map(tracedecay::display::format_token_count)
        .unwrap_or_else(|| "unknown".to_owned());
    let cost = cost
        .map(|cost| format!("${cost:.2}"))
        .unwrap_or_else(|| "unavailable".to_owned());
    let cache = cache_pct
        .map(|percent| format!("{percent:.0}%"))
        .unwrap_or_else(|| "unknown".to_owned());
    println!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10}",
        label, cost, input, output, cache
    );
}
