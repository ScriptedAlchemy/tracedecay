//! `tracedecay_session_start` and `tracedecay_session_end` — health snapshots bracketing an agent session.

use super::*;

fn session_dimension_deltas(
    dims_before: &Value,
    snap: &HealthSnapshot,
) -> (serde_json::Map<String, Value>, Vec<String>) {
    let mut dimensions = serde_json::Map::new();
    let mut degraded_dimensions: Vec<String> = vec![];

    for (name, after_val) in session_dimension_values(snap) {
        let before_val = dims_before[name].as_f64().unwrap_or(0.0);
        let dim_delta = after_val - before_val;
        let status = if dim_delta > 0.001 {
            "improved"
        } else if dim_delta < -0.001 {
            degraded_dimensions.push(name.to_string());
            "degraded"
        } else {
            "unchanged"
        };
        dimensions.insert(
            name.to_string(),
            json!({
                "before": (before_val * 10000.0).round() / 10000.0,
                "after": (after_val * 10000.0).round() / 10000.0,
                "delta": (dim_delta * 10000.0).round() / 10000.0,
                "status": status,
            }),
        );
    }

    (dimensions, degraded_dimensions)
}

fn session_tool_result(cg: &TraceDecay, args: &Value, output: &Value) -> ToolResult {
    generic_tool_result(Some(cg.project_root()), args, output, vec![])
}

/// Handles `tracedecay_session_start` tool calls.
pub(crate) async fn handle_session_start(
    cg: &TraceDecay,
    db: &RegisteredGlobalDb,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let delta = compute_health_delta_result(cg, db, None, path_prefix).await?;

    let tracedecay_dir = &cg.store_layout().data_root;
    std::fs::create_dir_all(tracedecay_dir).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to create active store data root: {e}"),
        }
    })?;
    let baseline_path = tracedecay_dir.join("session_baseline.json");
    std::fs::write(
        &baseline_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "health_delta_cursor": delta.after_cursor,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| crate::errors::TraceDecayError::Config {
        message: format!("failed to write session baseline: {e}"),
    })?;

    let output = json!({
        "status": "baseline_saved",
        "quality_signal": delta.after.quality_signal,
        "files_analyzed": delta.after.files_analyzed,
        "health_delta_cursor": delta.after_cursor,
        "deprecated": true,
        "replacement": "tracedecay_health_delta",
    });
    Ok(session_tool_result(cg, &args, &output))
}

/// Handles `tracedecay_session_end` tool calls.
pub(crate) async fn handle_session_end(
    cg: &TraceDecay,
    db: &RegisteredGlobalDb,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let tracedecay_dir = &cg.store_layout().data_root;
    let baseline_path = tracedecay_dir.join("session_baseline.json");

    if !baseline_path.exists() {
        let output = json!({
            "status": "no_baseline",
            "message": "No session baseline found. Call tracedecay_session_start first.",
        });
        return Ok(session_tool_result(cg, &args, &output));
    }

    let baseline_raw = std::fs::read_to_string(&baseline_path).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to read session baseline: {e}"),
        }
    })?;
    let baseline: Value = serde_json::from_str(&baseline_raw).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to parse session baseline: {e}"),
        }
    })?;
    if let Some(cursor) = baseline.get("health_delta_cursor").and_then(Value::as_str) {
        let path_prefix = effective_path(&args, scope_prefix);
        let result = compute_health_delta_result(cg, db, Some(cursor), path_prefix).await?;
        let _ = std::fs::remove_file(&baseline_path);
        let degraded_dimensions = result
            .dimensions
            .iter()
            .filter(|(_, value)| value.status == "degraded")
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let dimensions = result
            .dimensions
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    json!({
                        "before": value.before_ppm as f64 / 1_000_000.0,
                        "after": value.after_ppm as f64 / 1_000_000.0,
                        "delta": value.delta_ppm as f64 / 1_000_000.0,
                        "status": value.status,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let output = json!({
            "pass": result.pass,
            "signal_before": result.before.quality_signal,
            "signal_after": result.after.quality_signal,
            "delta": result.delta,
            "files_analyzed": result.after.files_analyzed,
            "degraded_dimensions": degraded_dimensions,
            "dimensions": dimensions,
            "before_watermark": result.before.watermark,
            "after_watermark": result.after.watermark,
            "coverage": result.coverage,
            "currentness": result.currentness,
            "deprecated": true,
            "replacement": "tracedecay_health_delta",
        });
        return Ok(session_tool_result(cg, &args, &output));
    }

    let signal_before = baseline["quality_signal"].as_u64().unwrap_or(0) as u32;
    let dims_before = &baseline["dimensions"];

    // Recompute current health
    let path_prefix = effective_path(&args, scope_prefix);
    let snap = compute_health_snapshot(cg, path_prefix).await?;

    let _ = std::fs::remove_file(&baseline_path);

    let signal_after = snap.quality_signal;
    let delta = i64::from(signal_after) - i64::from(signal_before);
    let pass = signal_after >= signal_before;

    let (dimensions, degraded_dimensions) = session_dimension_deltas(dims_before, &snap);

    let output = json!({
        "pass": pass,
        "signal_before": signal_before,
        "signal_after": signal_after,
        "delta": delta,
        "files_analyzed": snap.files_analyzed,
        "degraded_dimensions": degraded_dimensions,
        "dimensions": dimensions,
    });
    Ok(session_tool_result(cg, &args, &output))
}
