use std::marker::PhantomData;

use super::{ExecutionControl, MeasuredTemporalValue, TemporalPortError, TemporalRecord};
use crate::ranking::RankingCandidate;

const MAX_READ_ITEMS: usize = 8_192;
const MAX_READ_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_READ_ITEM_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_PAGE_ITEMS_CAP: usize = 1_024;
const MAX_CONTINUATION_KEY_BYTES: usize = 4_096;
pub(super) const MAX_BOUNDED_PAGE_PREALLOC: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageLimits {
    max_items: usize,
    max_total_bytes: usize,
    max_item_bytes: usize,
    max_page_items: usize,
}

impl PageLimits {
    pub fn new(
        max_items: usize,
        max_total_bytes: usize,
        max_item_bytes: usize,
        max_page_items: usize,
    ) -> Result<Self, TemporalPortError> {
        if max_items == 0 || max_items > MAX_READ_ITEMS {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "item count",
            });
        }
        if max_total_bytes == 0 || max_total_bytes > MAX_READ_TOTAL_BYTES {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "total bytes",
            });
        }
        if max_item_bytes == 0 || max_item_bytes > MAX_READ_ITEM_BYTES {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "item bytes",
            });
        }
        if max_page_items == 0 || max_page_items > max_items || max_page_items > MAX_PAGE_ITEMS_CAP
        {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "page item count",
            });
        }
        Ok(Self {
            max_items,
            max_total_bytes,
            max_item_bytes,
            max_page_items,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateFieldCaps {
    stable_id_bytes: usize,
    anchor_id_bytes: usize,
    metadata_field_bytes: usize,
}

impl CandidateFieldCaps {
    pub(super) const fn new(
        stable_id_bytes: usize,
        anchor_id_bytes: usize,
        metadata_field_bytes: usize,
    ) -> Self {
        Self {
            stable_id_bytes,
            anchor_id_bytes,
            metadata_field_bytes,
        }
    }

    pub const fn stable_id_bytes(self) -> usize {
        self.stable_id_bytes
    }

    pub const fn metadata_field_bytes(self) -> usize {
        self.metadata_field_bytes
    }

    pub const fn anchor_id_bytes(self) -> usize {
        self.anchor_id_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageKey(String);

impl PageKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    page_index: usize,
    keyset: Option<PageKey>,
    remaining_items: usize,
    remaining_total_bytes: usize,
    max_item_bytes: usize,
    page_item_limit: usize,
    page_total_byte_limit: usize,
    max_key_bytes: usize,
    candidate_field_caps: Option<CandidateFieldCaps>,
}

impl PageRequest {
    #[cfg(any(test, feature = "test-helpers"))]
    pub const fn for_test(
        remaining_items: usize,
        remaining_total_bytes: usize,
        max_item_bytes: usize,
        page_item_limit: usize,
        max_key_bytes: usize,
    ) -> Self {
        Self {
            page_index: 0,
            keyset: None,
            remaining_items,
            remaining_total_bytes,
            max_item_bytes,
            page_item_limit,
            page_total_byte_limit: remaining_total_bytes,
            max_key_bytes,
            candidate_field_caps: None,
        }
    }

    pub const fn page_index(&self) -> usize {
        self.page_index
    }

    pub fn keyset(&self) -> Option<&PageKey> {
        self.keyset.as_ref()
    }

    pub const fn remaining_items(&self) -> usize {
        self.remaining_items
    }

    pub const fn remaining_total_bytes(&self) -> usize {
        self.remaining_total_bytes
    }

    pub const fn max_item_bytes(&self) -> usize {
        self.max_item_bytes
    }

    pub const fn page_item_limit(&self) -> usize {
        self.page_item_limit
    }

    pub const fn page_total_byte_limit(&self) -> usize {
        self.page_total_byte_limit
    }

    pub const fn max_key_bytes(&self) -> usize {
        self.max_key_bytes
    }

    pub const fn candidate_field_caps(&self) -> Option<CandidateFieldCaps> {
        self.candidate_field_caps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageStatus {
    More,
    Complete,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BoundedPage<T> {
    items: Vec<T>,
    encoded_bytes: usize,
    status: PageStatus,
    pub(super) continuation: Option<PageKey>,
}

impl<T> BoundedPage<T> {
    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    pub const fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub const fn status(&self) -> PageStatus {
        self.status
    }

    pub fn continuation(&self) -> Option<&PageKey> {
        self.continuation.as_ref()
    }
}

pub struct ReadState<T> {
    limits: PageLimits,
    consumed_items: usize,
    consumed_bytes: usize,
    page_index: usize,
    keyset: Option<PageKey>,
    marker: PhantomData<fn() -> T>,
}

impl<T> ReadState<T> {
    pub const fn new(limits: PageLimits) -> Self {
        Self {
            limits,
            consumed_items: 0,
            consumed_bytes: 0,
            page_index: 0,
            keyset: None,
            marker: PhantomData,
        }
    }

    pub const fn consumed_items(&self) -> usize {
        self.consumed_items
    }

    pub const fn consumed_bytes(&self) -> usize {
        self.consumed_bytes
    }

    pub(super) fn require_within_limits(
        &self,
        max_items: usize,
        max_total_bytes: usize,
        max_item_bytes: usize,
        resources: ReadBudgetResources,
    ) -> Result<(), TemporalPortError> {
        if self.limits.max_items > max_items {
            return Err(TemporalPortError::BudgetExceeded {
                resource: resources.item_count,
            });
        }
        if self.limits.max_total_bytes > max_total_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: resources.total_bytes,
            });
        }
        if self.limits.max_item_bytes > max_item_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: resources.item_bytes,
            });
        }
        Ok(())
    }

    pub(super) fn request(
        &self,
        max_key_bytes: usize,
        candidate_field_caps: Option<CandidateFieldCaps>,
    ) -> PageRequest {
        let remaining_items = self.limits.max_items - self.consumed_items;
        let page_item_limit = remaining_items.min(self.limits.max_page_items);
        let remaining_total_bytes = self.limits.max_total_bytes - self.consumed_bytes;
        PageRequest {
            page_index: self.page_index,
            keyset: self.keyset.clone(),
            remaining_items,
            remaining_total_bytes,
            max_item_bytes: self.limits.max_item_bytes,
            page_item_limit,
            page_total_byte_limit: remaining_total_bytes
                .min(self.limits.max_item_bytes.saturating_mul(page_item_limit)),
            max_key_bytes,
            candidate_field_caps,
        }
    }

    pub(super) fn is_exhausted(&self) -> bool {
        self.consumed_items == self.limits.max_items
            || self.consumed_bytes == self.limits.max_total_bytes
    }

    pub(super) fn begin_page<'a>(
        &'a mut self,
        control: &'a ExecutionControl,
        max_key_bytes: usize,
        candidate_field_caps: Option<CandidateFieldCaps>,
        budget_resources: ReadBudgetResources,
    ) -> BoundedPageSink<'a, T> {
        BoundedPageSink {
            max_items: self.limits.max_items,
            max_total_bytes: self.limits.max_total_bytes,
            max_item_bytes: self.limits.max_item_bytes,
            max_page_items: self.limits.max_page_items,
            consumed_items: &mut self.consumed_items,
            consumed_bytes: &mut self.consumed_bytes,
            control,
            max_key_bytes,
            candidate_field_caps,
            budget_resources,
            items: Vec::with_capacity(self.limits.max_page_items.min(MAX_BOUNDED_PAGE_PREALLOC)),
            encoded_bytes: 0,
            continuation: None,
        }
    }

    pub(super) fn advanced_page(&mut self, continuation: Option<PageKey>) {
        self.page_index += 1;
        self.keyset = continuation;
    }

    pub(super) fn incomplete_coverage_error(
        &self,
        resources: ReadBudgetResources,
    ) -> TemporalPortError {
        if self.consumed_items == self.limits.max_items {
            TemporalPortError::BudgetExceeded {
                resource: resources.item_count,
            }
        } else {
            TemporalPortError::BudgetExceeded {
                resource: resources.total_bytes,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReadBudgetResources {
    item_count: &'static str,
    item_bytes: &'static str,
    total_bytes: &'static str,
}

pub(super) const CANDIDATE_READ_BUDGET: ReadBudgetResources = ReadBudgetResources {
    item_count: "candidate item count",
    item_bytes: "candidate item bytes",
    total_bytes: "candidate total bytes",
};

pub(super) const RECORD_READ_BUDGET: ReadBudgetResources = ReadBudgetResources {
    item_count: "record item count",
    item_bytes: "record item bytes",
    total_bytes: "record total bytes",
};

pub type CandidateReadState = ReadState<RankingCandidate>;
pub type TemporalRecordReadState = ReadState<TemporalRecord>;

pub struct BoundedPageSink<'a, T> {
    max_items: usize,
    max_total_bytes: usize,
    max_item_bytes: usize,
    max_page_items: usize,
    consumed_items: &'a mut usize,
    consumed_bytes: &'a mut usize,
    control: &'a ExecutionControl,
    max_key_bytes: usize,
    candidate_field_caps: Option<CandidateFieldCaps>,
    budget_resources: ReadBudgetResources,
    items: Vec<T>,
    encoded_bytes: usize,
    continuation: Option<PageKey>,
}

// Measurement stays sealed so producers cannot substitute underreported byte counts.
impl<T: MeasuredTemporalValue> BoundedPageSink<'_, T> {
    pub fn push(&mut self, value: T) -> Result<(), TemporalPortError> {
        self.control.checkpoint()?;
        if self.items.len() == self.max_page_items || *self.consumed_items == self.max_items {
            return Err(TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.item_count,
            });
        }
        value.validate_candidate_fields(self.candidate_field_caps)?;
        let encoded_bytes = value.measured_encoded_bytes()?;
        if encoded_bytes > self.max_item_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.item_bytes,
            });
        }
        let total_bytes = self.consumed_bytes.checked_add(encoded_bytes).ok_or(
            TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.total_bytes,
            },
        )?;
        if total_bytes > self.max_total_bytes {
            return Err(TemporalPortError::BudgetExceeded {
                resource: self.budget_resources.total_bytes,
            });
        }
        *self.consumed_items += 1;
        *self.consumed_bytes = total_bytes;
        self.encoded_bytes += encoded_bytes;
        self.items.push(value);
        self.control.checkpoint()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(test)]
    pub(super) fn preallocated_capacity(&self) -> usize {
        self.items.capacity()
    }

    pub fn set_continuation_key(&mut self, key: PageKey) -> Result<(), TemporalPortError> {
        let key_cap = self.max_key_bytes.min(MAX_CONTINUATION_KEY_BYTES);
        if key.0.len() > key_cap {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key bytes",
            });
        }
        self.continuation = Some(key);
        Ok(())
    }

    pub(super) fn finish(self, status: PageStatus) -> Result<BoundedPage<T>, TemporalPortError> {
        if status == PageStatus::More && self.items.is_empty() {
            return Err(TemporalPortError::Read {
                operation: "produce bounded page",
                message: "producer returned an empty continuation page".to_string(),
            });
        }
        if status == PageStatus::More && self.continuation.is_none() {
            return Err(TemporalPortError::Read {
                operation: "produce bounded page",
                message: "producer omitted the continuation key".to_string(),
            });
        }
        Ok(BoundedPage {
            items: self.items,
            encoded_bytes: self.encoded_bytes,
            status,
            continuation: self.continuation,
        })
    }
}

pub type CandidatePageSink<'a> = BoundedPageSink<'a, RankingCandidate>;
pub type TemporalRecordPageSink<'a> = BoundedPageSink<'a, TemporalRecord>;
