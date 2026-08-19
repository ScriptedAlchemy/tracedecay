use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::id::ShardId;

/// Per-shard progress without a fabricated global sequence.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorWatermark {
    pub components: BTreeMap<ShardId, u64>,
}

impl VectorWatermark {
    pub fn dominates(&self, other: &Self) -> bool {
        other
            .components
            .iter()
            .all(|(shard, sequence)| self.components.get(shard).copied().unwrap_or(0) >= *sequence)
    }

    pub fn partial_cmp_components(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_dominates = self.dominates(other);
        let other_dominates = other.dominates(self);
        match (self_dominates, other_dominates) {
            (true, true) => Some(std::cmp::Ordering::Equal),
            (true, false) => Some(std::cmp::Ordering::Greater),
            (false, true) => Some(std::cmp::Ordering::Less),
            (false, false) => None,
        }
    }

    pub fn merge_max(&self, other: &Self) -> Self {
        let mut components = self.components.clone();
        for (shard, sequence) in &other.components {
            components
                .entry(shard.clone())
                .and_modify(|current| *current = (*current).max(*sequence))
                .or_insert(*sequence);
        }
        Self { components }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ShardWatermark {
    pub shard_id: ShardId,
    pub outbox_sequence: u64,
}
