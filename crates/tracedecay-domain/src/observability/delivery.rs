use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{UtcMicros, WorkAttemptIdentityV1};

use super::{
    CoverageStateV1, DeliveryEventClassV1, DeliverySurfaceFamilyV1, WorkDeliveryFanoutObservedV1,
};

/// Maximum number of independently settled recipients for one owner event and
/// surface. Fan-out beyond this bound must be split by the owner rather than
/// silently truncating delivery evidence.
pub const MAX_DELIVERY_RECIPIENTS_V1: u16 = 64;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeliveryChannelIdentityV1 {
    pub surface: DeliverySurfaceFamilyV1,
    /// Payload-free identity for the concrete recipient/connection/subscriber.
    pub channel_ref: String,
}

impl DeliveryChannelIdentityV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::canonical_text::is_canonical_text_within(&self.channel_ref, 128) {
            return Err("delivery_channel_ref");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeliverySettlementAttemptV1 {
    /// Stable identity of the owning product event, not the transport request.
    pub owner_event_id: String,
    pub event_class: DeliveryEventClassV1,
    pub channel: DeliveryChannelIdentityV1,
    /// Exact optional Work source for this fan-out. A transport must supply
    /// this only when it received the typed attempt identity from the Work
    /// authority; owner-event text is never parsed into a Work binding.
    pub work_attempt: Option<WorkAttemptIdentityV1>,
    /// Exact eligible-recipient denominator for this owner event and surface.
    pub eligible: u16,
    pub valid_at: UtcMicros,
    pub attempted_at: UtcMicros,
}

impl DeliverySettlementAttemptV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::canonical_text::is_canonical_text_within(&self.owner_event_id, 128) {
            return Err("delivery_owner_event_id");
        }
        self.channel.validate()?;
        if self.eligible == 0 || self.eligible > MAX_DELIVERY_RECIPIENTS_V1 {
            return Err("delivery_eligible");
        }
        if self.valid_at.0 <= 0 || self.attempted_at < self.valid_at {
            return Err("delivery_attempted_at");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverySettlementOutcomeV1 {
    Delivered,
    Deduplicated,
    Dropped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDropReasonV1 {
    Backpressure,
    Cancelled,
    Deadline,
    Disconnected,
    Invalid,
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeliverySettlementV1 {
    pub attempt: DeliverySettlementAttemptV1,
    pub outcome: DeliverySettlementOutcomeV1,
    pub settled_at: UtcMicros,
    pub drop_reason: Option<DeliveryDropReasonV1>,
}

impl DeliverySettlementV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.attempt.validate()?;
        if self.settled_at < self.attempt.attempted_at {
            return Err("delivery_settled_at");
        }
        if (self.outcome == DeliverySettlementOutcomeV1::Dropped) != self.drop_reason.is_some() {
            return Err("delivery_drop_reason");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DeliverySettlementCensusV1 {
    pub owner_event_id: String,
    pub event_class: DeliveryEventClassV1,
    pub surface: DeliverySurfaceFamilyV1,
    /// The same immutable Work binding recorded for the fan-out identity.
    /// `None` means this delivery owner did not receive a typed Work attempt,
    /// not that no Work delivery occurred.
    pub work_attempt: Option<WorkAttemptIdentityV1>,
    pub eligible: u16,
    pub attempted: u16,
    pub delivered: u16,
    pub deduplicated: u16,
    pub dropped: u16,
    /// Attempted recipients that do not yet have a durable terminal outcome.
    pub unknown: u16,
    pub valid_at: UtcMicros,
    /// Time of the durable settlement that produced this census.
    pub settled_at: UtcMicros,
    pub coverage: CoverageStateV1,
}

impl DeliverySettlementCensusV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !crate::canonical_text::is_canonical_text_within(&self.owner_event_id, 128) {
            return Err("delivery_owner_event_id");
        }
        if self.eligible == 0
            || self.eligible > MAX_DELIVERY_RECIPIENTS_V1
            || self.attempted > self.eligible
            || self
                .delivered
                .saturating_add(self.deduplicated)
                .saturating_add(self.dropped)
                .saturating_add(self.unknown)
                != self.attempted
        {
            return Err("delivery_census_counts");
        }
        if self.valid_at.0 <= 0 || self.settled_at < self.valid_at {
            return Err("delivery_census_time");
        }
        let complete = self.attempted == self.eligible && self.unknown == 0;
        if (complete && self.coverage != CoverageStateV1::Known)
            || (!complete && self.coverage != CoverageStateV1::Partial)
        {
            return Err("delivery_census_coverage");
        }
        Ok(())
    }

    pub fn as_fanout_observation(&self) -> WorkDeliveryFanoutObservedV1 {
        WorkDeliveryFanoutObservedV1 {
            event_class: self.event_class,
            surface: self.surface,
            eligible: self.eligible,
            attempted: self.attempted,
            delivered: self.delivered,
            deduplicated: self.deduplicated,
            dropped: self.dropped,
            unknown: self.unknown,
        }
    }
}
