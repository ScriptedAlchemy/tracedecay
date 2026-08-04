use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

use super::error::DomainError;
use super::id::{ShardId, validate_canonical_string};
use super::time::UtcMicros;
use super::watermark::ShardWatermark;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ShardDispositionV1 {
    Searched,
    Skipped,
    Stale,
    Unavailable,
    Incompatible,
    Locked,
    Redacted,
    Truncated,
}

/// Whether the complete shard universe was known when coverage was captured.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageUniverseKnowledgeV1 {
    Known,
    #[default]
    Unknown,
}

/// Registry-owned retention class code. The domain records the code without
/// implementing retention policy or storage behavior.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct RetentionClass(String);

impl RetentionClass {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_canonical_string(&value, "RetentionClass")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RetentionClass {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRetentionWatermark {
    pub evaluated_at: UtcMicros,
    pub cutoffs: BTreeMap<RetentionClass, UtcMicros>,
}

/// Exact per-shard disposition captured with a retrieval result.
///
/// The in-memory authority is one map: a shard cannot occupy two disposition
/// groups. Custom serde preserves the grouped-vector V1 fixture wire form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoverageReportV1 {
    pub dispositions: BTreeMap<ShardId, ShardDispositionV1>,
    pub freshness: BTreeMap<ShardId, ShardWatermark>,
    pub retention_watermark: Option<EvidenceRetentionWatermark>,
    pub universe: CoverageUniverseKnowledgeV1,
}

impl CoverageReportV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        for shard in self.dispositions.keys() {
            shard.validate()?;
        }
        for (shard, watermark) in &self.freshness {
            shard.validate()?;
            if !self.dispositions.contains_key(shard) || shard != &watermark.shard_id {
                return Err(DomainError::UnknownReference {
                    field: "coverage freshness shard",
                });
            }
        }
        if let Some(retention) = &self.retention_watermark {
            for class in retention.cutoffs.keys() {
                validate_canonical_string(class.as_str(), "RetentionClass")?;
            }
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        if self.universe == CoverageUniverseKnowledgeV1::Unknown
            || self.dispositions.is_empty()
            || self
                .dispositions
                .values()
                .any(|disposition| *disposition != ShardDispositionV1::Searched)
        {
            return false;
        }
        true
    }

    pub fn disposition(&self, shard: &ShardId) -> Option<ShardDispositionV1> {
        self.dispositions.get(shard).copied()
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageWireV1 {
    #[serde(default)]
    searched: Vec<ShardId>,
    #[serde(default)]
    skipped: Vec<ShardId>,
    #[serde(default)]
    stale: Vec<ShardId>,
    #[serde(default)]
    unavailable: Vec<ShardId>,
    #[serde(default)]
    incompatible: Vec<ShardId>,
    #[serde(default)]
    locked: Vec<ShardId>,
    #[serde(default)]
    redacted: Vec<ShardId>,
    #[serde(default)]
    truncated: Vec<ShardId>,
    #[serde(default)]
    freshness: BTreeMap<ShardId, ShardWatermark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retention_watermark: Option<EvidenceRetentionWatermark>,
    #[serde(default)]
    unknown_coverage: Option<bool>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CoverageWireRefV1<'a> {
    #[serde(default)]
    searched: Vec<&'a ShardId>,
    #[serde(default)]
    skipped: Vec<&'a ShardId>,
    #[serde(default)]
    stale: Vec<&'a ShardId>,
    #[serde(default)]
    unavailable: Vec<&'a ShardId>,
    #[serde(default)]
    incompatible: Vec<&'a ShardId>,
    #[serde(default)]
    locked: Vec<&'a ShardId>,
    #[serde(default)]
    redacted: Vec<&'a ShardId>,
    #[serde(default)]
    truncated: Vec<&'a ShardId>,
    #[serde(default)]
    freshness: &'a BTreeMap<ShardId, ShardWatermark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retention_watermark: Option<&'a EvidenceRetentionWatermark>,
    #[serde(default)]
    unknown_coverage: Option<bool>,
}

impl CoverageWireV1 {
    fn add_group(
        dispositions: &mut BTreeMap<ShardId, ShardDispositionV1>,
        shards: Vec<ShardId>,
        disposition: ShardDispositionV1,
    ) -> Result<(), DomainError> {
        for shard in shards {
            if dispositions.insert(shard, disposition).is_some() {
                return Err(DomainError::DuplicateId {
                    field: "coverage dispositions",
                });
            }
        }
        Ok(())
    }
}

impl Serialize for CoverageReportV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut searched = Vec::new();
        let mut skipped = Vec::new();
        let mut stale = Vec::new();
        let mut unavailable = Vec::new();
        let mut incompatible = Vec::new();
        let mut locked = Vec::new();
        let mut redacted = Vec::new();
        let mut truncated = Vec::new();
        for (shard, disposition) in &self.dispositions {
            let group = match disposition {
                ShardDispositionV1::Searched => &mut searched,
                ShardDispositionV1::Skipped => &mut skipped,
                ShardDispositionV1::Stale => &mut stale,
                ShardDispositionV1::Unavailable => &mut unavailable,
                ShardDispositionV1::Incompatible => &mut incompatible,
                ShardDispositionV1::Locked => &mut locked,
                ShardDispositionV1::Redacted => &mut redacted,
                ShardDispositionV1::Truncated => &mut truncated,
            };
            group.push(shard);
        }
        CoverageWireRefV1 {
            searched,
            skipped,
            stale,
            unavailable,
            incompatible,
            locked,
            redacted,
            truncated,
            freshness: &self.freshness,
            retention_watermark: self.retention_watermark.as_ref(),
            unknown_coverage: Some(self.universe == CoverageUniverseKnowledgeV1::Unknown),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CoverageReportV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CoverageWireV1::deserialize(deserializer)?;
        let mut dispositions = BTreeMap::new();
        CoverageWireV1::add_group(
            &mut dispositions,
            wire.searched,
            ShardDispositionV1::Searched,
        )
        .and_then(|_| {
            CoverageWireV1::add_group(&mut dispositions, wire.skipped, ShardDispositionV1::Skipped)
        })
        .and_then(|_| {
            CoverageWireV1::add_group(&mut dispositions, wire.stale, ShardDispositionV1::Stale)
        })
        .and_then(|_| {
            CoverageWireV1::add_group(
                &mut dispositions,
                wire.unavailable,
                ShardDispositionV1::Unavailable,
            )
        })
        .and_then(|_| {
            CoverageWireV1::add_group(
                &mut dispositions,
                wire.incompatible,
                ShardDispositionV1::Incompatible,
            )
        })
        .and_then(|_| {
            CoverageWireV1::add_group(&mut dispositions, wire.locked, ShardDispositionV1::Locked)
        })
        .and_then(|_| {
            CoverageWireV1::add_group(
                &mut dispositions,
                wire.redacted,
                ShardDispositionV1::Redacted,
            )
        })
        .and_then(|_| {
            CoverageWireV1::add_group(
                &mut dispositions,
                wire.truncated,
                ShardDispositionV1::Truncated,
            )
        })
        .map_err(serde::de::Error::custom)?;

        let report = Self {
            dispositions,
            freshness: wire.freshness,
            retention_watermark: wire.retention_watermark,
            universe: match wire.unknown_coverage {
                Some(false) => CoverageUniverseKnowledgeV1::Known,
                Some(true) | None => CoverageUniverseKnowledgeV1::Unknown,
            },
        };
        report.validate().map_err(serde::de::Error::custom)?;
        Ok(report)
    }
}
