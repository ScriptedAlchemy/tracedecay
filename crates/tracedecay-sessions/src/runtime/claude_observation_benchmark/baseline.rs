use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hooks::{
    CURSOR_CATCH_UP_INGEST_MAX_BYTES, HookCompletedReadinessDistributions,
    aggregate_hook_completed_readiness, host_hook_telemetry_contract,
    measure_host_event_payload_bytes,
};

use super::REDACTION_MARKER;
use super::artifact::sha256_hex;

pub(super) const PROVIDER_BASELINE_SCHEMA_VERSION: u32 = 1;
pub(super) const PROVIDER_BASELINE_CATALOG_ID: &str = "provider-observation-baselines-v1";
pub(super) const PROVIDER_RECORDS_PER_REPETITION: usize = 8;

pub(super) const PROVIDERS: &[&str] = &[
    "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
];
const CHECKS: &[&str] = &[
    "parse",
    "normalize",
    "sanitize",
    "commit",
    "replay",
    "duplicate_noop",
    "projection",
    "backlog",
    "fairness",
    "peak_resource",
];
const PERFORMANCE_METRICS: &[&str] = &[
    "parse_latency_p50_p95_p99",
    "commit_latency_p50_p95_p99",
    "replay_latency_p50_p95_p99",
    "pipeline_latency_p50_p95_p99",
    "pipeline_records_per_second",
    "pipeline_cpu",
    "pipeline_process_write_bytes",
    "database_storage_growth_bytes",
    "peak_rss_kib",
    "no_op_replay_latency_p50_p95_p99",
    "no_op_observation_count_delta",
    "bounded_replay_backlog_records",
];
const PEAK_RESOURCE_FIELDS: &[&str] = &[
    "cpu_ticks",
    "process_write_bytes",
    "database_storage_growth_bytes",
    "peak_rss_kib",
];
const FORBIDDEN_TELEMETRY_CONTENT: &[&str] =
    &["sk-test-", "credentials", "private_path", "reasoning_text"];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderBaseline {
    pub(super) provider: String,
    pub(super) fixture: ProviderFixture,
    pub(super) checks: Vec<String>,
    pub(super) bounds: ProviderBounds,
    /// Additive v1 field. Validation requires it for every provider while old
    /// tolerant readers can continue to consume the original v1 fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) measurement: Option<ProviderMeasurement>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderFixture {
    pub(super) format: String,
    pub(super) source_paths: Vec<String>,
    pub(super) session_id: String,
    pub(super) message_id: String,
    pub(super) redacted_secret: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderBounds {
    pub(super) records_per_repetition: usize,
    pub(super) replay_limit: usize,
    pub(super) max_backlog_records: usize,
    pub(super) fair_rotation_providers: usize,
    pub(super) peak_resource_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderMeasurement {
    pub(super) harness_path: String,
    pub(super) harness_measures_performance: bool,
    pub(super) result_schema: String,
    pub(super) required_metrics: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ProviderBaselineCatalog {
    schema_version: u32,
    catalog_id: &'static str,
    compatibility: &'static str,
    baselines: Vec<ProviderBaseline>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct HookTelemetryReadiness {
    artifact_kind: String,
    canonical_contract: Value,
    host_fixture_measurements: Vec<HookHostFixtureMeasurement>,
    cursor_catch_up_ingest_max_bytes: u64,
    /// Deterministic empty aggregate shape proving readiness distributions are wired.
    /// Live rows are aggregated via `aggregate_hook_completed_readiness`; this catalog
    /// identity stays empty so fixture readiness remains reproducible.
    readiness_distributions: HookCompletedReadinessDistributions,
    unavailable_measurements: Vec<UnavailableHookMeasurement>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct HookHostFixtureMeasurement {
    host: String,
    fixture_path: String,
    fixture_sha256: String,
    canonical_request_payload_bytes: Vec<u64>,
    disposition_vocabulary: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct UnavailableHookMeasurement {
    metric: String,
    status: String,
    required_collection_step: String,
}

pub(super) fn expected() -> Vec<ProviderBaseline> {
    PROVIDERS
        .iter()
        .map(|provider| baseline(provider))
        .collect()
}

pub(super) fn catalog() -> ProviderBaselineCatalog {
    ProviderBaselineCatalog {
        schema_version: PROVIDER_BASELINE_SCHEMA_VERSION,
        catalog_id: PROVIDER_BASELINE_CATALOG_ID,
        compatibility: "v1_additive_optional_measurement_field",
        baselines: expected(),
    }
}

pub(super) fn validate(baselines: &[ProviderBaseline]) {
    assert_eq!(baselines, expected(), "provider baseline contract changed");
    for baseline in baselines {
        assert_eq!(
            baseline.checks,
            strings(CHECKS),
            "{} checks must declare the executable fair multi-source contract",
            baseline.provider
        );
        assert!(
            baseline.checks.iter().any(|check| check == "fairness"),
            "{} missing fairness check",
            baseline.provider
        );
        assert_eq!(
            baseline.bounds.fair_rotation_providers,
            PROVIDERS.len(),
            "{} fair_rotation_providers must cover every provider source",
            baseline.provider
        );
        let fixture = &baseline.fixture;
        assert_eq!(fixture.format, "checked_in_native_bounded_copy_v1");
        assert!(!fixture.source_paths.is_empty());
        assert!(fixture.redacted_secret.starts_with(REDACTION_MARKER));
        assert!(!fixture.redacted_secret.contains("sk-test-"));
        let measurement = baseline
            .measurement
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no measured result contract", baseline.provider));
        assert!(
            measurement.harness_measures_performance,
            "{} cannot be pending",
            baseline.provider
        );
        assert_ne!(
            measurement.harness_path,
            "pending_provider_observation_ingest"
        );
        assert_eq!(
            measurement.result_schema,
            "provider-observation-performance-result-v1"
        );
        assert_eq!(measurement.required_metrics, strings(PERFORMANCE_METRICS));
    }
}

pub(super) fn hook_telemetry_readiness() -> HookTelemetryReadiness {
    hook_telemetry_readiness_from_rows(&[])
}

pub(super) fn hook_telemetry_readiness_from_rows(rows: &[Value]) -> HookTelemetryReadiness {
    let canonical_contract = host_hook_telemetry_contract();
    let host_fixture_measurements = canonical_hook_hosts(&canonical_contract)
        .into_iter()
        .map(fixture_measurement)
        .collect();
    HookTelemetryReadiness {
        artifact_kind: "readiness_and_fixture_identity_not_runtime_contract".to_string(),
        canonical_contract,
        host_fixture_measurements,
        cursor_catch_up_ingest_max_bytes: CURSOR_CATCH_UP_INGEST_MAX_BYTES,
        readiness_distributions: aggregate_hook_completed_readiness(rows),
        unavailable_measurements: vec![unavailable(
            "daemon_processing_duration_distribution",
            "hook_completed emits host-measured daemon_rtt_us (TRUE host IPC RTT) only; daemon-internal processing duration is not present on the event contract",
        )],
    }
}

pub(super) fn validate_hook_telemetry_readiness() {
    let readiness = hook_telemetry_readiness();
    assert_eq!(
        readiness.artifact_kind,
        "readiness_and_fixture_identity_not_runtime_contract"
    );
    let metrics = readiness.canonical_contract["metrics"]
        .as_object()
        .expect("canonical hook telemetry metrics");
    assert_eq!(readiness.canonical_contract["schema_version"], 1);
    assert_eq!(metrics.len(), 5);
    assert_eq!(
        readiness.cursor_catch_up_ingest_max_bytes,
        CURSOR_CATCH_UP_INGEST_MAX_BYTES
    );
    let canonical_hosts = canonical_hook_hosts(&readiness.canonical_contract);
    assert_eq!(
        readiness
            .host_fixture_measurements
            .iter()
            .map(|host| host.host.as_str())
            .collect::<Vec<_>>(),
        canonical_hosts
    );
    for host in &readiness.host_fixture_measurements {
        assert_eq!(host.fixture_sha256.len(), 64);
        assert_eq!(host.canonical_request_payload_bytes.len(), 4);
        assert!(
            host.canonical_request_payload_bytes
                .iter()
                .all(|bytes| *bytes > 0 && *bytes < CURSOR_CATCH_UP_INGEST_MAX_BYTES)
        );
        let encoded = serde_json::to_string(host).expect("serialize fixture measurement");
        for forbidden in FORBIDDEN_TELEMETRY_CONTENT {
            assert!(!encoded.contains(forbidden));
        }
    }
    assert!(
        readiness
            .unavailable_measurements
            .iter()
            .any(|measurement| measurement.metric == "daemon_processing_duration_distribution")
    );
    assert_eq!(readiness.unavailable_measurements.len(), 1);
    assert_eq!(
        readiness.readiness_distributions.source_event,
        "hook_completed"
    );
    assert_eq!(
        serde_json::to_value(&readiness.readiness_distributions)
            .expect("serialize empty readiness distributions")["collection_status"],
        "no_samples"
    );
    assert_eq!(readiness.readiness_distributions.input_rows_received, 0);
    assert_eq!(readiness.readiness_distributions.input_rows_processed, 0);
    assert_eq!(
        readiness.readiness_distributions.input_rows_dropped_at_cap,
        0
    );
    assert_eq!(readiness.readiness_distributions.events_considered, 0);
    assert_eq!(
        readiness.canonical_contract["latency_semantics"]["host_ipc_rtt"]["event_field"],
        "daemon_rtt_us"
    );
    assert_eq!(
        readiness.canonical_contract["latency_semantics"]["daemon_processing_duration"]["status"],
        "unavailable"
    );
}

fn canonical_hook_hosts(contract: &Value) -> Vec<&str> {
    contract["provider_coverage"]
        .as_array()
        .expect("canonical hook provider coverage")
        .iter()
        .map(|entry| {
            entry["host"]
                .as_str()
                .expect("canonical hook provider host")
        })
        .collect()
}

fn baseline(provider: &str) -> ProviderBaseline {
    ProviderBaseline {
        provider: provider.to_string(),
        fixture: ProviderFixture {
            format: "checked_in_native_bounded_copy_v1".to_string(),
            source_paths: provider_fixture_paths(provider),
            session_id: format!("benchmark-{provider}-session"),
            message_id: format!("benchmark-{provider}-message-0"),
            redacted_secret: format!("{REDACTION_MARKER} fixture]"),
        },
        checks: strings(CHECKS),
        bounds: ProviderBounds {
            records_per_repetition: PROVIDER_RECORDS_PER_REPETITION,
            replay_limit: PROVIDER_RECORDS_PER_REPETITION + 1,
            max_backlog_records: PROVIDER_RECORDS_PER_REPETITION,
            fair_rotation_providers: PROVIDERS.len(),
            peak_resource_fields: strings(PEAK_RESOURCE_FIELDS),
        },
        measurement: Some(ProviderMeasurement {
            harness_path: format!("{provider}_production_observation_pipeline_v1"),
            harness_measures_performance: true,
            result_schema: "provider-observation-performance-result-v1".to_string(),
            required_metrics: strings(PERFORMANCE_METRICS),
        }),
    }
}

fn provider_fixture_paths(provider: &str) -> Vec<String> {
    let paths: &[&str] = match provider {
        "claude" => &["tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json"],
        "codex" => &[
            "tests/fixtures/provider_normalization/codex/session_meta.input.json",
            "tests/fixtures/provider_normalization/codex/agent_message.input.json",
        ],
        "cursor" => &["tests/fixtures/provider_normalization/cursor/tool_use.input.json"],
        "hermes" => {
            &["tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json"]
        }
        "kiro" => &["tests/fixtures/provider_normalization/kiro/workspace_session.input.json"],
        "cline" | "roo-code" | "kilo" => &[
            "tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json",
            "tests/fixtures/transcript_golden/cline_like/input/task_metadata.json",
        ],
        _ => panic!("unsupported provider fixture {provider}"),
    };
    strings(paths)
}

fn fixture_measurement(host: &str) -> HookHostFixtureMeasurement {
    let relative = format!("tests/fixtures/host_events/{host}/baseline.json");
    let path = repository_path(&relative);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("read direct host fixture {}: {error}", path.display()));
    let document: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse direct host fixture {}: {error}", path.display()));
    assert_eq!(document["provider"], host);
    let cases = document["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("fixture {relative} has no cases"));
    let payload_bytes = cases
        .iter()
        .map(|case| {
            let request = case
                .get("request")
                .unwrap_or_else(|| panic!("fixture {relative} case has no request"));
            measure_host_event_payload_bytes(&canonical_json(request).to_string())
                .expect("canonical fixture request length fits u64")
        })
        .collect::<Vec<_>>();
    let dispositions = cases
        .iter()
        .map(|case| {
            case["state"]
                .as_str()
                .unwrap_or_else(|| panic!("fixture {relative} case has no state"))
                .to_string()
        })
        .collect::<Vec<_>>();
    HookHostFixtureMeasurement {
        host: host.to_string(),
        fixture_path: relative,
        fixture_sha256: sha256_hex(&bytes),
        canonical_request_payload_bytes: payload_bytes,
        disposition_vocabulary: dispositions,
    }
}

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&object[key]));
            }
            Value::Object(canonical)
        }
        scalar => scalar.clone(),
    }
}

fn unavailable(metric: &str, required_collection_step: &str) -> UnavailableHookMeasurement {
    UnavailableHookMeasurement {
        metric: metric.to_string(),
        status: "unavailable".to_string(),
        required_collection_step: required_collection_step.to_string(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
