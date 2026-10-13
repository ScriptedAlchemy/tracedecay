use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use tracedecay_store::{
    CommitSequenceV1, ShardWatermarkV1, StoreCommitReceiptV1, StoreRuntimeBindingV1, StoreShardIdV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitWatermarkPublicationError {
    DuplicateShard(Box<StoreShardIdV1>),
    MissingShard(Box<StoreShardIdV1>),
    WrongIncarnation(Box<StoreShardIdV1>),
    WrongAuthorityEpoch(Box<StoreShardIdV1>),
    NonMonotonic {
        shard_id: Box<StoreShardIdV1>,
        current: CommitSequenceV1,
        attempted: CommitSequenceV1,
    },
}

impl fmt::Display for CommitWatermarkPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateShard(shard_id) => {
                write!(
                    formatter,
                    "committed watermark publisher already tracks shard {shard_id:?}"
                )
            }
            Self::MissingShard(shard_id) => {
                write!(
                    formatter,
                    "committed watermark publisher has no watermark for shard {shard_id:?}"
                )
            }
            Self::WrongIncarnation(shard_id) => {
                write!(
                    formatter,
                    "committed watermark publication used the wrong incarnation for shard {shard_id:?}"
                )
            }
            Self::WrongAuthorityEpoch(shard_id) => {
                write!(
                    formatter,
                    "committed watermark publication used the wrong authority epoch for shard {shard_id:?}"
                )
            }
            Self::NonMonotonic {
                shard_id,
                current,
                attempted,
            } => write!(
                formatter,
                "committed watermark publication for shard {shard_id:?} was non-monotonic: current {}, attempted {}",
                current.0, attempted.0
            ),
        }
    }
}

impl Error for CommitWatermarkPublicationError {}

/// The small capability a writer calls only after its transaction commits.
///
/// Publication is strictly monotonic and fenced to the bindings supplied at
/// construction.
pub struct CommittedWatermarkPublisher {
    watermarks: BTreeMap<StoreShardIdV1, Mutex<ShardWatermarkV1>>,
}

impl CommittedWatermarkPublisher {
    pub fn new(binding: StoreRuntimeBindingV1) -> Self {
        Self::from_bindings([binding]).expect("one binding cannot contain a duplicate shard")
    }

    pub fn from_bindings(
        bindings: impl IntoIterator<Item = StoreRuntimeBindingV1>,
    ) -> Result<Self, CommitWatermarkPublicationError> {
        Self::with_initial_watermarks(bindings.into_iter().map(|binding| ShardWatermarkV1 {
            shard_id: binding.shard_id,
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            commit_sequence: CommitSequenceV1(0),
        }))
    }

    pub fn with_initial_watermarks(
        watermarks: impl IntoIterator<Item = ShardWatermarkV1>,
    ) -> Result<Self, CommitWatermarkPublicationError> {
        let mut by_shard = BTreeMap::new();
        for watermark in watermarks {
            let shard_id = watermark.shard_id.clone();
            if by_shard
                .insert(shard_id.clone(), Mutex::new(watermark))
                .is_some()
            {
                return Err(CommitWatermarkPublicationError::DuplicateShard(Box::new(
                    shard_id,
                )));
            }
        }
        Ok(Self {
            watermarks: by_shard,
        })
    }

    #[cfg(test)]
    pub(crate) fn current(&self, shard_id: &StoreShardIdV1) -> Option<ShardWatermarkV1> {
        self.watermarks.get(shard_id).map(|watermark| {
            watermark
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
    }

    pub fn publish_committed(
        &self,
        receipt: &StoreCommitReceiptV1,
    ) -> Result<(), CommitWatermarkPublicationError> {
        self.publish_committed_watermark(ShardWatermarkV1 {
            shard_id: receipt.shard_id.clone(),
            incarnation: receipt.incarnation,
            authority_epoch: receipt.authority_epoch,
            commit_sequence: receipt.commit_sequence,
        })
    }

    pub(crate) fn publish_committed_watermark(
        &self,
        watermark: ShardWatermarkV1,
    ) -> Result<(), CommitWatermarkPublicationError> {
        let Some(watermark_cell) = self.watermarks.get(&watermark.shard_id) else {
            return Err(CommitWatermarkPublicationError::MissingShard(Box::new(
                watermark.shard_id,
            )));
        };

        let mut current = watermark_cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.incarnation != watermark.incarnation {
            return Err(CommitWatermarkPublicationError::WrongIncarnation(Box::new(
                watermark.shard_id,
            )));
        }
        if current.authority_epoch != watermark.authority_epoch {
            return Err(CommitWatermarkPublicationError::WrongAuthorityEpoch(
                Box::new(watermark.shard_id),
            ));
        }
        if watermark.commit_sequence <= current.commit_sequence {
            return Err(CommitWatermarkPublicationError::NonMonotonic {
                shard_id: Box::new(watermark.shard_id),
                current: current.commit_sequence,
                attempted: watermark.commit_sequence,
            });
        }
        *current = watermark;
        Ok(())
    }
}
