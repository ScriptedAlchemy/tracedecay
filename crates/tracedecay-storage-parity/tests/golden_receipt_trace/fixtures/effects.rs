//! Transactional outbox and inbox effects for the ledger stream.

use serde_json::{Value as Json, json};
use tracedecay_store::{StoreRuntimeBindingV1, TransactionalInboxReceiptV1, TransactionalOutboxEntryV1};

use super::super::harness::digest;

fn sessions_shard(binding: &StoreRuntimeBindingV1) -> Json {
    let mut shard = serde_json::to_value(&binding.shard_id).expect("shard json");
    shard["scope"] = json!({ "kind": "project_sessions", "project_id": "project.parity" });
    shard
}

fn effect_identity(binding: &StoreRuntimeBindingV1, effect_id: &str) -> Json {
    json!({
        "effect_id": effect_id,
        "command_digest": digest('e'),
        "ordering_key": "parity.ordering",
        "source_watermark": {
            "shard_id": binding.shard_id,
            "incarnation": binding.incarnation,
            "authority_epoch": binding.authority_epoch,
            "commit_sequence": 0
        },
        "target_watermark": {
            "shard_id": sessions_shard(binding),
            "incarnation": binding.incarnation,
            "authority_epoch": binding.authority_epoch,
            "commit_sequence": 0
        }
    })
}

/// An outbox effect sourced at `binding` and targeting its sessions shard.
pub fn outbox_entry(
    binding: &StoreRuntimeBindingV1,
    effect_id: &str,
    state: &str,
) -> TransactionalOutboxEntryV1 {
    serde_json::from_value(json!({
        "identity": effect_identity(binding, effect_id),
        "effect": "publish_observation",
        "state": state,
        "acknowledgement": null,
        "enqueued_at": 1_000_000,
        "updated_at": 1_000_000
    }))
    .expect("valid outbox entry")
}

/// The target's receipt for `effect_id`, acknowledging it at the source.
pub fn inbox_receipt(binding: &StoreRuntimeBindingV1, effect_id: &str) -> TransactionalInboxReceiptV1 {
    serde_json::from_value(json!({
        "identity": effect_identity(binding, effect_id),
        "disposition": "applied",
        "target_commit_watermark": {
            "shard_id": sessions_shard(binding),
            "incarnation": binding.incarnation,
            "authority_epoch": binding.authority_epoch,
            "commit_sequence": 1
        },
        "committed_at": 1_000_000
    }))
    .expect("valid inbox receipt")
}
