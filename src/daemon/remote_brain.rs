//! Daemon-owned remote Brain recovery composition.
//!
//! The coordinator performs no transport discovery and exposes no database
//! locator. Promotion is explicit preview/confirmation/CAS, then installs the
//! higher epoch at every durable sink before enabling service.

use std::collections::BTreeMap;

use tracedecay_store::{
    AuthorityCasV1, PromotionConfirmationV1, PromotionPreviewV1, PromotionReceiptV1,
    PromotionRecoveryStateV1, RemoteRecoveryContractErrorV1, ShardWatermarkV1,
    StoreRuntimeBindingV1,
};

pub trait RemoteAuthorityStoreV1 {
    fn compare_and_swap(
        &self,
        cas: &AuthorityCasV1,
    ) -> Result<AuthorityCasCommitV1, RemoteBrainRecoveryErrorV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityCasCommitV1 {
    pub previous_binding: StoreRuntimeBindingV1,
    pub installed_binding: StoreRuntimeBindingV1,
    pub installed_placement_revision: u64,
}

pub trait DurableFenceSinkV1 {
    fn sink_id(&self) -> &str;
    fn install_fence(
        &self,
        binding: &StoreRuntimeBindingV1,
        placement_revision: u64,
    ) -> Result<(), String>;
}

pub trait OldAuthorityFenceV1 {
    fn fence_read_only(
        &self,
        old_binding: &StoreRuntimeBindingV1,
        replacement_binding: &StoreRuntimeBindingV1,
    ) -> Result<(), String>;
}

pub trait StandbyFrontierV1 {
    fn durable_frontier(&self, binding: &StoreRuntimeBindingV1)
    -> Result<ShardWatermarkV1, String>;
}

pub trait PromotionPublisherV1 {
    fn publish_and_enable_serving(
        &self,
        binding: &StoreRuntimeBindingV1,
        placement_revision: u64,
        frontier: &ShardWatermarkV1,
    ) -> Result<String, String>;
}

pub struct RemotePromotionCoordinatorV1<'a> {
    pub authority_store: &'a dyn RemoteAuthorityStoreV1,
    pub sinks: &'a [&'a dyn DurableFenceSinkV1],
    pub old_authority: &'a dyn OldAuthorityFenceV1,
    pub standby: &'a dyn StandbyFrontierV1,
    pub publisher: &'a dyn PromotionPublisherV1,
}

impl RemotePromotionCoordinatorV1<'_> {
    pub fn promote(
        &self,
        preview: &PromotionPreviewV1,
        confirmation: &PromotionConfirmationV1,
        receipt_id: String,
    ) -> Result<PromotionReceiptV1, RemoteBrainRecoveryErrorV1> {
        preview
            .validate()
            .map_err(RemoteBrainRecoveryErrorV1::Contract)?;
        validate_confirmation(preview, confirmation)?;
        validate_sink_inventory(preview, self.sinks)?;

        let observed = self
            .standby
            .durable_frontier(&preview.cas.expected_binding)
            .map_err(RemoteBrainRecoveryErrorV1::StandbyUnavailable)?;
        if !observed.satisfies(&preview.required_frontier) {
            return Err(RemoteBrainRecoveryErrorV1::StandbyFrontierInsufficient);
        }

        let committed = self.authority_store.compare_and_swap(&preview.cas)?;
        if committed.previous_binding != preview.cas.expected_binding
            || committed.installed_binding != preview.cas.replacement_binding
            || committed.installed_placement_revision != preview.cas.replacement_placement_revision
        {
            return Err(RemoteBrainRecoveryErrorV1::ForwardRecoveryRequired {
                installed_sinks: Vec::new(),
                missing_sinks: preview.required_sink_ids.clone(),
                reason: "authority_cas_receipt_mismatch".to_owned(),
            });
        }

        let mut installed = BTreeMap::new();
        for sink in self.sinks {
            if let Err(reason) = sink.install_fence(
                &committed.installed_binding,
                committed.installed_placement_revision,
            ) {
                return Err(forward_recovery(
                    preview,
                    installed.keys().cloned().collect(),
                    reason,
                ));
            }
            installed.insert(
                sink.sink_id().to_owned(),
                committed.installed_binding.authority_epoch.get(),
            );
        }

        if let Err(reason) = self
            .old_authority
            .fence_read_only(&committed.previous_binding, &committed.installed_binding)
        {
            return Err(forward_recovery(
                preview,
                installed.keys().cloned().collect(),
                reason,
            ));
        }

        let replacement_frontier = ShardWatermarkV1 {
            shard_id: observed.shard_id,
            incarnation: observed.incarnation,
            authority_epoch: committed.installed_binding.authority_epoch,
            commit_sequence: observed.commit_sequence,
        };
        self.publisher
            .publish_and_enable_serving(
                &committed.installed_binding,
                committed.installed_placement_revision,
                &replacement_frontier,
            )
            .map_err(|reason| {
                forward_recovery(preview, installed.keys().cloned().collect(), reason)
            })?;

        let receipt = PromotionReceiptV1 {
            receipt_id,
            preview_id: preview.preview_id.clone(),
            replacement_binding: committed.installed_binding,
            replacement_placement_revision: committed.installed_placement_revision,
            installed_sink_epochs: installed,
            published_frontier: replacement_frontier,
            old_authority_read_only: true,
            state: PromotionRecoveryStateV1::Serving,
        };
        receipt
            .validate_against(preview)
            .map_err(RemoteBrainRecoveryErrorV1::Contract)?;
        Ok(receipt)
    }
}

fn validate_confirmation(
    preview: &PromotionPreviewV1,
    confirmation: &PromotionConfirmationV1,
) -> Result<(), RemoteBrainRecoveryErrorV1> {
    if confirmation.preview_id != preview.preview_id
        || confirmation.expected_authority_epoch
            != preview.cas.expected_binding.authority_epoch.get()
        || confirmation.expected_placement_revision != preview.cas.expected_placement_revision
    {
        return Err(RemoteBrainRecoveryErrorV1::StaleConfirmation);
    }
    Ok(())
}

fn validate_sink_inventory(
    preview: &PromotionPreviewV1,
    sinks: &[&dyn DurableFenceSinkV1],
) -> Result<(), RemoteBrainRecoveryErrorV1> {
    let mut actual = sinks
        .iter()
        .map(|sink| sink.sink_id().to_owned())
        .collect::<Vec<_>>();
    actual.sort();
    actual.dedup();
    let mut required = preview.required_sink_ids.clone();
    required.sort();
    required.dedup();
    if actual != required {
        return Err(RemoteBrainRecoveryErrorV1::SinkInventoryMismatch);
    }
    Ok(())
}

fn forward_recovery(
    preview: &PromotionPreviewV1,
    installed_sinks: Vec<String>,
    reason: String,
) -> RemoteBrainRecoveryErrorV1 {
    let missing_sinks = preview
        .required_sink_ids
        .iter()
        .filter(|sink| !installed_sinks.contains(sink))
        .cloned()
        .collect();
    RemoteBrainRecoveryErrorV1::ForwardRecoveryRequired {
        installed_sinks,
        missing_sinks,
        reason,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RejoinDispositionV1 {
    CurrentAuthority,
    FencedReadOnly {
        current_binding: StoreRuntimeBindingV1,
    },
    ReseedRequired {
        current_binding: StoreRuntimeBindingV1,
    },
}

pub fn classify_rejoin(
    local: &StoreRuntimeBindingV1,
    current: Option<&StoreRuntimeBindingV1>,
    reseed_complete: bool,
) -> RejoinDispositionV1 {
    let Some(current) = current else {
        return RejoinDispositionV1::ReseedRequired {
            current_binding: local.clone(),
        };
    };
    if local == current {
        return RejoinDispositionV1::CurrentAuthority;
    }
    if local.shard_id == current.shard_id
        && local.incarnation == current.incarnation
        && local.authority_epoch < current.authority_epoch
    {
        return if reseed_complete {
            RejoinDispositionV1::FencedReadOnly {
                current_binding: current.clone(),
            }
        } else {
            RejoinDispositionV1::ReseedRequired {
                current_binding: current.clone(),
            }
        };
    }
    RejoinDispositionV1::ReseedRequired {
        current_binding: current.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteBrainRecoveryErrorV1 {
    Contract(RemoteRecoveryContractErrorV1),
    StaleConfirmation,
    AuthorityUnavailable,
    AuthorityCasConflict,
    SinkInventoryMismatch,
    StandbyUnavailable(String),
    StandbyFrontierInsufficient,
    ForwardRecoveryRequired {
        installed_sinks: Vec<String>,
        missing_sinks: Vec<String>,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tracedecay_domain::{BrainId, UserProfileId};
    use tracedecay_store::{
        CommitSequenceV1, StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn binding(epoch: u64) -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::profile(
                id::<BrainId>("brain.remote"),
                id::<UserProfileId>("profile.remote"),
            ),
            StoreIncarnationV1::new(3).unwrap(),
            StoreAuthorityEpochV1::new(epoch).unwrap(),
        )
    }

    fn watermark(epoch: u64, sequence: u64) -> ShardWatermarkV1 {
        let binding = binding(epoch);
        ShardWatermarkV1 {
            shard_id: binding.shard_id,
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            commit_sequence: CommitSequenceV1(sequence),
        }
    }

    struct Authority;
    impl RemoteAuthorityStoreV1 for Authority {
        fn compare_and_swap(
            &self,
            cas: &AuthorityCasV1,
        ) -> Result<AuthorityCasCommitV1, RemoteBrainRecoveryErrorV1> {
            Ok(AuthorityCasCommitV1 {
                previous_binding: cas.expected_binding.clone(),
                installed_binding: cas.replacement_binding.clone(),
                installed_placement_revision: cas.replacement_placement_revision,
            })
        }
    }

    struct Sink {
        installed: Cell<bool>,
    }
    impl DurableFenceSinkV1 for Sink {
        fn sink_id(&self) -> &str {
            "writer"
        }
        fn install_fence(
            &self,
            _binding: &StoreRuntimeBindingV1,
            _placement_revision: u64,
        ) -> Result<(), String> {
            self.installed.set(true);
            Ok(())
        }
    }

    struct Standby;
    impl StandbyFrontierV1 for Standby {
        fn durable_frontier(
            &self,
            _binding: &StoreRuntimeBindingV1,
        ) -> Result<ShardWatermarkV1, String> {
            Ok(watermark(8, 12))
        }
    }

    struct Fence;
    impl OldAuthorityFenceV1 for Fence {
        fn fence_read_only(
            &self,
            _old_binding: &StoreRuntimeBindingV1,
            _replacement_binding: &StoreRuntimeBindingV1,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    struct Publisher<'a>(&'a Cell<bool>);
    impl PromotionPublisherV1 for Publisher<'_> {
        fn publish_and_enable_serving(
            &self,
            _binding: &StoreRuntimeBindingV1,
            _placement_revision: u64,
            _frontier: &ShardWatermarkV1,
        ) -> Result<String, String> {
            assert!(self.0.get(), "sink must be fenced before serving");
            Ok("publication.1".into())
        }
    }

    fn preview() -> PromotionPreviewV1 {
        PromotionPreviewV1 {
            preview_id: "promotion.1".into(),
            cas: AuthorityCasV1 {
                shard_id: binding(8).shard_id,
                expected_binding: binding(8),
                replacement_binding: binding(9),
                expected_placement_revision: 4,
                replacement_placement_revision: 5,
            },
            required_frontier: watermark(8, 10),
            required_sink_ids: vec!["writer".into()],
        }
    }

    #[test]
    fn every_sink_is_installed_before_serving() {
        let sink = Sink {
            installed: Cell::new(false),
        };
        let coordinator = RemotePromotionCoordinatorV1 {
            authority_store: &Authority,
            sinks: &[&sink],
            old_authority: &Fence,
            standby: &Standby,
            publisher: &Publisher(&sink.installed),
        };
        let receipt = coordinator
            .promote(
                &preview(),
                &PromotionConfirmationV1 {
                    preview_id: "promotion.1".into(),
                    expected_authority_epoch: 8,
                    expected_placement_revision: 4,
                },
                "receipt.1".into(),
            )
            .unwrap();
        assert!(sink.installed.get());
        assert!(matches!(receipt.state, PromotionRecoveryStateV1::Serving));
    }

    #[test]
    fn old_authority_rejoin_requires_explicit_reseed() {
        assert!(matches!(
            classify_rejoin(&binding(8), Some(&binding(9)), false),
            RejoinDispositionV1::ReseedRequired { .. }
        ));
        assert!(matches!(
            classify_rejoin(&binding(8), Some(&binding(9)), true),
            RejoinDispositionV1::FencedReadOnly { .. }
        ));
    }
}
