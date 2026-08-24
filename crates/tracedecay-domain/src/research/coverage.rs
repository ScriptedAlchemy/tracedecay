use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

use serde::de::{IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::DomainError;
use super::id::{
    AuthorityEpoch, BrainId, BrainNodeId, EntityVersionId, ManifestDigest, ShardId,
    StoreAuthorityId, ensure_unique, validate_canonical_string,
};
use super::time::UtcMicros;
use super::watermark::{ShardWatermark, VectorWatermark};

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BrainNodeRoleV1 {
    Standalone,
    Authority,
    RemoteClient,
    ReadReplica,
    Standby,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadConsistencyV1 {
    Authoritative,
    BoundedStale { max_lag_micros: u64 },
    OfflineCache,
}

/// Signed cache-grant state plus the authority-side evidence verified for this
/// coverage evaluation.
///
/// The grant digest identifies the immutable signed snapshot. Its validity and
/// purge frontier are carried directly so freshness cannot be inferred from an
/// unbound cache timestamp. Optional verified fields represent current evidence;
/// absent evidence never implies current placement, authority, or revocation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCacheGrantSnapshotV1 {
    pub grant_digest: ManifestDigest,
    pub issued_at: UtcMicros,
    pub not_after: UtcMicros,
    pub grant_revocation_generation: u64,
    pub purge_frontier: VectorWatermark,
    pub verified_placement_version: Option<EntityVersionId>,
    pub verified_authority_id: Option<StoreAuthorityId>,
    pub verified_authority_epoch: Option<AuthorityEpoch>,
    pub verified_revocation_generation: Option<u64>,
    pub verified_purge_frontier: Option<VectorWatermark>,
}

impl VerifiedCacheGrantSnapshotV1 {
    fn validate(&self) -> Result<(), DomainError> {
        self.grant_digest.validate()?;
        if self.not_after <= self.issued_at {
            return Err(DomainError::NonCanonical {
                field: "cache grant validity",
            });
        }
        for shard in self.purge_frontier.components.keys() {
            shard.validate()?;
        }
        if let Some(verified_purge_frontier) = &self.verified_purge_frontier {
            for shard in verified_purge_frontier.components.keys() {
                shard.validate()?;
            }
        }
        if let Some(placement_version) = &self.verified_placement_version {
            placement_version.validate()?;
        }
        if let Some(authority_id) = &self.verified_authority_id {
            authority_id.validate()?;
        }
        if self.verified_authority_id.is_some() != self.verified_authority_epoch.is_some() {
            return Err(DomainError::UnknownReference {
                field: "cache grant verified authority",
            });
        }
        Ok(())
    }

    fn proves_current_access(
        &self,
        evaluated_at: UtcMicros,
        cache_not_after: UtcMicros,
        placement_version: &EntityVersionId,
        authority_id: &StoreAuthorityId,
        authority_epoch: AuthorityEpoch,
    ) -> bool {
        self.issued_at <= evaluated_at
            && self.not_after == cache_not_after
            && self.not_after > evaluated_at
            && self.verified_placement_version.as_ref() == Some(placement_version)
            && self.verified_authority_id.as_ref() == Some(authority_id)
            && self.verified_authority_epoch == Some(authority_epoch)
            && self.verified_revocation_generation == Some(self.grant_revocation_generation)
            && self
                .verified_purge_frontier
                .as_ref()
                .is_some_and(|verified| verified.dominates(&self.purge_frontier))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteShardCoverageV1 {
    pub shard_id: ShardId,
    pub authority_id: StoreAuthorityId,
    pub authority_epoch: AuthorityEpoch,
    pub served_by_node: BrainNodeId,
    pub served_by_role: BrainNodeRoleV1,
    pub captured_watermark: Option<ShardWatermark>,
    pub cache_generation: Option<ManifestDigest>,
    pub cache_not_after: Option<UtcMicros>,
    pub cache_age_micros: Option<u64>,
    pub cache_grant_snapshot: Option<VerifiedCacheGrantSnapshotV1>,
    pub sync_lag_micros: Option<u64>,
    pub pending_local_observations: u64,
    pub pending_tombstone_acks: u64,
}

impl RemoteShardCoverageV1 {
    fn validate(&self) -> Result<(), DomainError> {
        self.shard_id.validate()?;
        self.authority_id.validate()?;
        self.served_by_node.validate()?;
        if let Some(watermark) = &self.captured_watermark
            && watermark.shard_id != self.shard_id
        {
            return Err(DomainError::UnknownReference {
                field: "remote coverage watermark shard",
            });
        }
        let cache_field_count = [
            self.cache_generation.is_some(),
            self.cache_not_after.is_some(),
            self.cache_age_micros.is_some(),
            self.cache_grant_snapshot.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if cache_field_count != 0 && cache_field_count != 4 {
            return Err(DomainError::UnknownReference {
                field: "remote coverage cache state",
            });
        }
        if let Some(cache_grant_snapshot) = &self.cache_grant_snapshot {
            cache_grant_snapshot.validate()?;
        }
        Ok(())
    }

    fn has_fresh_cache_at(
        &self,
        evaluated_at: UtcMicros,
        placement_version: &EntityVersionId,
    ) -> bool {
        self.cache_generation.is_some()
            && self.cache_age_micros.is_some()
            && self.cache_not_after.is_some_and(|cache_not_after| {
                self.cache_grant_snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.proves_current_access(
                        evaluated_at,
                        cache_not_after,
                        placement_version,
                        &self.authority_id,
                        self.authority_epoch,
                    )
                })
            })
    }

    fn is_authoritatively_complete(&self) -> bool {
        self.served_by_role == BrainNodeRoleV1::Authority
            && self.captured_watermark.is_some()
            && self.pending_local_observations == 0
            && self.pending_tombstone_acks == 0
    }
}

/// A vector whose length cannot exceed `MAX`.
///
/// Deserialization consumes at most `MAX` values into memory. If another value
/// is present, it is ignored and the sequence is rejected immediately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedVec<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> BoundedVec<T, MAX> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.0.iter_mut()
    }
}

impl<T, const MAX: usize> TryFrom<Vec<T>> for BoundedVec<T, MAX> {
    type Error = Vec<T>;

    fn try_from(values: Vec<T>) -> Result<Self, Self::Error> {
        if values.len() <= MAX {
            Ok(Self(values))
        } else {
            Err(values)
        }
    }
}

impl<T, const MAX: usize> Index<usize> for BoundedVec<T, MAX> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl<T, const MAX: usize> IndexMut<usize> for BoundedVec<T, MAX> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

impl<'a, T, const MAX: usize> IntoIterator for &'a BoundedVec<T, MAX> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, T, const MAX: usize> IntoIterator for &'a mut BoundedVec<T, MAX> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<T, const MAX: usize> IntoIterator for BoundedVec<T, MAX> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<T, const MAX: usize> Serialize for BoundedVec<T, MAX>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

struct BoundedVecVisitor<T, const MAX: usize>(PhantomData<T>);

impl<'de, T, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX>
where
    T: Deserialize<'de>,
{
    type Value = BoundedVec<T, MAX>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a sequence with at most {MAX} elements")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
        while values.len() < MAX {
            match sequence.next_element()? {
                Some(value) => values.push(value),
                None => return Ok(BoundedVec(values)),
            }
        }
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::invalid_length(
                MAX.saturating_add(1),
                &self,
            ));
        }
        Ok(BoundedVec(values))
    }
}

impl<'de, T, const MAX: usize> Deserialize<'de> for BoundedVec<T, MAX>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedVecVisitor(PhantomData))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteCoverageV1 {
    pub brain_id: BrainId,
    pub placement_version: EntityVersionId,
    /// Immutable instant at which this coverage decision was evaluated.
    pub evaluated_at: UtcMicros,
    pub requested_consistency: ReadConsistencyV1,
    pub shards: BoundedVec<RemoteShardCoverageV1, 1_024>,
}

impl RemoteCoverageV1 {
    fn validate(&self) -> Result<(), DomainError> {
        self.brain_id.validate()?;
        self.placement_version.validate()?;
        ensure_unique(
            self.shards.iter().map(|shard| &shard.shard_id),
            "remote coverage shards",
        )?;
        for shard in &self.shards {
            shard.validate()?;
        }
        Ok(())
    }

    fn is_complete_for_requested_consistency(&self) -> bool {
        match self.requested_consistency {
            ReadConsistencyV1::Authoritative => self
                .shards
                .iter()
                .all(RemoteShardCoverageV1::is_authoritatively_complete),
            ReadConsistencyV1::BoundedStale { max_lag_micros } => self.shards.iter().all(|shard| {
                shard.pending_tombstone_acks == 0
                    && shard
                        .sync_lag_micros
                        .is_some_and(|lag| lag <= max_lag_micros)
            }),
            ReadConsistencyV1::OfflineCache => self.shards.iter().all(|shard| {
                shard.has_fresh_cache_at(self.evaluated_at, &self.placement_version)
                    && shard.pending_tombstone_acks == 0
            }),
        }
    }
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
    pub remote: Option<RemoteCoverageV1>,
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
        if let Some(remote) = &self.remote {
            remote.validate()?;
            if remote
                .shards
                .iter()
                .any(|shard| !self.dispositions.contains_key(&shard.shard_id))
            {
                return Err(DomainError::UnknownReference {
                    field: "remote coverage disposition shard",
                });
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
        self.remote
            .as_ref()
            .is_none_or(RemoteCoverageV1::is_complete_for_requested_consistency)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<RemoteCoverageV1>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<&'a RemoteCoverageV1>,
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
            remote: self.remote.as_ref(),
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
            remote: wire.remote,
        };
        report.validate().map_err(serde::de::Error::custom)?;
        Ok(report)
    }
}
