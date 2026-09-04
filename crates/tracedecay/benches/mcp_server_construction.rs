//! Hermetic process fixture for production MCP server construction.
//!
//! This intentionally stops before listener binding or RMCP traffic. Wrap the
//! built executable with `scripts/profile-hotpath-os-counters.sh` to measure
//! no-RMCP project-open wall time, retained RSS, and high-water memory without
//! shifting lazy global dispatch-catalog construction onto this lifecycle.

use std::time::Duration;

use tracedecay::daemon::rmcp_benchmark::run_mcp_server_construction_fixture;

const RETAIN_FOR: Duration = Duration::from_secs(2);

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    run_mcp_server_construction_fixture(RETAIN_FOR)
        .await
        .expect("construct and shut down production MCP server fixture");
}
