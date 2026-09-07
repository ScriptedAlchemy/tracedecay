//! Hermetic one-connection JSON-RPC pipeline benchmark.
//!
//! Runs only against a temporary git project mounted by the production
//! composition harness. It never opens the operator daemon or profile store.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;
use tracedecay_mcp::transport::ChannelTransport;

const READ_ROUNDS: usize = 16;
const MIXED_ROUNDS: usize = 8;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .unwrap_or_else(|error| panic!("git {args:?} failed to start: {error}"));
    assert!(status.success(), "git {args:?} failed");
}

fn request(id: u64, tool: &str, arguments: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments,
        }
    })
    .to_string()
}

fn percentile_95(mut samples: Vec<u64>) -> u64 {
    samples.sort_unstable();
    let index = samples
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    samples.get(index).copied().unwrap_or_default()
}

async fn run_workload(server: Arc<McpServer>) -> Value {
    let (mut transport, sender, mut responses) = ChannelTransport::new();
    let serving = tokio::spawn(async move { server.run_connection(&mut transport).await });
    let mut sent_at = HashMap::new();
    let started = Instant::now();
    let mut next_id = 1_u64;

    for round in 0..READ_ROUNDS {
        for (tool, arguments) in [
            (
                "tracedecay_search",
                json!({"query": format!("pipeline symbol {round}"), "limit": 5}),
            ),
            (
                "tracedecay_status",
                json!({"admission_only": true, "format": "json"}),
            ),
            (
                "tracedecay_fact_store_search",
                json!({"query": format!("pipeline memory {round}"), "limit": 5}),
            ),
        ] {
            sent_at.insert(next_id, Instant::now());
            sender
                .send(request(next_id, tool, arguments))
                .expect("send independent benchmark read");
            next_id += 1;
        }
    }

    for round in 0..MIXED_ROUNDS {
        for (tool, arguments) in [
            (
                "tracedecay_status",
                json!({"admission_only": true, "format": "json"}),
            ),
            (
                "tracedecay_fact_store_add",
                json!({
                    "content": format!("pipeline effect {round}"),
                    "category": "project",
                    "trust": 0.9,
                }),
            ),
            (
                "tracedecay_fact_store_search",
                json!({"query": format!("pipeline effect {round}"), "limit": 5}),
            ),
        ] {
            sent_at.insert(next_id, Instant::now());
            sender
                .send(request(next_id, tool, arguments))
                .expect("send mixed benchmark request");
            next_id += 1;
        }
    }
    let request_count = next_id - 1;
    drop(sender);

    let mut queue_samples_us = Vec::with_capacity(request_count as usize);
    let mut response_count = 0_u64;
    while let Some(line) = responses.recv().await {
        let response: Value = serde_json::from_str(line.trim()).expect("benchmark response JSON");
        let Some(id) = response.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let sent = sent_at.remove(&id).expect("known benchmark response id");
        let total_us = u64::try_from(sent.elapsed().as_micros()).unwrap_or(u64::MAX);
        let handler_us = response
            .pointer("/result/_meta/duration_us")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        queue_samples_us.push(total_us.saturating_sub(handler_us));
        response_count += 1;
    }
    serving
        .await
        .expect("join benchmark connection")
        .expect("serve benchmark connection");
    assert_eq!(response_count, request_count);
    let elapsed = started.elapsed();
    json!({
        "requests": request_count,
        "elapsed_us": u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
        "throughput_requests_per_second": request_count as f64 / elapsed.as_secs_f64(),
        "p95_dispatch_queue_us": percentile_95(queue_samples_us),
        "read_rounds": READ_ROUNDS,
        "mixed_rounds": MIXED_ROUNDS,
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    tracedecay::product_runtime::register_fixture_product_runtime();
    let sandbox = tempfile::TempDir::new().expect("pipeline benchmark sandbox");
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("benchmark source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn pipeline_symbol() -> usize { 1 }\n",
    )
    .expect("benchmark source");
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["config", "user.email", "pipeline@test.invalid"]);
    git(&project, &["config", "user.name", "Pipeline Benchmark"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "fixture"]);

    let harness = ProductionProjectCompositionHarnessV1::open(sandbox.path(), [project.clone()])
        .await
        .expect("production benchmark composition");
    let server = harness.server(&project).expect("mounted benchmark server");
    server.set_timings_enabled(true);
    let result = run_workload(server).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("serialize benchmark result")
    );
    harness.shutdown().await;
}
