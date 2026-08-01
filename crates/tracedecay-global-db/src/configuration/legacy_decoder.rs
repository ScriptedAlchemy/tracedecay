//! Read-only decoding of the legacy `config.json` surface.
//!
//! This module does not read files or process environment variables itself.
//! Callers supply raw JSON and an explicit environment map, then receive
//! ordered, redacted migration inputs. The fixed order is legacy host profile,
//! `config.json`, and finally parseable environment overrides. `root_dir` is
//! always quarantined and can never become a durable authority reference.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use thiserror::Error;
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    DIAGNOSTICS_PREWARM_SETTING_KEY, INDEX_EXCLUDE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    SYNC_AUTO_INIT_SETTING_KEY, SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY, SYNC_AUTO_WATCH_SETTING_KEY,
    SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY, SYNC_BRANCH_GC_DAYS_SETTING_KEY,
    SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY, SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
    SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY, SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
    SYNC_READ_REFRESH_SETTING_KEY, SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
    SYNC_SESSION_START_SYNC_SETTING_KEY, SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
    SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY, SYNC_WATCH_MAX_PROJECTS_SETTING_KEY, SettingKey,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::{DomainError, ManifestDigest};

use crate::configuration::resolver::{
    ConfigurationLayerV1, ConfigurationResolutionError, ConfigurationResolutionInputSourceV1,
    ConfigurationResolutionInputV1, ConfigurationResolutionV1, resolve_configuration_inputs,
};
use crate::configuration::migration::{
    ConfigurationMigrationQuarantineReasonV1, LegacyConfigurationEntryV1,
    LegacyConfigurationSourceKindV1, ReadonlyLegacyConfigurationInputV1,
    ReadonlyLegacyConfigurationInputsV1,
};

use super::{ConfigurationRegistry, ConfigurationRegistryError};

const SOURCE_KEY_DIGEST_DOMAIN: &str = "tracedecay.configuration.legacy-source-key.v1";
const VALUE_DIGEST_DOMAIN: &str = "tracedecay.configuration.legacy-value.v1";

/// Canonical, already-authorized target identity supplied by the migration
/// boundary. It deliberately contains no path; `root_dir` cannot determine a
/// target layer or revision.
#[derive(Clone, Debug)]
pub struct LegacyConfigurationDecodeTargetV1 {
    pub target_layer: ConfigurationLayerIdV1,
    pub target_revision_id: ConfigurationRevisionId,
}

impl LegacyConfigurationDecodeTargetV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target_layer.validate()?;
        self.target_revision_id.validate()
    }
}

#[derive(Debug, Error)]
pub enum LegacyConfigurationDecoderError {
    #[error("legacy configuration decode target is invalid: {0}")]
    Domain(#[from] DomainError),
    #[error("legacy configuration decoder produced an invalid registry value: {0}")]
    Registry(#[from] ConfigurationRegistryError),
    #[error("legacy configuration resolution failed: {0}")]
    Resolution(#[from] ConfigurationResolutionError),
    #[error("legacy configuration input repeats setting {0}")]
    DuplicateSetting(SettingKey),
}

/// Decode persisted JSON field by field. A malformed top-level document, a
/// malformed known field, or an unknown field yields an opaque quarantine
/// candidate without preventing other known fields from being retained.
pub fn decode_legacy_config_json(
    config_json: &str,
    target: &LegacyConfigurationDecodeTargetV1,
) -> Result<ReadonlyLegacyConfigurationInputV1, DomainError> {
    target.validate()?;
    match serde_json::from_str::<Value>(config_json) {
        Ok(value) => decode_legacy_config_value(&value, target),
        Err(_) => Ok(config_input(
            target,
            vec![quarantined_entry(
                LegacyConfigurationSourceKindV1::ConfigJson,
                "config_json",
                &Value::String(config_json.to_owned()),
                ConfigurationMigrationQuarantineReasonV1::Undecodable,
            )?],
        )),
    }
}

/// Decode an already-parsed legacy document. This form lets migration callers
/// retain field-level behavior even when one nested value is malformed.
pub fn decode_legacy_config_value(
    config_json: &Value,
    target: &LegacyConfigurationDecodeTargetV1,
) -> Result<ReadonlyLegacyConfigurationInputV1, DomainError> {
    target.validate()?;
    let Some(object) = config_json.as_object() else {
        return Ok(config_input(
            target,
            vec![quarantined_entry(
                LegacyConfigurationSourceKindV1::ConfigJson,
                "config_json",
                config_json,
                ConfigurationMigrationQuarantineReasonV1::Undecodable,
            )?],
        ));
    };

    let source_kind = LegacyConfigurationSourceKindV1::ConfigJson;
    let mut entries = Vec::new();

    if let Some(value) = object.get("root_dir") {
        entries.push(quarantined_entry(
            source_kind,
            "root_dir",
            value,
            ConfigurationMigrationQuarantineReasonV1::PathDerivedAuthority,
        )?);
    }

    decode_config_field(
        &mut entries,
        source_kind,
        "exclude",
        INDEX_EXCLUDE_SETTING_KEY,
        object.get("exclude"),
        decode_string_list,
    )?;
    decode_config_field(
        &mut entries,
        source_kind,
        "include",
        INDEX_INCLUDE_SETTING_KEY,
        object.get("include"),
        decode_string_list,
    )?;
    decode_config_field(
        &mut entries,
        source_kind,
        "max_file_size",
        INDEX_MAX_FILE_SIZE_SETTING_KEY,
        object.get("max_file_size"),
        decode_unsigned,
    )?;
    decode_config_field(
        &mut entries,
        source_kind,
        "extract_docstrings",
        INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY,
        object.get("extract_docstrings"),
        decode_boolean,
    )?;
    decode_config_field(
        &mut entries,
        source_kind,
        "track_call_sites",
        INDEX_TRACK_CALL_SITES_SETTING_KEY,
        object.get("track_call_sites"),
        decode_boolean,
    )?;
    decode_config_field(
        &mut entries,
        source_kind,
        "git_ignore",
        INDEX_GIT_IGNORE_SETTING_KEY,
        object.get("git_ignore"),
        decode_boolean,
    )?;
    decode_config_field(
        &mut entries,
        source_kind,
        "diagnostics_prewarm",
        DIAGNOSTICS_PREWARM_SETTING_KEY,
        object.get("diagnostics_prewarm"),
        decode_boolean,
    )?;

    decode_sync_fields(&mut entries, source_kind, object.get("sync"))?;
    decode_telemetry_fields(&mut entries, source_kind, object.get("telemetry"))?;
    decode_unknown_object_fields(
        &mut entries,
        source_kind,
        "",
        object,
        is_known_top_level_field,
    )?;

    Ok(config_input(target, entries))
}

/// Decode only explicitly supplied environment values. Missing values do not
/// materialize defaults; registry defaults and persisted `config.json` remain
/// visible in resolution provenance. Every parseable override is ordered after
/// the config JSON input by [`ReadonlyLegacyConfigurationInputsV1`].
pub fn decode_legacy_environment_overrides(
    environment: &BTreeMap<String, String>,
    target: &LegacyConfigurationDecodeTargetV1,
) -> Result<ReadonlyLegacyConfigurationInputV1, DomainError> {
    target.validate()?;
    let source_kind = LegacyConfigurationSourceKindV1::Environment;
    let mut entries = Vec::new();

    let mut mapped_settings = BTreeSet::new();
    for (source_key, raw) in environment {
        let raw_value = Value::String(raw.clone());
        let decoded = match source_key.as_str() {
            "TRACEDECAY_DIAGNOSTICS_PREWARM" => Some((
                DIAGNOSTICS_PREWARM_SETTING_KEY,
                parse_legacy_bool(raw).map(ConfigurationValueV1::Boolean),
            )),
            "TRACEDECAY_SYNC_AUTO_WATCH" => Some((
                SYNC_AUTO_WATCH_SETTING_KEY,
                parse_legacy_bool(raw).map(ConfigurationValueV1::Boolean),
            )),
            "TRACEDECAY_SYNC_WATCH_DEBOUNCE_MS" => Some((
                SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_WATCH_MAX_DELAY_MS" => Some((
                SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_WATCH_MAX_PROJECTS" => Some((
                SYNC_WATCH_MAX_PROJECTS_SETTING_KEY,
                parse_legacy_usize(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_READ_REFRESH" => Some((
                SYNC_READ_REFRESH_SETTING_KEY,
                parse_legacy_bool(raw).map(ConfigurationValueV1::Boolean),
            )),
            "TRACEDECAY_SYNC_READ_COOLDOWN_SECS" => Some((
                SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_SESSION_START_SYNC" => Some((
                SYNC_SESSION_START_SYNC_SETTING_KEY,
                parse_legacy_bool(raw).map(ConfigurationValueV1::Boolean),
            )),
            "TRACEDECAY_SYNC_SESSION_START_STALE_THRESHOLD_SECS" => Some((
                SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_BACKSTOP_INTERVAL_MINS" => Some((
                SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_FULL_SYNC_ESCALATION_FILES" => Some((
                SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
                parse_legacy_usize(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_MAX_CONCURRENT_SYNCS" => Some((
                SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
                parse_legacy_usize(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_BRANCH_GC_DAYS" => Some((
                SYNC_BRANCH_GC_DAYS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_ORPHAN_DB_GC_DAYS" => Some((
                SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
                parse_legacy_u64(raw).map(ConfigurationValueV1::Unsigned),
            )),
            "TRACEDECAY_SYNC_AUTO_INIT" => Some((
                SYNC_AUTO_INIT_SETTING_KEY,
                parse_legacy_bool(raw).map(ConfigurationValueV1::Boolean),
            )),
            "TRACEDECAY_SYNC_AUTO_TRACK_PR_BRANCHES" => Some((
                SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
                parse_legacy_bool(raw).map(ConfigurationValueV1::Boolean),
            )),
            "TRACEDECAY_SYNC_AUTO_TRACK_PR_POLL_SECS" => Some((
                SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
                parse_legacy_u64(raw).map(|value| {
                    ConfigurationValueV1::Unsigned(
                        value.max(crate::configuration::registry::MIN_AUTO_TRACK_PR_POLL_SECS),
                    )
                }),
            )),
            _ => None,
        };

        match decoded {
            Some((setting_key, Some(value))) if mapped_settings.insert(setting_key) => entries
                .push(decoded_entry(
                    source_kind,
                    source_key,
                    &raw_value,
                    setting_key,
                    value,
                )?),
            Some((_setting_key, Some(_value))) => entries.push(quarantined_entry(
                source_kind,
                source_key,
                &raw_value,
                ConfigurationMigrationQuarantineReasonV1::DuplicateKey,
            )?),
            Some((_setting_key, None)) => entries.push(quarantined_entry(
                source_kind,
                source_key,
                &raw_value,
                ConfigurationMigrationQuarantineReasonV1::Undecodable,
            )?),
            None => entries.push(quarantined_entry(
                source_kind,
                source_key,
                &raw_value,
                ConfigurationMigrationQuarantineReasonV1::UnknownKey,
            )?),
        }
    }

    Ok(ReadonlyLegacyConfigurationInputV1 {
        source_kind,
        target_layer: target.target_layer.clone(),
        target_revision_id: target.target_revision_id.clone(),
        entries,
    })
}

/// Decode the persisted source and environment overrides into the one ordered
/// migration input shape. The input digest is stable for equivalent JSON and
/// sorted environment maps, and changes when a source value or provenance
/// source changes.
pub fn decode_legacy_configuration_inputs(
    config_json: &str,
    environment: &BTreeMap<String, String>,
    target: &LegacyConfigurationDecodeTargetV1,
) -> Result<ReadonlyLegacyConfigurationInputsV1, DomainError> {
    let inputs = ReadonlyLegacyConfigurationInputsV1 {
        inputs: vec![
            decode_legacy_config_json(config_json, target)?,
            decode_legacy_environment_overrides(environment, target)?,
        ],
    };
    inputs.validate()?;
    Ok(inputs)
}

/// Convert already-decoded migration snapshots into explicit resolver inputs.
/// Quarantined and incomplete entries have no resolution effect; the registry
/// default remains visible instead. This is intentionally read-only and does
/// not call process environment APIs or legacy file readers.
pub fn legacy_resolution_inputs(
    registry: &ConfigurationRegistry,
    inputs: &ReadonlyLegacyConfigurationInputsV1,
) -> Result<Vec<ConfigurationResolutionInputV1>, LegacyConfigurationDecoderError> {
    inputs.validate()?;
    let mut resolution_inputs = Vec::with_capacity(inputs.inputs.len());
    for input in &inputs.inputs {
        let mut entries = BTreeMap::new();
        for entry in &input.entries {
            if entry.quarantine_reason.is_some() {
                continue;
            }
            let (Some(key), Some(value)) = (&entry.setting_key, &entry.value) else {
                continue;
            };
            registry.validate_value(key, value)?;
            if entries.insert(key.clone(), value.clone()).is_some() {
                return Err(LegacyConfigurationDecoderError::DuplicateSetting(
                    key.clone(),
                ));
            }
        }
        resolution_inputs.push(ConfigurationResolutionInputV1 {
            source: resolution_source(input.source_kind),
            layer: ConfigurationLayerV1 {
                layer: input.target_layer.clone(),
                revision_id: input.target_revision_id.clone(),
                entries,
            },
        });
    }
    Ok(resolution_inputs)
}

/// Resolve an ordered legacy snapshot through the sole configuration resolver.
/// This is the migration parity seam; production readers remain unwired.
pub fn resolve_legacy_configuration_inputs(
    registry: &ConfigurationRegistry,
    inputs: &ReadonlyLegacyConfigurationInputsV1,
) -> Result<ConfigurationResolutionV1, LegacyConfigurationDecoderError> {
    let resolution_inputs = legacy_resolution_inputs(registry, inputs)?;
    Ok(resolve_configuration_inputs(registry, &resolution_inputs)?)
}

fn config_input(
    target: &LegacyConfigurationDecodeTargetV1,
    entries: Vec<LegacyConfigurationEntryV1>,
) -> ReadonlyLegacyConfigurationInputV1 {
    ReadonlyLegacyConfigurationInputV1 {
        source_kind: LegacyConfigurationSourceKindV1::ConfigJson,
        target_layer: target.target_layer.clone(),
        target_revision_id: target.target_revision_id.clone(),
        entries,
    }
}

fn decode_sync_fields(
    entries: &mut Vec<LegacyConfigurationEntryV1>,
    source_kind: LegacyConfigurationSourceKindV1,
    sync: Option<&Value>,
) -> Result<(), DomainError> {
    let Some(sync) = sync else {
        return Ok(());
    };
    let Some(sync) = sync.as_object() else {
        entries.push(quarantined_entry(
            source_kind,
            "sync",
            sync,
            ConfigurationMigrationQuarantineReasonV1::Undecodable,
        )?);
        return Ok(());
    };

    decode_config_field(
        entries,
        source_kind,
        "sync.auto_watch",
        SYNC_AUTO_WATCH_SETTING_KEY,
        sync.get("auto_watch"),
        decode_boolean,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.watch_debounce_ms",
        SYNC_WATCH_DEBOUNCE_MS_SETTING_KEY,
        sync.get("watch_debounce_ms"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.watch_max_delay_ms",
        SYNC_WATCH_MAX_DELAY_MS_SETTING_KEY,
        sync.get("watch_max_delay_ms"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.watch_max_projects",
        SYNC_WATCH_MAX_PROJECTS_SETTING_KEY,
        sync.get("watch_max_projects"),
        decode_usize,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.read_refresh",
        SYNC_READ_REFRESH_SETTING_KEY,
        sync.get("read_refresh"),
        decode_boolean,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.read_cooldown_secs",
        SYNC_READ_COOLDOWN_SECS_SETTING_KEY,
        sync.get("read_cooldown_secs"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.session_start_sync",
        SYNC_SESSION_START_SYNC_SETTING_KEY,
        sync.get("session_start_sync"),
        decode_boolean,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.session_start_stale_threshold_secs",
        SYNC_SESSION_START_STALE_THRESHOLD_SECS_SETTING_KEY,
        sync.get("session_start_stale_threshold_secs"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.backstop_interval_mins",
        SYNC_BACKSTOP_INTERVAL_MINS_SETTING_KEY,
        sync.get("backstop_interval_mins"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.full_sync_escalation_files",
        SYNC_FULL_SYNC_ESCALATION_FILES_SETTING_KEY,
        sync.get("full_sync_escalation_files"),
        decode_usize,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.max_concurrent_syncs",
        SYNC_MAX_CONCURRENT_SYNCS_SETTING_KEY,
        sync.get("max_concurrent_syncs"),
        decode_usize,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.branch_gc_days",
        SYNC_BRANCH_GC_DAYS_SETTING_KEY,
        sync.get("branch_gc_days"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.orphan_db_gc_days",
        SYNC_ORPHAN_DB_GC_DAYS_SETTING_KEY,
        sync.get("orphan_db_gc_days"),
        decode_unsigned,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.auto_init",
        SYNC_AUTO_INIT_SETTING_KEY,
        sync.get("auto_init"),
        decode_boolean,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.auto_track_pr_branches",
        SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
        sync.get("auto_track_pr_branches"),
        decode_boolean,
    )?;
    decode_config_field(
        entries,
        source_kind,
        "sync.auto_track_pr_poll_secs",
        SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
        sync.get("auto_track_pr_poll_secs"),
        decode_pr_autotrack_poll_secs,
    )?;
    decode_unknown_object_fields(entries, source_kind, "sync", sync, is_known_sync_field)
}

fn decode_telemetry_fields(
    entries: &mut Vec<LegacyConfigurationEntryV1>,
    source_kind: LegacyConfigurationSourceKindV1,
    telemetry: Option<&Value>,
) -> Result<(), DomainError> {
    let Some(telemetry) = telemetry else {
        return Ok(());
    };
    let Some(telemetry) = telemetry.as_object() else {
        entries.push(quarantined_entry(
            source_kind,
            "telemetry",
            telemetry,
            ConfigurationMigrationQuarantineReasonV1::Undecodable,
        )?);
        return Ok(());
    };
    decode_config_field(
        entries,
        source_kind,
        "telemetry.timings",
        TELEMETRY_TIMINGS_SETTING_KEY,
        telemetry.get("timings"),
        decode_boolean,
    )?;
    decode_unknown_object_fields(
        entries,
        source_kind,
        "telemetry",
        telemetry,
        is_known_telemetry_field,
    )
}

fn decode_config_field(
    entries: &mut Vec<LegacyConfigurationEntryV1>,
    source_kind: LegacyConfigurationSourceKindV1,
    source_key: &str,
    setting_key: &str,
    raw: Option<&Value>,
    decode: impl FnOnce(&Value) -> Option<ConfigurationValueV1>,
) -> Result<(), DomainError> {
    let Some(raw) = raw else {
        return Ok(());
    };
    match decode(raw) {
        Some(value) => entries.push(decoded_entry(
            source_kind,
            source_key,
            raw,
            setting_key,
            value,
        )?),
        None => entries.push(quarantined_entry(
            source_kind,
            source_key,
            raw,
            ConfigurationMigrationQuarantineReasonV1::Undecodable,
        )?),
    }
    Ok(())
}

fn decode_unknown_object_fields(
    entries: &mut Vec<LegacyConfigurationEntryV1>,
    source_kind: LegacyConfigurationSourceKindV1,
    prefix: &str,
    object: &Map<String, Value>,
    known: fn(&str) -> bool,
) -> Result<(), DomainError> {
    let mut unknown_keys: Vec<_> = object
        .keys()
        .filter(|key| !known(key))
        .map(String::as_str)
        .collect();
    unknown_keys.sort_unstable();
    for key in unknown_keys {
        let source_key = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        let value = object.get(key).ok_or(DomainError::NonCanonical {
            field: "legacy configuration object key",
        })?;
        entries.push(quarantined_entry(
            source_kind,
            &source_key,
            value,
            ConfigurationMigrationQuarantineReasonV1::UnknownKey,
        )?);
    }
    Ok(())
}

fn decoded_entry(
    source_kind: LegacyConfigurationSourceKindV1,
    source_key: &str,
    raw_value: &Value,
    setting_key: &str,
    value: ConfigurationValueV1,
) -> Result<LegacyConfigurationEntryV1, DomainError> {
    if value.validate().is_err() {
        return quarantined_entry(
            source_kind,
            source_key,
            raw_value,
            ConfigurationMigrationQuarantineReasonV1::DeprecatedInvalid,
        );
    }
    Ok(LegacyConfigurationEntryV1 {
        source_key_digest: source_key_digest(source_kind, source_key)?,
        setting_key: Some(SettingKey::new(setting_key)?),
        value: Some(value),
        redacted_value_digest: value_digest(source_kind, source_key, raw_value)?,
        quarantine_reason: None,
    })
}

fn quarantined_entry(
    source_kind: LegacyConfigurationSourceKindV1,
    source_key: &str,
    raw_value: &Value,
    reason: ConfigurationMigrationQuarantineReasonV1,
) -> Result<LegacyConfigurationEntryV1, DomainError> {
    Ok(LegacyConfigurationEntryV1 {
        source_key_digest: source_key_digest(source_kind, source_key)?,
        setting_key: None,
        value: None,
        redacted_value_digest: value_digest(source_kind, source_key, raw_value)?,
        quarantine_reason: Some(reason),
    })
}

fn source_key_digest(
    source_kind: LegacyConfigurationSourceKindV1,
    source_key: &str,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(SOURCE_KEY_DIGEST_DOMAIN, source_kind.as_str(), source_key))
}

fn value_digest(
    source_kind: LegacyConfigurationSourceKindV1,
    source_key: &str,
    value: &Value,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(VALUE_DIGEST_DOMAIN, source_kind.as_str(), source_key, value))
}

fn decode_boolean(value: &Value) -> Option<ConfigurationValueV1> {
    value.as_bool().map(ConfigurationValueV1::Boolean)
}

fn decode_unsigned(value: &Value) -> Option<ConfigurationValueV1> {
    value.as_u64().map(ConfigurationValueV1::Unsigned)
}

fn decode_usize(value: &Value) -> Option<ConfigurationValueV1> {
    usize::try_from(value.as_u64()?)
        .ok()
        .map(|value| ConfigurationValueV1::Unsigned(value as u64))
}

fn decode_pr_autotrack_poll_secs(value: &Value) -> Option<ConfigurationValueV1> {
    value.as_u64().map(|value| {
        ConfigurationValueV1::Unsigned(value.max(crate::configuration::registry::MIN_AUTO_TRACK_PR_POLL_SECS))
    })
}

fn decode_string_list(value: &Value) -> Option<ConfigurationValueV1> {
    let values: Vec<String> = value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    Some(ConfigurationValueV1::StringList(values))
}

fn parse_legacy_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn parse_legacy_u64(raw: &str) -> Option<u64> {
    raw.trim().parse().ok()
}

fn parse_legacy_usize(raw: &str) -> Option<u64> {
    raw.trim().parse::<usize>().ok().map(|value| value as u64)
}

fn resolution_source(
    source_kind: LegacyConfigurationSourceKindV1,
) -> ConfigurationResolutionInputSourceV1 {
    match source_kind {
        LegacyConfigurationSourceKindV1::HostProfile => {
            ConfigurationResolutionInputSourceV1::LegacyHostProfile
        }
        LegacyConfigurationSourceKindV1::ConfigJson => {
            ConfigurationResolutionInputSourceV1::LegacyConfigJson
        }
        LegacyConfigurationSourceKindV1::Environment => {
            ConfigurationResolutionInputSourceV1::LegacyEnvironment
        }
    }
}

fn is_known_top_level_field(key: &str) -> bool {
    matches!(
        key,
        "version"
            | "root_dir"
            | "exclude"
            | "include"
            | "max_file_size"
            | "extract_docstrings"
            | "track_call_sites"
            | "git_ignore"
            | "diagnostics_prewarm"
            | "sync"
            | "telemetry"
    )
}

fn is_known_sync_field(key: &str) -> bool {
    matches!(
        key,
        "auto_watch"
            | "watch_debounce_ms"
            | "watch_max_delay_ms"
            | "watch_max_projects"
            | "read_refresh"
            | "read_cooldown_secs"
            | "session_start_sync"
            | "session_start_stale_threshold_secs"
            | "backstop_interval_mins"
            | "full_sync_escalation_files"
            | "max_concurrent_syncs"
            | "branch_gc_days"
            | "orphan_db_gc_days"
            | "auto_init"
            | "auto_track_pr_branches"
            | "auto_track_pr_poll_secs"
    )
}

fn is_known_telemetry_field(key: &str) -> bool {
    key == "timings"
}
