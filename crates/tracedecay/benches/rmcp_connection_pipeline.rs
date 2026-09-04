//! Measures the production typed-RMCP broker connection path.
//!
//! The durable workload is implemented inside the composition crate so it can
//! drive the private daemon broker routing authority without exposing a
//! shipped benchmark API. It reports persistent `tools/call` p50/p95 and full
//! reconnect churn p50/p95. Build with `hotpath-alloc` to also emit exact
//! allocation bytes per measured RMCP dispatch future:
//!
//! ```sh
//! source scripts/hotpath-rustflags.sh
//! HOTPATH_METRICS_SERVER_OFF=1 \
//! cargo bench -p tracedecay --bench rmcp_connection_pipeline \
//!   --no-default-features --features production,rmcp-benchmark,hotpath-alloc
//! ```

#[cfg(feature = "hotpath")]
use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay::daemon::rmcp_benchmark::{
    PERSISTENT_MEASURED_REQUESTS, RECONNECT_MEASURED_ROUNDS, run_rmcp_connection_pipeline,
};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

#[cfg(feature = "hotpath")]
const RMCP_DISPATCH_LABEL: &str = "mcp.server.rmcp.dispatch";

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    #[cfg(feature = "hotpath")]
    let mut hotpath = None;

    let measurement = {
        let mut before_measurement = || {
            // Opening the full production composition is intentionally outside
            // the measurement window. Start Hotpath immediately before the
            // first broker connection so it observes only RMCP initialization,
            // dispatch, and teardown.
            #[cfg(feature = "hotpath")]
            {
                hotpath = Some(hotpath_guard());
            }
        };
        run_rmcp_connection_pipeline(
            PERSISTENT_MEASURED_REQUESTS,
            RECONNECT_MEASURED_ROUNDS,
            &mut before_measurement,
        )
        .await
    }
    .expect("run typed RMCP production connection benchmark");

    #[cfg(feature = "hotpath")]
    let (report_path, hotpath_guard) = hotpath.expect("start RMCP Hotpath measurement");
    #[cfg(feature = "hotpath")]
    drop(hotpath_guard);

    let mut report = json!({"measurement": measurement});
    #[cfg(feature = "hotpath")]
    attach_hotpath_allocations(&mut report, &report_path);
    #[cfg(not(feature = "hotpath"))]
    {
        report["allocation"] = Value::String(
            "build with --features hotpath-alloc for allocated bytes per RMCP dispatch".to_owned(),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize RMCP connection benchmark")
    );
}

#[cfg(feature = "hotpath")]
fn hotpath_guard() -> (PathBuf, hotpath::HotpathGuard) {
    let report_path = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("tracedecay-rmcp-connection-hotpath.json"));
    (
        report_path.clone(),
        hotpath::HotpathGuardBuilder::new("rmcp-connection-pipeline")
            .format(hotpath::Format::Json)
            .output_path(&report_path)
            .build(),
    )
}

#[cfg(feature = "hotpath")]
fn attach_hotpath_allocations(report: &mut Value, report_path: &PathBuf) {
    let hotpath = match std::fs::read_to_string(report_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        Some(hotpath) => hotpath,
        None => {
            report["hotpath_report_path"] = json!(report_path);
            report["allocation"] = json!({"state": "unavailable"});
            return;
        }
    };
    let entry = hotpath
        .pointer("/futures/data")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("label").and_then(Value::as_str) == Some(RMCP_DISPATCH_LABEL)
            })
        })
        .cloned();
    let allocation = entry.map_or_else(
        || json!({"state": "unavailable", "label": RMCP_DISPATCH_LABEL}),
        |entry| {
            let calls = entry["call_count"].as_u64().unwrap_or_default();
            let allocated_bytes = entry["total_poll_alloc_bytes"].as_u64();
            json!({
                "state": allocated_bytes.is_some().then_some("measured").unwrap_or("unavailable"),
                "label": RMCP_DISPATCH_LABEL,
                "dispatches": calls,
                "allocated_bytes": allocated_bytes,
                "allocated_bytes_per_dispatch": allocated_bytes
                    .filter(|_| calls > 0)
                    .map(|bytes| bytes as f64 / calls as f64),
            })
        },
    );
    report["hotpath_report_path"] = json!(report_path);
    report["allocation"] = allocation;
}
