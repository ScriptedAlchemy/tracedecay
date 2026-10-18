//! Measures the production typed-RMCP broker connection path.
//!
//! The durable workload is implemented inside the composition crate so it can
//! drive the private daemon broker routing authority without exposing a
//! shipped benchmark API. It reports persistent `tools/call` p50/p95 and full
//! reconnect churn p50/p95. The process-global dispatch catalog is warmed
//! before sampling, so the output explicitly reports steady-state typed RMCP
//! cost; constructor/cold-start cost is a separate lifecycle measurement.
//!
//! ```sh
//! cargo bench -p tracedecay --bench rmcp_connection_pipeline \
//!   --no-default-features --features production,rmcp-benchmark
//! ```

#![allow(clippy::too_many_lines)]

use serde_json::json;
use tracedecay::daemon::rmcp_benchmark::{
    PERSISTENT_MEASURED_REQUESTS, RECONNECT_MEASURED_ROUNDS, run_rmcp_connection_pipeline,
};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let measurement =
        run_rmcp_connection_pipeline(PERSISTENT_MEASURED_REQUESTS, RECONNECT_MEASURED_ROUNDS)
            .await
            .expect("run typed RMCP production connection benchmark");

    let report = json!({"measurement": measurement});
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize RMCP connection benchmark")
    );
}
