use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracedecay_domain::UtcMicros;

use super::{
    SnapshotLeaseIdV1, StorageRuntimeContractErrorV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreShardIdV1, StoreSnapshotIdV1,
};

/// Per-incarnation sequence assigned only after a successful commit.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct CommitSequenceV1(pub u64);

/// Complete storage commit position for one canonical logical shard history.
///
/// This is intentionally distinct from `tracedecay_domain::ShardWatermark`,
/// whose sequence is an outbox frontier and does not carry an incarnation or
/// writer fence. Converting between them would be lossy, so no alias or `From`
/// implementation is provided.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShardWatermarkV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
    pub commit_sequence: CommitSequenceV1,
}

impl ShardWatermarkV1 {
    pub fn same_history_as(&self, other: &Self) -> bool {
        self.shard_id == other.shard_id
            && self.incarnation == other.incarnation
            && self.authority_epoch == other.authority_epoch
    }

    pub fn satisfies(&self, required: &Self) -> bool {
        self.same_history_as(required) && self.commit_sequence >= required.commit_sequence
    }
}

/// Immutable cross-store target. It is a canonical storage commit vector, not
/// `tracedecay_domain::VectorWatermark` and not a distributed transaction. Its
/// JSON representation is a sorted array of fully fenced watermarks rather
/// than a JSON map keyed by a structured shard identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenWatermarkVectorV1(BTreeMap<StoreShardIdV1, ShardWatermarkV1>);

impl FrozenWatermarkVectorV1 {
    pub fn new(
        watermarks: impl IntoIterator<Item = ShardWatermarkV1>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let mut by_shard = BTreeMap::new();
        for watermark in watermarks {
            let shard_id = watermark.shard_id.clone();
            if by_shard.insert(shard_id, watermark).is_some() {
                return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                    field: "duplicate watermark",
                });
            }
        }
        if by_shard.is_empty() {
            return Err(StorageRuntimeContractErrorV1::EmptyWatermarkVector);
        }
        Ok(Self(by_shard))
    }

    pub fn get(&self, shard_id: &StoreShardIdV1) -> Option<&ShardWatermarkV1> {
        self.0.get(shard_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&StoreShardIdV1, &ShardWatermarkV1)> {
        self.0.iter()
    }
}

impl Serialize for FrozenWatermarkVectorV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.values().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FrozenWatermarkVectorV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Vec::<ShardWatermarkV1>::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl TryFrom<Vec<ShardWatermarkV1>> for FrozenWatermarkVectorV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: Vec<ShardWatermarkV1>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<FrozenWatermarkVectorV1> for Vec<ShardWatermarkV1> {
    fn from(value: FrozenWatermarkVectorV1) -> Self {
        value.0.into_values().collect()
    }
}

/// Retained exact snapshot and its bounded lease.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SnapshotLeaseV1 {
    pub lease_id: SnapshotLeaseIdV1,
    pub snapshot_id: StoreSnapshotIdV1,
    pub watermark: ShardWatermarkV1,
    pub acquired_at: UtcMicros,
    pub expires_at: UtcMicros,
}

impl SnapshotLeaseV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.expires_at <= self.acquired_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "snapshot lease",
            });
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now: UtcMicros) -> bool {
        now >= self.expires_at
    }
}

impl<'de> Deserialize<'de> for SnapshotLeaseV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            lease_id: SnapshotLeaseIdV1,
            snapshot_id: StoreSnapshotIdV1,
            watermark: ShardWatermarkV1,
            acquired_at: UtcMicros,
            expires_at: UtcMicros,
        }

        let wire = Wire::deserialize(deserializer)?;
        let lease = Self {
            lease_id: wire.lease_id,
            snapshot_id: wire.snapshot_id,
            watermark: wire.watermark,
            acquired_at: wire.acquired_at,
            expires_at: wire.expires_at,
        };
        lease.validate().map_err(serde::de::Error::custom)?;
        Ok(lease)
    }
}

/// Read guarantee requested from a runtime. All waiting remains bounded by the caller.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsistencyModeV1 {
    LatestAvailable,
    AtLeast { commit_sequence: CommitSequenceV1 },
    ExactSnapshot { lease: Box<SnapshotLeaseV1> },
    FrozenWatermarkVector { vector: FrozenWatermarkVectorV1 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkCoverageStatusV1 {
    Satisfied,
    Stale,
    Unavailable,
}

/// Observations against a frozen vector. Status is always derived, never supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenWatermarkCoverageV1 {
    pub required: FrozenWatermarkVectorV1,
    observed: BTreeMap<StoreShardIdV1, ShardWatermarkV1>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenWatermarkCoverageWireV1 {
    required: FrozenWatermarkVectorV1,
    observed: Vec<ShardWatermarkV1>,
}

impl FrozenWatermarkCoverageV1 {
    pub fn new(
        required: FrozenWatermarkVectorV1,
        observed: impl IntoIterator<Item = ShardWatermarkV1>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let mut observed_by_shard = BTreeMap::new();
        for watermark in observed {
            let shard_id = watermark.shard_id.clone();
            if required.get(&shard_id).is_none() {
                return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                    field: "observed watermark not required",
                });
            }
            if observed_by_shard.insert(shard_id, watermark).is_some() {
                return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                    field: "duplicate observed watermark",
                });
            }
        }
        Ok(Self {
            required,
            observed: observed_by_shard,
        })
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        Self::new(self.required.clone(), self.observed.values().cloned()).map(|_| ())
    }

    pub fn observed(&self, shard_id: &StoreShardIdV1) -> Option<&ShardWatermarkV1> {
        self.observed.get(shard_id)
    }

    pub fn status_for(&self, shard_id: &StoreShardIdV1) -> WatermarkCoverageStatusV1 {
        let Some(required) = self.required.get(shard_id) else {
            return WatermarkCoverageStatusV1::Unavailable;
        };
        match self.observed.get(shard_id) {
            Some(observed) if !observed.same_history_as(required) => {
                WatermarkCoverageStatusV1::Unavailable
            }
            Some(observed) if observed.satisfies(required) => WatermarkCoverageStatusV1::Satisfied,
            Some(_) => WatermarkCoverageStatusV1::Stale,
            None => WatermarkCoverageStatusV1::Unavailable,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.required
            .iter()
            .all(|(shard_id, _)| self.status_for(shard_id) == WatermarkCoverageStatusV1::Satisfied)
    }

    pub fn is_partial(&self) -> bool {
        !self.is_complete()
            && self.required.iter().any(|(shard_id, _)| {
                self.status_for(shard_id) == WatermarkCoverageStatusV1::Satisfied
            })
    }
}

impl Serialize for FrozenWatermarkCoverageV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        FrozenWatermarkCoverageWireV1 {
            required: self.required.clone(),
            observed: self.observed.values().cloned().collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FrozenWatermarkCoverageV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FrozenWatermarkCoverageWireV1::deserialize(deserializer)?;
        Self::new(wire.required, wire.observed).map_err(serde::de::Error::custom)
    }
}
