use super::daemon::daemon_tool_json;
use serde::Deserialize;

#[derive(Deserialize)]
struct SavingsDayPayload {
    day: i64,
    saved_tokens: u64,
    calls: u64,
}

#[derive(Deserialize)]
struct SavingsTotalPayload {
    saved_tokens: u64,
    calls: u64,
}

/// Convert raw tokens-saved into a USD estimate using Sonnet input pricing.
/// Sonnet is the default agent target; output-token savings are not relevant
/// for retrieval savings.
///
/// Pure lookup against the deterministic bundled pricing authority.
pub(crate) fn estimate_dollars_saved(saved_tokens: u64) -> Option<f64> {
    let table = tracedecay::application::provider_pricing::load_table();
    let price = tracedecay::application::provider_pricing::resolve_model_price(
        table,
        "claude",
        "claude-sonnet-4-6",
    )?;
    Some((saved_tokens as f64) * price.prompt_per_mtok / 1_000_000.0)
}

pub async fn handle_gain(
    all: bool,
    history: bool,
    range: &str,
    json_output: bool,
) -> tracedecay::errors::Result<()> {
    let since = tracedecay::application::provider_usage::provider_usage_range_start(range)
        .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
    let since = i64::try_from(since).map_err(|_| tracedecay::errors::TraceDecayError::Config {
        message: "savings range exceeds the supported timestamp domain".to_owned(),
    })?;
    let project_filter: Option<String> = if all {
        None
    } else {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    };

    let result = daemon_tool_json(
        None,
        "tracedecay_admin_cli",
        serde_json::json!({
            "action": "gain_query",
            "project_arg": project_filter,
            "since": since,
            "history": history,
        }),
    )
    .await?;
    if history {
        let rows = result
            .get("history")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                message: "daemon gain history response is missing history rows".to_owned(),
            })?;
        let rows = rows
            .iter()
            .cloned()
            .map(serde_json::from_value::<SavingsDayPayload>)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|row| tracedecay::global_db::SavingsDay {
                day: row.day,
                saved_tokens: row.saved_tokens,
                calls: row.calls,
            })
            .collect::<Vec<_>>();
        if json_output {
            let arr: Vec<_> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "day": r.day,
                        "saved_tokens": r.saved_tokens,
                        "calls": r.calls,
                        "usd": estimate_dollars_saved(r.saved_tokens),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr)?);
        } else {
            tracedecay::display::print_gain_history(&rows, estimate_dollars_saved);
        }
        return Ok(());
    }

    let total: SavingsTotalPayload = serde_json::from_value(result)?;
    let saved_tokens = total.saved_tokens;
    let calls = total.calls;
    let usd = estimate_dollars_saved(saved_tokens);

    if json_output {
        let out = serde_json::json!({
            "range": range,
            "project": project_filter.clone().unwrap_or_else(|| "ALL".to_string()),
            "saved_tokens": saved_tokens,
            "calls": calls,
            "usd": usd,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        tracedecay::display::print_gain_total(
            project_filter.as_deref().unwrap_or("ALL projects"),
            range,
            saved_tokens,
            calls,
            usd,
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::estimate_dollars_saved;

    #[test]
    fn dollars_uses_sonnet_input_price_by_default() {
        // 1_000_000 tokens × $3 / MTok = $3.00 (Sonnet input price)
        let usd = estimate_dollars_saved(1_000_000);
        assert!((usd.unwrap() - 3.0).abs() < 0.01);
    }

    #[test]
    fn dollars_handles_small_counts() {
        // 1_000 tokens × $3 / MTok = $0.003
        let usd = estimate_dollars_saved(1_000);
        assert!((usd.unwrap() - 0.003).abs() < 0.001);
    }

    #[test]
    fn dollars_zero_for_zero_tokens() {
        assert_eq!(estimate_dollars_saved(0), Some(0.0));
    }
}
