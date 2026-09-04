//! Typed RMCP connection benchmark support over the daemon's real broker route.
//!
//! The benchmark deliberately drives `serve_routed_rmcp_connection` rather
//! than constructing a transport lookalike. Each request traverses broker
//! framing, the RMCP server adapter, the shared dispatch envelope, and the
//! production-composition `McpServer`. The initial replay models the daemon
//! handshake, which consumes the first initialize frame before handing the
//! routed connection to RMCP.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use rmcp::model::CallToolRequestParams;
use rmcp::transport::IntoTransport;
use rmcp::{RoleClient, ServiceExt};
use serde::Serialize;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tracedecay_daemon_protocol::{
    BrokerListener, BrokerStream, DaemonEndpoint, default_loopback_endpoint,
};
use tracedecay_domain::ProjectId;

use super::{BrokerStreamTransport, DaemonLifecycle, serve_routed_rmcp_connection};
use crate::host_admission::HostAdmissionTestRuntimeV1;
use crate::mcp::McpServer;
use crate::tracedecay::TraceDecayOpenOptions;

pub const PERSISTENT_WARMUP_REQUESTS: usize = 8;
pub const PERSISTENT_MEASURED_REQUESTS: usize = 64;
pub const RECONNECT_WARMUP_ROUNDS: usize = 3;
pub const RECONNECT_MEASURED_ROUNDS: usize = 20;

/// One p50/p95 distribution, expressed in nanoseconds.
#[derive(Debug, Serialize)]
pub struct LatencyDistribution {
    pub samples: usize,
    pub p50_ns: u64,
    pub p95_ns: u64,
}

/// Stable result for the real typed RMCP connection benchmark.
#[derive(Debug, Serialize)]
pub struct RmcpConnectionPipelineMeasurement {
    pub schema_version: u32,
    pub workload: &'static str,
    pub transport: &'static str,
    pub persistent_warmup_requests: usize,
    pub persistent_measured_requests: usize,
    pub reconnect_warmup_rounds: usize,
    pub reconnect_measured_rounds: usize,
    pub persistent_status_round_trip: LatencyDistribution,
    pub reconnect_initialize_status_close: LatencyDistribution,
}

struct BenchmarkConnection {
    client: rmcp::service::RunningService<RoleClient, ()>,
    lifecycle: DaemonLifecycle,
    serving: tokio::task::JoinHandle<tracedecay_domain::errors::Result<()>>,
}

impl BenchmarkConnection {
    async fn connect(
        listener: &BrokerListener,
        endpoint: &DaemonEndpoint,
        server: Arc<McpServer>,
    ) -> Result<Self, String> {
        let client = BrokerStream::connect(endpoint)
            .await
            .map_err(|error| format!("connect benchmark RMCP client: {error}"))?;
        let accepted = listener
            .accept()
            .await
            .map_err(|error| format!("accept benchmark RMCP client: {error}"))?;
        let lifecycle = DaemonLifecycle::default();
        let serving_lifecycle = lifecycle.clone();
        let serving = tokio::spawn(async move {
            serve_routed_rmcp_connection(
                server,
                BrokerStreamTransport::new(accepted),
                initialize_replay(),
                VecDeque::new(),
                None,
                false,
                &serving_lifecycle,
            )
            .await
        });

        // The daemon handshake has already read the first initialize frame.
        // Consume its replayed response before starting the real typed client,
        // whose own initialize then goes through the live RMCP server loop.
        let mut bootstrap_reader = tokio::io::BufReader::new(client);
        let mut bootstrap_response = String::new();
        bootstrap_reader
            .read_line(&mut bootstrap_response)
            .await
            .map_err(|error| format!("read benchmark RMCP initialize replay: {error}"))?;
        let bootstrap_response: serde_json::Value = serde_json::from_str(&bootstrap_response)
            .map_err(|error| format!("decode benchmark RMCP initialize replay: {error}"))?;
        if bootstrap_response["result"]["serverInfo"]["name"] != json!("tracedecay") {
            return Err(
                "benchmark RMCP initialize replay did not reach the routed server".to_owned(),
            );
        }
        let client = ()
            .serve(IntoTransport::<RoleClient, _, _>::into_transport(
                bootstrap_reader.into_inner(),
            ))
            .await
            .map_err(|error| format!("initialize typed benchmark RMCP client: {error}"))?;

        Ok(Self {
            client,
            lifecycle,
            serving,
        })
    }

    async fn status(&self) -> Result<(), String> {
        self.client
            .call_tool(
                CallToolRequestParams::new("tracedecay_status").with_arguments(
                    json!({"admission_only": true, "format": "json"})
                        .as_object()
                        .cloned()
                        .ok_or_else(|| "benchmark status arguments must be an object".to_owned())?,
                ),
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("call typed benchmark RMCP status: {error}"))
    }

    async fn shutdown(mut self) -> Result<(), String> {
        self.client
            .close()
            .await
            .map_err(|error| format!("close typed benchmark RMCP client: {error}"))?;
        self.lifecycle.begin_draining();
        self.serving
            .await
            .map_err(|error| format!("join benchmark RMCP route: {error}"))?
            .map_err(|error| format!("finish benchmark RMCP route: {error}"))
    }
}

/// Measures persistent-call latency separately from full reconnect churn.
///
/// The production composition and listener are created before sampling so the
/// persistent distribution means exactly one typed `tools/call` round trip.
/// A reconnect sample starts before socket connection and includes typed
/// initialize, `tracedecay_status`, client close, and daemon-route teardown.
pub async fn run_rmcp_connection_pipeline<F>(
    persistent_requests: usize,
    reconnect_rounds: usize,
    before_measurement: F,
) -> Result<RmcpConnectionPipelineMeasurement, String>
where
    F: FnOnce(),
{
    if persistent_requests == 0 || reconnect_rounds == 0 {
        return Err("RMCP benchmark requires non-zero persistent and reconnect samples".to_owned());
    }

    crate::product_runtime::register_fixture_product_runtime();
    let sandbox =
        tempfile::TempDir::new().map_err(|error| format!("create benchmark sandbox: {error}"))?;
    let project = sandbox.path().join("project");
    let profile = sandbox.path().join("profile");
    initialize_project(&project)?;
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        &profile,
        &project,
        ProjectId::new("rmcp-connection-benchmark")
            .map_err(|error| format!("create benchmark project identity: {error}"))?,
    )
    .await
    .map_err(|error| format!("open registered benchmark runtime: {error}"))?;
    let graph = runtime
        .initialize_project_graph_for_test(
            &project,
            TraceDecayOpenOptions {
                profile_root: Some(profile),
                global_db_path: None,
            },
        )
        .await
        .map_err(|error| format!("initialize registered benchmark graph: {error}"))?;
    let server = McpServer::new_with_host_admission_test_runtime_for_test(graph, None, runtime)
        .await
        .map_err(|error| format!("resolve production benchmark server: {error}"))?;
    let (listener, endpoint) = BrokerListener::bind(&default_loopback_endpoint())
        .await
        .map_err(|error| format!("bind benchmark RMCP broker listener: {error}"))?;

    before_measurement();

    let persistent =
        BenchmarkConnection::connect(&listener, &endpoint, Arc::clone(&server)).await?;
    for _ in 0..PERSISTENT_WARMUP_REQUESTS {
        persistent.status().await?;
    }
    let mut persistent_samples = Vec::with_capacity(persistent_requests);
    for _ in 0..persistent_requests {
        let started = Instant::now();
        persistent.status().await?;
        persistent_samples.push(duration_ns(started.elapsed()));
    }
    persistent.shutdown().await?;

    for _ in 0..RECONNECT_WARMUP_ROUNDS {
        let connection =
            BenchmarkConnection::connect(&listener, &endpoint, Arc::clone(&server)).await?;
        connection.status().await?;
        connection.shutdown().await?;
    }
    let mut reconnect_samples = Vec::with_capacity(reconnect_rounds);
    for _ in 0..reconnect_rounds {
        let started = Instant::now();
        let connection =
            BenchmarkConnection::connect(&listener, &endpoint, Arc::clone(&server)).await?;
        connection.status().await?;
        connection.shutdown().await?;
        reconnect_samples.push(duration_ns(started.elapsed()));
    }

    server.shutdown().await;
    Ok(RmcpConnectionPipelineMeasurement {
        schema_version: 1,
        workload: "rmcp-production-broker-connection-pipeline",
        transport: "broker-framing+typed-rmcp-server+shared-dispatch-envelope",
        persistent_warmup_requests: PERSISTENT_WARMUP_REQUESTS,
        persistent_measured_requests: persistent_requests,
        reconnect_warmup_rounds: RECONNECT_WARMUP_ROUNDS,
        reconnect_measured_rounds: reconnect_rounds,
        persistent_status_round_trip: distribution(persistent_samples),
        reconnect_initialize_status_close: distribution(reconnect_samples),
    })
}

fn initialize_project(project: &Path) -> Result<(), String> {
    std::fs::create_dir_all(project.join("src"))
        .map_err(|error| format!("create benchmark source directory: {error}"))?;
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn rmcp_connection_benchmark_symbol() -> usize { 1 }\n",
    )
    .map_err(|error| format!("write benchmark source: {error}"))?;
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["config", "user.email", "rmcp-benchmark@test.invalid"].as_slice(),
        ["config", "user.name", "RMCP Connection Benchmark"].as_slice(),
        ["add", "."].as_slice(),
        ["commit", "-q", "-m", "fixture"].as_slice(),
    ] {
        let status = Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .map_err(|error| format!("resolve git executable: {error}"))?,
        )
        .args(args)
        .current_dir(project)
        .status()
        .map_err(|error| format!("run git {args:?}: {error}"))?;
        if !status.success() {
            return Err(format!("git {args:?} failed with {status}"));
        }
    }
    Ok(())
}

fn initialize_replay() -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "tracedecay-rmcp-connection-benchmark-bootstrap",
                "version": "1"
            }
        }
    })
    .to_string()
}

fn duration_ns(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn distribution(mut samples: Vec<u64>) -> LatencyDistribution {
    samples.sort_unstable();
    let percentile = |numerator: usize| {
        samples
            .get(
                samples
                    .len()
                    .saturating_mul(numerator)
                    .div_ceil(100)
                    .saturating_sub(1),
            )
            .copied()
            .unwrap_or_default()
    };
    LatencyDistribution {
        samples: samples.len(),
        p50_ns: percentile(50),
        p95_ns: percentile(95),
    }
}
