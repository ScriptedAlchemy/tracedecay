use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const COMPATIBILITY_INVENTORY_SCHEMA_V1: &str = "tracedecay.v2.compatibility-inventory/v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityInventoryV1 {
    pub schema: String,
    pub entries: Vec<CompatibilityEntryV1>,
    pub source_family_appendix: Vec<SourceFamilyAppendixEntryV1>,
    pub summaries: InventorySummariesV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityInventoryEnvelopeV1 {
    pub metadata: InventoryRunMetadata,
    pub inventory: CompatibilityInventoryV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryRunMetadata {
    pub binary: String,
    pub commit: String,
    pub generated_at: String,
    pub watermark: String,
    pub source_set_digest: String,
    pub semantic_snapshot_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityEntryV1 {
    pub stable_id: String,
    pub kind: String,
    pub canonical_name: String,
    pub source_refs: Vec<String>,
    pub platform: String,
    pub route_status: RouteStatusV1,
    pub entity_disposition: EntityDispositionV1,
    pub platform_disposition: Option<PlatformDispositionV1>,
    pub owners: InventoryOwnersV1,
    pub readers: Vec<String>,
    pub writers: Vec<String>,
    pub tests: Vec<String>,
    pub gates: InventoryGatesV1,
    pub recovery: String,
    pub delete_by_pr: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryOwnersV1 {
    pub v1_owner: String,
    pub v2_owner: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryGatesV1 {
    pub parity_gate: String,
    pub cutover_gate: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFamilyAppendixEntryV1 {
    pub stable_id: String,
    pub source_family: String,
    pub relative_paths_or_globs: Vec<String>,
    pub tables: Vec<String>,
    pub indexes: Vec<String>,
    pub triggers: Vec<String>,
    pub sidecars: Vec<String>,
    pub owner: String,
    pub entry_refs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventorySummariesV1 {
    pub entries_by_kind: BTreeMap<String, u64>,
    pub entries_by_route_status: BTreeMap<RouteStatusV1, u64>,
    pub entries_by_entity_disposition: BTreeMap<EntityDispositionV1, u64>,
    pub entries_by_platform_disposition: BTreeMap<PlatformDispositionV1, u64>,
}

impl InventorySummariesV1 {
    pub fn from_entries(entries: &[CompatibilityEntryV1]) -> Self {
        let mut summaries = Self::default();
        for entry in entries {
            *summaries
                .entries_by_kind
                .entry(entry.kind.clone())
                .or_default() += 1;
            *summaries
                .entries_by_route_status
                .entry(entry.route_status)
                .or_default() += 1;
            *summaries
                .entries_by_entity_disposition
                .entry(entry.entity_disposition)
                .or_default() += 1;
            if let Some(disposition) = entry.platform_disposition {
                *summaries
                    .entries_by_platform_disposition
                    .entry(disposition)
                    .or_default() += 1;
            }
        }
        summaries
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatusV1 {
    V1Only,
    V2Shadow,
    ParityProven,
    V2Default,
    MigrationOnly,
    Retired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityDispositionV1 {
    Retained,
    Skipped,
    Quarantined,
    Redacted,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformDispositionV1 {
    Supported,
    Alternative,
    Unavailable,
    Untested,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct InventoryValidationError {
    pub field: String,
    pub message: String,
}

impl CompatibilityInventoryV1 {
    pub fn validate(&self) -> Result<(), InventoryValidationError> {
        require_exact("schema", &self.schema, COMPATIBILITY_INVENTORY_SCHEMA_V1)?;
        if self.entries.is_empty() {
            return Err(error(
                "entries",
                "must contain at least one compatibility entry",
            ));
        }
        require_sorted_unique_by("entries", &self.entries, |entry| entry.stable_id.as_str())?;
        require_sorted_unique_by(
            "source_family_appendix",
            &self.source_family_appendix,
            |entry| entry.stable_id.as_str(),
        )?;

        let entry_ids: BTreeSet<&str> = self
            .entries
            .iter()
            .map(|entry| entry.stable_id.as_str())
            .collect();
        for (index, entry) in self.entries.iter().enumerate() {
            entry.validate(&format!("entries[{index}]"))?;
        }
        for (index, appendix) in self.source_family_appendix.iter().enumerate() {
            appendix.validate(&format!("source_family_appendix[{index}]"), &entry_ids)?;
        }
        self.validate_summaries()
    }

    pub fn validate_adapter_deadlines(
        &self,
        current_pr: u32,
    ) -> Result<(), InventoryValidationError> {
        self.validate()?;
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.kind != "adapter" {
                continue;
            }
            let delete_by_pr = parse_delete_by_pr(
                &format!("entries[{index}].delete_by_pr"),
                &entry.delete_by_pr,
            )?;
            if delete_by_pr <= current_pr {
                return Err(error(
                    &format!("entries[{index}].delete_by_pr"),
                    &format!("adapter expired at PR {delete_by_pr}; current PR is {current_pr}"),
                ));
            }
        }
        Ok(())
    }

    fn validate_summaries(&self) -> Result<(), InventoryValidationError> {
        if self.summaries != InventorySummariesV1::from_entries(&self.entries) {
            return Err(error("summaries", "does not exactly match entries"));
        }
        Ok(())
    }
}

impl CompatibilityInventoryEnvelopeV1 {
    pub fn validate(&self) -> Result<(), InventoryValidationError> {
        self.metadata.validate()?;
        self.inventory.validate()?;
        let actual_digest = semantic_snapshot_digest(&self.inventory)?;
        if self.metadata.semantic_snapshot_digest != actual_digest {
            return Err(error(
                "metadata.semantic_snapshot_digest",
                "does not match the canonical semantic inventory",
            ));
        }
        Ok(())
    }
}

impl InventoryRunMetadata {
    pub fn validate(&self) -> Result<(), InventoryValidationError> {
        require_nonempty("metadata.binary", &self.binary)?;
        require_nonempty("metadata.commit", &self.commit)?;
        require_nonempty("metadata.generated_at", &self.generated_at)?;
        require_nonempty("metadata.watermark", &self.watermark)?;
        require_sha256_digest("metadata.source_set_digest", &self.source_set_digest)?;
        require_sha256_digest(
            "metadata.semantic_snapshot_digest",
            &self.semantic_snapshot_digest,
        )
    }
}

impl CompatibilityEntryV1 {
    fn validate(&self, field: &str) -> Result<(), InventoryValidationError> {
        require_stable_id(&format!("{field}.stable_id"), &self.stable_id)?;
        require_nonempty(&format!("{field}.kind"), &self.kind)?;
        require_nonempty(&format!("{field}.canonical_name"), &self.canonical_name)?;
        require_nonempty(&format!("{field}.platform"), &self.platform)?;
        require_sorted_unique(&format!("{field}.source_refs"), &self.source_refs)?;
        require_sorted_unique(&format!("{field}.readers"), &self.readers)?;
        require_sorted_unique(&format!("{field}.writers"), &self.writers)?;
        require_sorted_unique(&format!("{field}.tests"), &self.tests)?;
        if self.tests.is_empty() {
            return Err(error(
                &format!("{field}.tests"),
                "must contain at least one exact test denominator",
            ));
        }
        if self.kind == "platform_operation" && self.platform_disposition.is_none() {
            return Err(error(
                &format!("{field}.platform_disposition"),
                "is required for platform operations",
            ));
        }
        if self.platform_disposition == Some(PlatformDispositionV1::Untested) {
            return Err(error(
                &format!("{field}.platform_disposition"),
                "untested platform operations fail compatibility validation",
            ));
        }
        require_nonempty(&format!("{field}.owners.v1_owner"), &self.owners.v1_owner)?;
        require_nonempty(&format!("{field}.owners.v2_owner"), &self.owners.v2_owner)?;
        require_nonempty(
            &format!("{field}.gates.parity_gate"),
            &self.gates.parity_gate,
        )?;
        require_nonempty(
            &format!("{field}.gates.cutover_gate"),
            &self.gates.cutover_gate,
        )?;
        require_nonempty(&format!("{field}.recovery"), &self.recovery)?;
        if self.kind == "adapter" {
            parse_delete_by_pr(&format!("{field}.delete_by_pr"), &self.delete_by_pr)?;
        } else {
            require_nonempty(&format!("{field}.delete_by_pr"), &self.delete_by_pr)?;
        }
        Ok(())
    }
}

impl SourceFamilyAppendixEntryV1 {
    fn validate(
        &self,
        field: &str,
        entry_ids: &BTreeSet<&str>,
    ) -> Result<(), InventoryValidationError> {
        require_stable_id(&format!("{field}.stable_id"), &self.stable_id)?;
        require_nonempty(&format!("{field}.source_family"), &self.source_family)?;
        require_nonempty(&format!("{field}.owner"), &self.owner)?;
        require_sorted_unique(
            &format!("{field}.relative_paths_or_globs"),
            &self.relative_paths_or_globs,
        )?;
        require_sorted_unique(&format!("{field}.tables"), &self.tables)?;
        require_sorted_unique(&format!("{field}.indexes"), &self.indexes)?;
        require_sorted_unique(&format!("{field}.triggers"), &self.triggers)?;
        require_sorted_unique(&format!("{field}.sidecars"), &self.sidecars)?;
        require_sorted_unique(&format!("{field}.entry_refs"), &self.entry_refs)?;
        for entry_ref in &self.entry_refs {
            if !entry_ids.contains(entry_ref.as_str()) {
                return Err(error(
                    &format!("{field}.entry_refs"),
                    &format!("unknown entry reference {entry_ref:?}"),
                ));
            }
        }
        Ok(())
    }
}

pub fn require_stable_id(field: &str, value: &str) -> Result<(), InventoryValidationError> {
    require_nonempty(field, value)?;
    if value.len() > 128
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(error(field, "must match [A-Za-z0-9][A-Za-z0-9._:-]{0,127}"));
    }
    Ok(())
}

pub fn require_nonempty(field: &str, value: &str) -> Result<(), InventoryValidationError> {
    if value.is_empty() || value.trim() != value {
        return Err(error(
            field,
            "must be non-empty with no surrounding whitespace",
        ));
    }
    Ok(())
}

fn require_sha256_digest(field: &str, value: &str) -> Result<(), InventoryValidationError> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(error(
            field,
            "must use sha256:<64 lowercase hex characters>",
        ));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(error(
            field,
            "must use sha256:<64 lowercase hex characters>",
        ));
    }
    Ok(())
}

fn parse_delete_by_pr(field: &str, value: &str) -> Result<u32, InventoryValidationError> {
    let Some(identifier) = value.strip_prefix("PR ") else {
        return Err(error(
            field,
            "must match PR <number><optional uppercase suffix>",
        ));
    };
    let digit_count = identifier
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let (number, suffix) = identifier.split_at(digit_count);
    if number.is_empty() || number.starts_with('0') || suffix.len() > 2 {
        return Err(error(
            field,
            "must match PR <number><optional uppercase suffix>",
        ));
    }
    if !suffix.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(error(
            field,
            "must match PR <number><optional uppercase suffix>",
        ));
    }
    number
        .parse::<u32>()
        .map_err(|_| error(field, "PR number must be a positive 32-bit integer"))
}

fn semantic_snapshot_digest(
    inventory: &CompatibilityInventoryV1,
) -> Result<String, InventoryValidationError> {
    let bytes = serde_json::to_vec(inventory).map_err(|serialization_error| {
        error(
            "inventory",
            &format!("failed canonical serialization: {serialization_error}"),
        )
    })?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

pub fn require_sorted_unique<T: Ord>(
    field: &str,
    values: &[T],
) -> Result<(), InventoryValidationError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(error(field, "must be strictly sorted and duplicate-free"));
    }
    Ok(())
}

fn require_sorted_unique_by<T, K: Ord + ?Sized>(
    field: &str,
    values: &[T],
    key: impl Fn(&T) -> &K,
) -> Result<(), InventoryValidationError> {
    if values.windows(2).any(|pair| key(&pair[0]) >= key(&pair[1])) {
        return Err(error(
            field,
            "must be strictly sorted by stable ID and duplicate-free",
        ));
    }
    Ok(())
}

fn require_exact(
    field: &str,
    actual: &str,
    expected: &str,
) -> Result<(), InventoryValidationError> {
    if actual != expected {
        return Err(error(field, &format!("must equal {expected:?}")));
    }
    Ok(())
}

fn error(field: &str, message: &str) -> InventoryValidationError {
    InventoryValidationError {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(stable_id: &str) -> CompatibilityEntryV1 {
        CompatibilityEntryV1 {
            stable_id: stable_id.to_owned(),
            kind: "store".to_owned(),
            canonical_name: "activity".to_owned(),
            source_refs: vec!["src/global_db.rs".to_owned()],
            platform: "all".to_owned(),
            route_status: RouteStatusV1::V1Only,
            entity_disposition: EntityDispositionV1::Retained,
            platform_disposition: Some(PlatformDispositionV1::Supported),
            owners: InventoryOwnersV1 {
                v1_owner: "root".to_owned(),
                v2_owner: "tracedecay-store".to_owned(),
            },
            readers: vec!["reader".to_owned()],
            writers: vec!["writer".to_owned()],
            tests: vec!["inventory_is_complete".to_owned()],
            gates: InventoryGatesV1 {
                parity_gate: "PR3-PARITY".to_owned(),
                cutover_gate: "PR37-CUTOVER".to_owned(),
            },
            recovery: "restore read-only archive".to_owned(),
            delete_by_pr: "PR 37".to_owned(),
        }
    }

    fn inventory(entries: Vec<CompatibilityEntryV1>) -> CompatibilityInventoryV1 {
        let summaries = InventorySummariesV1::from_entries(&entries);
        CompatibilityInventoryV1 {
            schema: COMPATIBILITY_INVENTORY_SCHEMA_V1.to_owned(),
            entries,
            source_family_appendix: vec![],
            summaries,
        }
    }

    #[test]
    fn route_status_serialization_is_exact() {
        assert_eq!(
            serde_json::to_string(&RouteStatusV1::V1Only).unwrap(),
            "\"v1_only\""
        );
        assert_eq!(
            serde_json::to_string(&RouteStatusV1::V2Shadow).unwrap(),
            "\"v2_shadow\""
        );
        assert_eq!(
            serde_json::to_string(&RouteStatusV1::ParityProven).unwrap(),
            "\"parity_proven\""
        );
        assert_eq!(
            serde_json::to_string(&RouteStatusV1::V2Default).unwrap(),
            "\"v2_default\""
        );
        assert_eq!(
            serde_json::to_string(&RouteStatusV1::MigrationOnly).unwrap(),
            "\"migration_only\""
        );
        assert_eq!(
            serde_json::to_string(&RouteStatusV1::Retired).unwrap(),
            "\"retired\""
        );
    }

    #[test]
    fn validation_rejects_unsorted_entries() {
        let err = inventory(vec![entry("store:z"), entry("store:a")])
            .validate()
            .unwrap_err();
        assert_eq!(err.field, "entries");
    }

    #[test]
    fn validation_accepts_a_complete_semantic_snapshot() {
        inventory(vec![entry("store:activity")]).validate().unwrap();
    }

    #[test]
    fn validation_rejects_empty_inventory_and_test_denominator() {
        assert_eq!(inventory(vec![]).validate().unwrap_err().field, "entries");

        let mut untested = entry("store:activity");
        untested.tests.clear();
        assert_eq!(
            inventory(vec![untested]).validate().unwrap_err().field,
            "entries[0].tests"
        );
    }

    #[test]
    fn platform_requirements_do_not_apply_to_non_platform_entries() {
        let mut non_platform = entry("store:activity");
        non_platform.platform_disposition = None;
        inventory(vec![non_platform]).validate().unwrap();

        let mut platform = entry("platform:backup:linux");
        platform.kind = "platform_operation".to_owned();
        platform.platform_disposition = None;
        assert_eq!(
            inventory(vec![platform]).validate().unwrap_err().field,
            "entries[0].platform_disposition"
        );

        let mut untested_platform = entry("platform:backup:windows");
        untested_platform.kind = "platform_operation".to_owned();
        untested_platform.platform_disposition = Some(PlatformDispositionV1::Untested);
        assert_eq!(
            inventory(vec![untested_platform])
                .validate()
                .unwrap_err()
                .field,
            "entries[0].platform_disposition"
        );
    }

    #[test]
    fn metadata_envelope_validates_provenance() {
        let semantic_inventory = inventory(vec![entry("store:activity")]);
        let envelope = CompatibilityInventoryEnvelopeV1 {
            metadata: InventoryRunMetadata {
                binary: "tracedecay".to_owned(),
                commit: "abc123".to_owned(),
                generated_at: "2026-07-13T00:00:00Z".to_owned(),
                watermark: "fixture".to_owned(),
                source_set_digest: format!("sha256:{}", "a".repeat(64)),
                semantic_snapshot_digest: semantic_snapshot_digest(&semantic_inventory).unwrap(),
            },
            inventory: semantic_inventory,
        };
        envelope.validate().unwrap();
    }

    #[test]
    fn metadata_envelope_rejects_a_stale_semantic_digest() {
        let envelope = CompatibilityInventoryEnvelopeV1 {
            metadata: InventoryRunMetadata {
                binary: "tracedecay".to_owned(),
                commit: "abc123".to_owned(),
                generated_at: "2026-07-13T00:00:00Z".to_owned(),
                watermark: "fixture".to_owned(),
                source_set_digest: format!("sha256:{}", "a".repeat(64)),
                semantic_snapshot_digest: format!("sha256:{}", "b".repeat(64)),
            },
            inventory: inventory(vec![entry("store:activity")]),
        };
        assert_eq!(
            envelope.validate().unwrap_err().field,
            "metadata.semantic_snapshot_digest"
        );
    }

    #[test]
    fn validation_rejects_unstructured_or_expired_adapter_gates() {
        let mut unstructured = entry("store:activity");
        unstructured.kind = "adapter".to_owned();
        unstructured.delete_by_pr = "eventually".to_owned();
        assert_eq!(
            inventory(vec![unstructured]).validate().unwrap_err().field,
            "entries[0].delete_by_pr"
        );

        let mut expiring = entry("store:activity");
        expiring.kind = "adapter".to_owned();
        assert_eq!(
            inventory(vec![expiring])
                .validate_adapter_deadlines(37)
                .unwrap_err()
                .field,
            "entries[0].delete_by_pr"
        );

        let mut non_adapter = entry("footprint:convergence:scorecard");
        non_adapter.kind = "convergence".to_owned();
        non_adapter.delete_by_pr = "not-applicable".to_owned();
        inventory(vec![non_adapter])
            .validate_adapter_deadlines(37)
            .unwrap();
    }
}
