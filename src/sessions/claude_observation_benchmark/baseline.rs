use serde::{Deserialize, Serialize};

use super::{RECORDS_PER_REPETITION, REDACTION_MARKER};

pub(super) const PROVIDER_BASELINE_SCHEMA_VERSION: u32 = 1;
pub(super) const PROVIDER_BASELINE_CATALOG_ID: &str = "provider-observation-baselines-v1";

const PROVIDERS: &[&str] = &[
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
const PEAK_RESOURCE_FIELDS: &[&str] = &[
    "cpu_ticks",
    "process_write_bytes",
    "database_storage_growth_bytes",
    "peak_rss_kib",
];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderBaseline {
    pub(super) provider: String,
    pub(super) fixture: ProviderFixture,
    pub(super) checks: Vec<String>,
    pub(super) bounds: ProviderBounds,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderFixture {
    pub(super) format: String,
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

#[derive(Debug, Serialize)]
pub(super) struct ProviderBaselineCatalog {
    schema_version: u32,
    catalog_id: &'static str,
    baselines: Vec<ProviderBaseline>,
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
        baselines: expected(),
    }
}

pub(super) fn validate(baselines: &[ProviderBaseline]) {
    assert_eq!(baselines, expected(), "provider baseline contract changed");
    for baseline in baselines {
        let fixture = &baseline.fixture;
        assert_eq!(fixture.format, "deterministic_redacted_synthetic_v1");
        assert!(fixture.redacted_secret.starts_with(REDACTION_MARKER));
        assert!(
            !fixture.redacted_secret.contains("benchmark-secret-"),
            "provider fixture must not retain a secret canary"
        );
    }
}

fn baseline(provider: &str) -> ProviderBaseline {
    ProviderBaseline {
        provider: provider.to_string(),
        fixture: ProviderFixture {
            format: "deterministic_redacted_synthetic_v1".to_string(),
            session_id: format!("benchmark-{provider}-session"),
            message_id: format!("benchmark-{provider}-message-0"),
            redacted_secret: format!("{REDACTION_MARKER} fixture]"),
        },
        checks: strings(CHECKS),
        bounds: ProviderBounds {
            records_per_repetition: RECORDS_PER_REPETITION,
            replay_limit: RECORDS_PER_REPETITION + 1,
            max_backlog_records: RECORDS_PER_REPETITION,
            fair_rotation_providers: PROVIDERS.len(),
            peak_resource_fields: strings(PEAK_RESOURCE_FIELDS),
        },
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
