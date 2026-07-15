use super::daemon::daemon_tool_json;

/// Convert raw tokens-saved into a USD estimate using Sonnet input pricing.
/// Sonnet is the default agent target; output-token savings are not relevant
/// for retrieval savings.
///
/// Pure table lookup: callers that want up-to-date prices must run
/// `pricing::refresh_if_stale()` once beforehand (see [`handle_gain`]).
/// Keeping the refresh out of this function avoids a network fetch per call
/// (it used to fire for every history row and for every unit test process).
pub(crate) fn estimate_dollars_saved(saved_tokens: u64) -> f64 {
    use tracedecay::accounting::pricing;
    let price = pricing::lookup("claude-sonnet-4").map_or(3.0, |p| p.input_per_mtok);
    (saved_tokens as f64) * price / 1_000_000.0
}

pub async fn handle_gain(
    all: bool,
    history: bool,
    range: &str,
    json_output: bool,
) -> tracedecay::errors::Result<()> {
    tracedecay::accounting::pricing::refresh_if_stale();
    let since = tracedecay::accounting::metrics::parse_range(range);
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
            "since": since as i64,
            "history": history,
        }),
    )
    .await?;
    if history {
        let rows = result
            .get("history")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(|row| tracedecay::global_db::SavingsDay {
                day: row
                    .get("day")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0),
                saved_tokens: row
                    .get("saved_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                calls: row
                    .get("calls")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
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
            println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
        } else {
            tracedecay::display::print_gain_history(&rows, estimate_dollars_saved);
        }
        return Ok(());
    }

    let saved_tokens = result
        .get("saved_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let calls = result
        .get("calls")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let usd = estimate_dollars_saved(saved_tokens);

    if json_output {
        let out = serde_json::json!({
            "range": range,
            "project": project_filter.clone().unwrap_or_else(|| "ALL".to_string()),
            "saved_tokens": saved_tokens,
            "calls": calls,
            "usd": usd,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
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
        assert!((usd - 3.0).abs() < 0.01, "expected ~$3.00, got ${usd}");
    }

    #[test]
    fn dollars_handles_small_counts() {
        // 1_000 tokens × $3 / MTok = $0.003
        let usd = estimate_dollars_saved(1_000);
        assert!((usd - 0.003).abs() < 0.001);
    }

    #[test]
    fn dollars_zero_for_zero_tokens() {
        assert_eq!(estimate_dollars_saved(0), 0.0);
    }
}
