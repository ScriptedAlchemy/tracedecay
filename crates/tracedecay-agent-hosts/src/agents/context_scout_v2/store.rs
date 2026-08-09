use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use super::{
    ContextScoutAddressV1, ContextScoutDeliveryReceiptV1, ContextScoutDurableClaimOutcomeV1,
    ContextScoutDurableClaimV1, ContextScoutDurableQueueEntryV1,
    ContextScoutDurableStartupOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutDurableStoreV1, ContextScoutFeedbackV1, ContextScoutLeaseV1,
    ContextScoutRecentDeliveryV1, ContextScoutRecentReadOutcomeV1, ContextScoutRecentStateV1,
    ContextScoutStoreFuture, ContextScoutWorkV1, MAX_SCOUT_ACTIVE_ADDRESSES,
    MAX_SCOUT_RECENT_DELIVERIES, validate_context_scout_delivery_receipt,
    validate_context_scout_feedback, validate_receipt_shape,
};
use crate::db::Database;
use crate::db::engine::params;

const STORE_KEY_V1: &str = "agents.context-scout.durable.v1";
const MAX_STORED_STATE_BYTES_V1: usize = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredQueueEntryV1 {
    entry: ContextScoutDurableQueueEntryV1,
    lease: Option<ContextScoutLeaseV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredDeliveryAddressV1 {
    envelope_id: [u8; 16],
    address: ContextScoutAddressV1,
    #[serde(default)]
    entry: Option<ContextScoutDurableQueueEntryV1>,
    #[serde(default)]
    lease: Option<ContextScoutLeaseV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredContextScoutStateV1 {
    project_id: [u8; 16],
    entries: Vec<StoredQueueEntryV1>,
    tombstones: Vec<ContextScoutWorkV1>,
    receipts: Vec<ContextScoutDeliveryReceiptV1>,
    feedback: Vec<ContextScoutFeedbackV1>,
    #[serde(default)]
    delivery_addresses: Vec<StoredDeliveryAddressV1>,
    #[serde(default)]
    delivery_provenance_complete: bool,
}

impl StoredContextScoutStateV1 {
    fn new(project_id: [u8; 16]) -> Self {
        Self {
            project_id,
            entries: Vec::new(),
            tombstones: Vec::new(),
            receipts: Vec::new(),
            feedback: Vec::new(),
            delivery_addresses: Vec::new(),
            delivery_provenance_complete: true,
        }
    }

    fn validate(&self, project_id: [u8; 16]) -> bool {
        if self.project_id != project_id
            || self.entries.len() > MAX_SCOUT_ACTIVE_ADDRESSES
            || self.tombstones.len() > MAX_SCOUT_ACTIVE_ADDRESSES
            || self.receipts.len() > MAX_SCOUT_RECENT_DELIVERIES
            || self.feedback.len() > MAX_SCOUT_RECENT_DELIVERIES
            || self.delivery_addresses.len() > MAX_SCOUT_RECENT_DELIVERIES
        {
            return false;
        }

        let mut addresses = BTreeSet::new();
        let entries_valid = self.entries.iter().all(|stored| {
            stored.entry.validate().is_ok()
                && stored.entry.work.address.project_id == project_id
                && addresses.insert(stored.entry.work.address)
                && stored
                    .lease
                    .is_none_or(|lease| lease.lease_id != [0; 16] && lease.expires_at.0 > 0)
        });
        let mut tombstones = Vec::new();
        let tombstones_valid = self.tombstones.iter().all(|work| {
            if tombstones.contains(work) {
                return false;
            }
            tombstones.push(*work);
            work.address.validate().is_ok()
                && work.address.project_id == project_id
                && work.generation > 0
                && work.input_watermark != [0; 32]
        });
        let mut receipt_ids = BTreeSet::new();
        let mut envelope_ids = BTreeSet::new();
        let receipts_valid = self.receipts.iter().all(|receipt| {
            validate_receipt_shape(receipt).is_ok()
                && receipt_ids.insert(receipt.receipt_id)
                && envelope_ids.insert(receipt.envelope_id)
        });
        let feedback_valid = self.feedback.iter().all(|feedback| {
            self.receipts
                .iter()
                .find(|receipt| receipt.receipt_id == feedback.receipt_id)
                .is_some_and(|receipt| validate_context_scout_feedback(receipt, *feedback).is_ok())
        });
        let mut delivery_envelopes = BTreeSet::new();
        let delivery_addresses_valid = self.delivery_addresses.iter().all(|binding| {
            binding.address.project_id == project_id
                && binding.address.validate().is_ok()
                && delivery_envelopes.insert(binding.envelope_id)
                && binding.entry.as_ref().is_none_or(|entry| {
                    entry.validate().is_ok()
                        && entry.work.address == binding.address
                        && entry.envelope.envelope_id == binding.envelope_id
                })
                && binding
                    .lease
                    .is_none_or(|lease| lease.lease_id != [0; 16] && lease.expires_at.0 > 0)
                && self
                    .receipts
                    .iter()
                    .any(|receipt| receipt.envelope_id == binding.envelope_id)
        });
        let complete_delivery_provenance =
            !self.delivery_provenance_complete || self.has_complete_delivery_provenance();

        entries_valid
            && tombstones_valid
            && receipts_valid
            && feedback_valid
            && delivery_addresses_valid
            && complete_delivery_provenance
    }

    fn has_complete_delivery_provenance(&self) -> bool {
        self.delivery_addresses.len() == self.receipts.len()
            && self.delivery_addresses.iter().all(|binding| {
                binding.entry.is_some()
                    && self
                        .receipts
                        .iter()
                        .any(|receipt| receipt.envelope_id == binding.envelope_id)
            })
    }

    fn refresh_delivery_provenance(&mut self) {
        self.delivery_provenance_complete = self.has_complete_delivery_provenance();
    }

    fn recover_expired_claims(&mut self, now: UtcMicros) {
        for stored in &mut self.entries {
            if stored
                .lease
                .is_some_and(|lease| lease.expires_at.0 <= now.0)
            {
                stored.lease = None;
            }
        }
    }

    fn add_tombstone(&mut self, work: ContextScoutWorkV1) {
        if self.tombstones.contains(&work) {
            return;
        }
        if self.tombstones.len() == MAX_SCOUT_ACTIVE_ADDRESSES {
            self.tombstones.remove(0);
        }
        self.tombstones.push(work);
    }

    fn trim_receipts(&mut self) {
        while self.receipts.len() > MAX_SCOUT_RECENT_DELIVERIES {
            let Some((index, evicted)) = self
                .receipts
                .iter()
                .enumerate()
                .min_by_key(|(_, receipt)| receipt.delivered_at.0)
                .map(|(index, receipt)| (index, receipt.receipt_id))
            else {
                break;
            };
            self.receipts.remove(index);
            self.feedback
                .retain(|feedback| feedback.receipt_id != evicted);
            let retained_envelopes = self
                .receipts
                .iter()
                .map(|receipt| receipt.envelope_id)
                .collect::<BTreeSet<_>>();
            self.delivery_addresses
                .retain(|binding| retained_envelopes.contains(&binding.envelope_id));
        }
    }

    fn record_delivery(
        &mut self,
        claim: &ContextScoutDurableClaimV1,
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        if let Some(existing) = self
            .receipts
            .iter()
            .find(|existing| existing.receipt_id == receipt.receipt_id)
        {
            if existing != receipt {
                return ContextScoutDurableStoreOutcomeV1::Superseded;
            }
            if let Some(binding) = self
                .delivery_addresses
                .iter_mut()
                .find(|binding| binding.envelope_id == receipt.envelope_id)
            {
                if binding.address != claim.entry.work.address
                    || binding
                        .entry
                        .as_ref()
                        .is_some_and(|stored| stored != &claim.entry)
                    || binding.lease.is_some_and(|stored| stored != claim.lease)
                {
                    return ContextScoutDurableStoreOutcomeV1::Superseded;
                }
                if binding.entry.as_ref() == Some(&claim.entry)
                    && binding.lease == Some(claim.lease)
                {
                    return ContextScoutDurableStoreOutcomeV1::Duplicate;
                }
                binding.entry = Some(claim.entry.clone());
                binding.lease = Some(claim.lease);
            } else {
                self.delivery_addresses.push(StoredDeliveryAddressV1 {
                    envelope_id: claim.entry.envelope.envelope_id,
                    address: claim.entry.work.address,
                    entry: Some(claim.entry.clone()),
                    lease: Some(claim.lease),
                });
            }
            self.refresh_delivery_provenance();
            return ContextScoutDurableStoreOutcomeV1::Stored;
        }
        let Some(index) = self
            .entries
            .iter()
            .position(|stored| stored.entry == claim.entry && stored.lease == Some(claim.lease))
        else {
            return if self
                .entries
                .iter()
                .any(|stored| stored.entry.work.address == claim.entry.work.address)
            {
                ContextScoutDurableStoreOutcomeV1::Superseded
            } else {
                ContextScoutDurableStoreOutcomeV1::Unavailable
            };
        };
        self.entries.remove(index);
        self.add_tombstone(claim.entry.work);
        self.receipts.push(receipt.clone());
        self.delivery_addresses.push(StoredDeliveryAddressV1 {
            envelope_id: claim.entry.envelope.envelope_id,
            address: claim.entry.work.address,
            entry: Some(claim.entry.clone()),
            lease: Some(claim.lease),
        });
        self.trim_receipts();
        self.refresh_delivery_provenance();
        ContextScoutDurableStoreOutcomeV1::Stored
    }
}

/// Project-scoped Context Scout persistence backed by the already-open
/// project graph database. All mutations use its serialized immediate writer
/// transaction and one bounded metadata value; no database or policy authority
/// is created here.
#[derive(Clone)]
pub struct ProjectContextScoutDurableStoreV1 {
    database: Database,
    project_id: [u8; 16],
}

impl ProjectContextScoutDurableStoreV1 {
    /// Builds an owned store from the daemon's retained project database.
    pub fn from_project_database(database: Database, project_id: [u8; 16]) -> Option<Arc<Self>> {
        (project_id != [0; 16]).then(|| {
            Arc::new(Self {
                database,
                project_id,
            })
        })
    }

    /// Daemon startup convenience: construct the owned store, atomically
    /// requeue expired claims, and return a bounded ready page.
    pub async fn startup_from_project_database(
        database: Database,
        project_id: [u8; 16],
        now: UtcMicros,
        limit: usize,
    ) -> Option<(Arc<Self>, ContextScoutDurableStartupOutcomeV1)> {
        let store = Self::from_project_database(database, project_id)?;
        let outcome = store.startup(now, limit).await;
        Some((store, outcome))
    }

    fn in_scope(&self, address: ContextScoutAddressV1) -> bool {
        address.project_id == self.project_id && address.validate().is_ok()
    }

    async fn recent_matching(
        &self,
        configuration_revision: [u8; 32],
        observed_at: UtcMicros,
        limit: usize,
        matches: impl Fn(ContextScoutAddressV1) -> bool,
    ) -> ContextScoutRecentReadOutcomeV1 {
        if configuration_revision == [0; 32]
            || observed_at.0 <= 0
            || limit == 0
            || limit > MAX_SCOUT_RECENT_DELIVERIES
        {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        }
        let Some(encoded) = self
            .database
            .get_metadata(STORE_KEY_V1)
            .await
            .ok()
            .flatten()
        else {
            return ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
                configuration_revision,
                observed_at,
                pending: Vec::new(),
                deliveries: Vec::new(),
                omitted: 0,
            });
        };
        if encoded.len() > MAX_STORED_STATE_BYTES_V1 {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        }
        let Ok(state) = serde_json::from_str::<StoredContextScoutStateV1>(&encoded) else {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        };
        if !state.validate(self.project_id) {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        }
        if !state.has_complete_delivery_provenance() {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        }
        let mut pending = state
            .entries
            .iter()
            .filter(|stored| matches(stored.entry.work.address))
            .filter(|stored| {
                stored.entry.envelope.configuration_revision == configuration_revision
                    && stored.entry.envelope.candidate.expires_at.0 > observed_at.0
            })
            .map(|stored| stored.entry.clone())
            .collect::<Vec<_>>();
        pending.sort_by_key(|entry| std::cmp::Reverse(entry.work.generation));
        let mut deliveries = state
            .delivery_addresses
            .iter()
            .filter(|binding| matches(binding.address))
            .filter_map(|binding| {
                let entry = binding.entry.as_ref()?.clone();
                if entry.envelope.configuration_revision != configuration_revision {
                    return None;
                }
                let receipt = state
                    .receipts
                    .iter()
                    .find(|receipt| receipt.envelope_id == binding.envelope_id)?
                    .clone();
                let feedback = state
                    .feedback
                    .iter()
                    .rev()
                    .find(|feedback| feedback.receipt_id == receipt.receipt_id)
                    .copied();
                Some(ContextScoutRecentDeliveryV1 {
                    entry,
                    receipt,
                    feedback,
                })
            })
            .collect::<Vec<_>>();
        deliveries.sort_by_key(|delivery| std::cmp::Reverse(delivery.receipt.delivered_at));
        let total = pending.len().saturating_add(deliveries.len());
        pending.truncate(limit);
        deliveries.truncate(limit.saturating_sub(pending.len()));
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            configuration_revision,
            observed_at,
            omitted: total.saturating_sub(pending.len().saturating_add(deliveries.len())),
            pending,
            deliveries,
        })
    }

    pub(crate) async fn recent(
        &self,
        address: ContextScoutAddressV1,
        configuration_revision: [u8; 32],
        observed_at: UtcMicros,
        limit: usize,
    ) -> ContextScoutRecentReadOutcomeV1 {
        if !self.in_scope(address) {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        }
        self.recent_matching(configuration_revision, observed_at, limit, |candidate| {
            candidate == address
        })
        .await
    }

    pub(crate) async fn recent_for_protected_session(
        &self,
        protected_session_id: [u8; 32],
        configuration_revision: [u8; 32],
        observed_at: UtcMicros,
        limit: usize,
    ) -> ContextScoutRecentReadOutcomeV1 {
        if protected_session_id == [0; 32] {
            return ContextScoutRecentReadOutcomeV1::Unavailable;
        }
        self.recent_matching(configuration_revision, observed_at, limit, |candidate| {
            candidate.project_id == self.project_id
                && candidate.protected_session_id == protected_session_id
        })
        .await
    }

    pub(crate) async fn recent_project(
        &self,
        configuration_revision: [u8; 32],
        observed_at: UtcMicros,
        limit: usize,
    ) -> ContextScoutRecentReadOutcomeV1 {
        self.recent_matching(configuration_revision, observed_at, limit, |candidate| {
            candidate.project_id == self.project_id
        })
        .await
    }

    async fn update_state<T: Send>(
        &self,
        operation: &str,
        update: impl FnOnce(&mut StoredContextScoutStateV1) -> T + Send,
    ) -> Option<T> {
        let transaction = self
            .database
            .begin_write_transaction(operation)
            .await
            .ok()?;
        let mut rows = transaction
            .query_engine(
                "SELECT value FROM metadata WHERE key = ?1",
                params![STORE_KEY_V1],
            )
            .await
            .ok()?;
        let encoded = rows
            .next()
            .await
            .ok()?
            .map(|row| row.get::<String>(0))
            .transpose()
            .ok()?;
        drop(rows);

        let mut state = match encoded {
            Some(encoded) if encoded.len() <= MAX_STORED_STATE_BYTES_V1 => {
                serde_json::from_str::<StoredContextScoutStateV1>(&encoded).ok()?
            }
            Some(_) => return None,
            None => StoredContextScoutStateV1::new(self.project_id),
        };
        if !state.validate(self.project_id) {
            return None;
        }

        let original = state.clone();
        state.refresh_delivery_provenance();
        let outcome = update(&mut state);
        if !state.validate(self.project_id) {
            return None;
        }
        if state == original {
            transaction.rollback().await.ok()?;
            return Some(outcome);
        }
        let encoded = serde_json::to_string(&state).ok()?;
        if encoded.len() > MAX_STORED_STATE_BYTES_V1 {
            return None;
        }
        self.database
            .set_metadata_unguarded(&transaction, STORE_KEY_V1, &encoded)
            .await
            .ok()?;
        transaction.commit().await.ok()?;
        Some(outcome)
    }

    async fn startup_inner(
        &self,
        now: UtcMicros,
        limit: usize,
    ) -> ContextScoutDurableStartupOutcomeV1 {
        if now.0 <= 0 || limit == 0 || limit > MAX_SCOUT_ACTIVE_ADDRESSES {
            return ContextScoutDurableStartupOutcomeV1::Unavailable;
        }
        self.update_state("start Context Scout durable store", |state| {
            state.recover_expired_claims(now);
            let mut entries = state
                .entries
                .iter()
                .filter(|stored| stored.lease.is_none())
                .map(|stored| stored.entry.clone())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| (entry.work.address, entry.work.generation));
            let truncated = entries.len() > limit;
            entries.truncate(limit);
            ContextScoutDurableStartupOutcomeV1::Ready { entries, truncated }
        })
        .await
        .unwrap_or(ContextScoutDurableStartupOutcomeV1::Unavailable)
    }

    pub(crate) async fn work_snapshot(
        &self,
        now: UtcMicros,
        limit: usize,
    ) -> ContextScoutDurableStartupOutcomeV1 {
        if now.0 <= 0 || limit == 0 || limit > MAX_SCOUT_ACTIVE_ADDRESSES {
            return ContextScoutDurableStartupOutcomeV1::Unavailable;
        }
        self.update_state("restore Context Scout work generations", |state| {
            state.recover_expired_claims(now);
            let mut entries = state
                .entries
                .iter()
                .map(|stored| stored.entry.clone())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| (entry.work.address, entry.work.generation));
            let truncated = entries.len() > limit;
            entries.truncate(limit);
            ContextScoutDurableStartupOutcomeV1::Ready { entries, truncated }
        })
        .await
        .unwrap_or(ContextScoutDurableStartupOutcomeV1::Unavailable)
    }

    async fn enqueue_inner(
        &self,
        entry: ContextScoutDurableQueueEntryV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        if entry.validate().is_err() || !self.in_scope(entry.work.address) {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        self.update_state("enqueue Context Scout suggestion", move |state| {
            if state.tombstones.contains(&entry.work) {
                return ContextScoutDurableStoreOutcomeV1::Superseded;
            }
            if state
                .receipts
                .iter()
                .any(|receipt| receipt.envelope_id == entry.envelope.envelope_id)
            {
                return ContextScoutDurableStoreOutcomeV1::Duplicate;
            }
            if let Some(existing) = state
                .entries
                .iter()
                .find(|stored| stored.entry.envelope.envelope_id == entry.envelope.envelope_id)
            {
                return if existing.entry == entry {
                    ContextScoutDurableStoreOutcomeV1::Duplicate
                } else {
                    ContextScoutDurableStoreOutcomeV1::Superseded
                };
            }
            if let Some(index) = state
                .entries
                .iter()
                .position(|stored| stored.entry.work.address == entry.work.address)
            {
                let existing = &state.entries[index].entry;
                if existing == &entry {
                    return ContextScoutDurableStoreOutcomeV1::Duplicate;
                }
                if existing.work.generation >= entry.work.generation {
                    return ContextScoutDurableStoreOutcomeV1::Superseded;
                }
                let existing = state.entries.remove(index);
                state.add_tombstone(existing.entry.work);
            } else if state.entries.len() == MAX_SCOUT_ACTIVE_ADDRESSES {
                return ContextScoutDurableStoreOutcomeV1::Unavailable;
            }
            state
                .entries
                .push(StoredQueueEntryV1 { entry, lease: None });
            ContextScoutDurableStoreOutcomeV1::Stored
        })
        .await
        .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    async fn claim_inner(
        &self,
        address: ContextScoutAddressV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutDurableClaimOutcomeV1 {
        if !self.in_scope(address) || lease.validate(now).is_err() {
            return ContextScoutDurableClaimOutcomeV1::Unavailable;
        }
        self.update_state("claim Context Scout suggestion", move |state| {
            state.recover_expired_claims(now);
            let Some(stored) = state
                .entries
                .iter_mut()
                .find(|stored| stored.entry.work.address == address)
            else {
                return ContextScoutDurableClaimOutcomeV1::Empty;
            };
            match stored.lease {
                Some(existing) if existing == lease => {
                    ContextScoutDurableClaimOutcomeV1::Claimed(ContextScoutDurableClaimV1 {
                        entry: stored.entry.clone(),
                        lease,
                    })
                }
                Some(_) => ContextScoutDurableClaimOutcomeV1::Empty,
                None => {
                    stored.lease = Some(lease);
                    ContextScoutDurableClaimOutcomeV1::Claimed(ContextScoutDurableClaimV1 {
                        entry: stored.entry.clone(),
                        lease,
                    })
                }
            }
        })
        .await
        .unwrap_or(ContextScoutDurableClaimOutcomeV1::Unavailable)
    }

    async fn requeue_inner(
        &self,
        claimed: ContextScoutDurableClaimV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        if claimed.entry.validate().is_err()
            || !self.in_scope(claimed.entry.work.address)
            || claimed.lease.lease_id == [0; 16]
            || claimed.lease.expires_at.0 <= 0
        {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        self.update_state("requeue Context Scout suggestion", move |state| {
            let Some(stored) = state.entries.iter_mut().find(|stored| {
                stored.entry.envelope.envelope_id == claimed.entry.envelope.envelope_id
            }) else {
                return if state.tombstones.contains(&claimed.entry.work) {
                    ContextScoutDurableStoreOutcomeV1::Superseded
                } else {
                    ContextScoutDurableStoreOutcomeV1::Unavailable
                };
            };
            if stored.entry != claimed.entry {
                return ContextScoutDurableStoreOutcomeV1::Superseded;
            }
            match stored.lease {
                Some(lease) if lease == claimed.lease => {
                    stored.lease = None;
                    ContextScoutDurableStoreOutcomeV1::Stored
                }
                None => ContextScoutDurableStoreOutcomeV1::Duplicate,
                Some(_) => ContextScoutDurableStoreOutcomeV1::Superseded,
            }
        })
        .await
        .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    async fn cancel_inner(&self, work: ContextScoutWorkV1) -> ContextScoutDurableStoreOutcomeV1 {
        if work.generation == 0 || work.input_watermark == [0; 32] || !self.in_scope(work.address) {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        self.update_state("cancel Context Scout suggestion", move |state| {
            if state.tombstones.contains(&work) {
                return ContextScoutDurableStoreOutcomeV1::Duplicate;
            }
            let matching = state
                .entries
                .iter()
                .position(|stored| stored.entry.work == work);
            let newer_exists = matching.is_none()
                && state
                    .entries
                    .iter()
                    .any(|stored| stored.entry.work.address == work.address);
            if let Some(index) = matching {
                state.entries.remove(index);
            }
            state.add_tombstone(work);
            if newer_exists {
                ContextScoutDurableStoreOutcomeV1::Superseded
            } else {
                ContextScoutDurableStoreOutcomeV1::Stored
            }
        })
        .await
        .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    async fn record_delivery_inner(
        &self,
        claim: &ContextScoutDurableClaimV1,
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        if claim.entry.validate().is_err()
            || claim.lease.validate(receipt.delivered_at).is_err()
            || !self.in_scope(claim.entry.work.address)
            || validate_context_scout_delivery_receipt(&claim.entry.envelope, receipt).is_err()
        {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        let claim = claim.clone();
        let receipt = receipt.clone();
        self.update_state("record Context Scout delivery", move |state| {
            state.record_delivery(&claim, &receipt)
        })
        .await
        .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    async fn record_delivery_by_lease_inner(
        &self,
        work: ContextScoutWorkV1,
        envelope_id: [u8; 16],
        lease: ContextScoutLeaseV1,
        configuration_revision: [u8; 32],
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        if work.generation == 0
            || work.input_watermark == [0; 32]
            || !self.in_scope(work.address)
            || envelope_id == [0; 16]
            || configuration_revision == [0; 32]
            || receipt.envelope_id != envelope_id
            || validate_receipt_shape(receipt).is_err()
            || lease.validate(receipt.delivered_at).is_err()
        {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        let receipt = receipt.clone();
        self.update_state("record Context Scout delivery by lease", move |state| {
            let entry = state
                .entries
                .iter()
                .map(|stored| &stored.entry)
                .chain(
                    state
                        .delivery_addresses
                        .iter()
                        .filter(|binding| binding.lease == Some(lease))
                        .filter_map(|binding| binding.entry.as_ref()),
                )
                .find(|entry| {
                    entry.work == work
                        && entry.envelope.envelope_id == envelope_id
                        && entry.envelope.configuration_revision == configuration_revision
                })
                .cloned();
            let Some(entry) = entry else {
                return if state
                    .entries
                    .iter()
                    .any(|stored| stored.entry.work.address == work.address)
                    || state
                        .delivery_addresses
                        .iter()
                        .any(|binding| binding.address == work.address)
                {
                    ContextScoutDurableStoreOutcomeV1::Superseded
                } else {
                    ContextScoutDurableStoreOutcomeV1::Unavailable
                };
            };
            if validate_context_scout_delivery_receipt(&entry.envelope, &receipt).is_err() {
                return ContextScoutDurableStoreOutcomeV1::Unavailable;
            }
            state.record_delivery(&ContextScoutDurableClaimV1 { entry, lease }, &receipt)
        })
        .await
        .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }

    async fn record_feedback_inner(
        &self,
        receipt: &ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutDurableStoreOutcomeV1 {
        if validate_context_scout_feedback(receipt, feedback).is_err() {
            return ContextScoutDurableStoreOutcomeV1::Unavailable;
        }
        let receipt = receipt.clone();
        self.update_state("record Context Scout feedback", move |state| {
            let Some(stored_receipt) = state
                .receipts
                .iter()
                .find(|stored| stored.receipt_id == receipt.receipt_id)
            else {
                return ContextScoutDurableStoreOutcomeV1::Unavailable;
            };
            if stored_receipt != &receipt {
                return ContextScoutDurableStoreOutcomeV1::Superseded;
            }
            if state.feedback.contains(&feedback) {
                return ContextScoutDurableStoreOutcomeV1::Duplicate;
            }
            if state.feedback.len() == MAX_SCOUT_RECENT_DELIVERIES {
                state.feedback.remove(0);
            }
            state.feedback.push(feedback);
            ContextScoutDurableStoreOutcomeV1::Stored
        })
        .await
        .unwrap_or(ContextScoutDurableStoreOutcomeV1::Unavailable)
    }
}

impl ContextScoutDurableStoreV1 for ProjectContextScoutDurableStoreV1 {
    fn startup(
        &self,
        now: UtcMicros,
        limit: usize,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStartupOutcomeV1> {
        Box::pin(self.startup_inner(now, limit))
    }

    fn enqueue(
        &self,
        entry: ContextScoutDurableQueueEntryV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
        Box::pin(self.enqueue_inner(entry))
    }

    fn claim(
        &self,
        address: ContextScoutAddressV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableClaimOutcomeV1> {
        Box::pin(self.claim_inner(address, now, lease))
    }

    fn requeue(
        &self,
        claimed: ContextScoutDurableClaimV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
        Box::pin(self.requeue_inner(claimed))
    }

    fn cancel_work(
        &self,
        work: ContextScoutWorkV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
        Box::pin(self.cancel_inner(work))
    }

    fn record_delivery<'a>(
        &'a self,
        claim: &'a ContextScoutDurableClaimV1,
        receipt: &'a ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
        Box::pin(self.record_delivery_inner(claim, receipt))
    }

    fn record_delivery_by_lease<'a>(
        &'a self,
        work: ContextScoutWorkV1,
        envelope_id: [u8; 16],
        lease: ContextScoutLeaseV1,
        configuration_revision: [u8; 32],
        receipt: &'a ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
        Box::pin(self.record_delivery_by_lease_inner(
            work,
            envelope_id,
            lease,
            configuration_revision,
            receipt,
        ))
    }

    fn record_feedback<'a>(
        &'a self,
        receipt: &'a ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
        Box::pin(self.record_feedback_inner(receipt, feedback))
    }
}
