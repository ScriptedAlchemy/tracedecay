use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::watch;
use tracedecay_store::{
    CommitSequenceV1, ShardWatermarkV1, StoreCommitReceiptV1, StoreRuntimeBindingV1,
    StoreShardIdV1, UnavailableReasonV1,
};

use crate::read_consistency::{CommitWatermarkSource, WatermarkSourceState};

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
                    "committed watermark publisher has no channel for shard {shard_id:?}"
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

struct Channels {
    by_shard: BTreeMap<StoreShardIdV1, watch::Sender<ShardWatermarkV1>>,
}

/// The small capability a writer calls only after its transaction commits.
///
/// Publication is strictly monotonic and fenced to the bindings supplied at
/// construction. Keeping this capability distinct from the subscription makes
/// it impossible for readers or telemetry to advance commit truth.
pub struct CommittedWatermarkPublisher {
    channels: Arc<Channels>,
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
                .insert(shard_id.clone(), watch::channel(watermark).0)
                .is_some()
            {
                return Err(CommitWatermarkPublicationError::DuplicateShard(Box::new(
                    shard_id,
                )));
            }
        }
        Ok(Self {
            channels: Arc::new(Channels { by_shard }),
        })
    }

    pub fn subscribe(&self) -> CommitWatermarkSubscription {
        CommitWatermarkSubscription {
            channels: Arc::clone(&self.channels),
        }
    }

    pub(crate) fn current(&self, shard_id: &StoreShardIdV1) -> Option<ShardWatermarkV1> {
        self.channels
            .by_shard
            .get(shard_id)
            .map(|sender| sender.borrow().clone())
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
        let Some(sender) = self.channels.by_shard.get(&watermark.shard_id) else {
            return Err(CommitWatermarkPublicationError::MissingShard(Box::new(
                watermark.shard_id,
            )));
        };

        let mut outcome = Ok(());
        sender.send_if_modified(|current| {
            if current.incarnation != watermark.incarnation {
                outcome = Err(CommitWatermarkPublicationError::WrongIncarnation(Box::new(
                    watermark.shard_id.clone(),
                )));
                return false;
            }
            if current.authority_epoch != watermark.authority_epoch {
                outcome = Err(CommitWatermarkPublicationError::WrongAuthorityEpoch(
                    Box::new(watermark.shard_id.clone()),
                ));
                return false;
            }
            if watermark.commit_sequence <= current.commit_sequence {
                outcome = Err(CommitWatermarkPublicationError::NonMonotonic {
                    shard_id: Box::new(watermark.shard_id.clone()),
                    current: current.commit_sequence,
                    attempted: watermark.commit_sequence,
                });
                return false;
            }
            *current = watermark;
            true
        });
        outcome
    }
}

/// Read-only view over committed writer notifications.
#[derive(Clone)]
pub struct CommitWatermarkSubscription {
    channels: Arc<Channels>,
}

impl CommitWatermarkSource for CommitWatermarkSubscription {
    fn current(&self, shard_id: &StoreShardIdV1) -> WatermarkSourceState {
        self.channels
            .by_shard
            .get(shard_id)
            .map(|sender| WatermarkSourceState::Available(sender.borrow().clone()))
            .unwrap_or(WatermarkSourceState::Unavailable(
                UnavailableReasonV1::MissingAuthority,
            ))
    }

    fn wait_for_change<'a>(
        &'a self,
        shard_id: &'a StoreShardIdV1,
        after: &'a ShardWatermarkV1,
    ) -> Pin<Box<dyn Future<Output = WatermarkSourceState> + Send + 'a>> {
        let receiver = self
            .channels
            .by_shard
            .get(shard_id)
            .map(watch::Sender::subscribe);
        Box::pin(async move {
            let Some(mut receiver) = receiver else {
                return WatermarkSourceState::Unavailable(UnavailableReasonV1::MissingAuthority);
            };
            loop {
                let current = receiver.borrow_and_update().clone();
                if !current.same_history_as(after)
                    || current.commit_sequence > after.commit_sequence
                {
                    return WatermarkSourceState::Available(current);
                }
                if receiver.changed().await.is_err() {
                    return WatermarkSourceState::Unavailable(
                        UnavailableReasonV1::MissingAuthority,
                    );
                }
            }
        })
    }
}
