//! Pure hint delivery decisions for the production hook journey.

use serde::{Deserialize, Serialize};

/// Immutable dedupe/budget state for one real hook hint candidate.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HintDeliveryInputV1 {
    pub category_was_delivered: bool,
    pub escalation_was_delivered: bool,
    pub triggers_after_delivery: u32,
    pub delivered_in_session: usize,
    pub session_limit: usize,
    pub escalation_threshold: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HintDeliveryDecisionV1 {
    Deliver,
    DeliverEscalation,
    SuppressDuplicate,
    SuppressBudget,
}

/// Selects delivery from already-persisted hint state. State mutation remains
/// with the hook owner and happens only after this decision returns.
pub const fn decide_hint_delivery(input: HintDeliveryInputV1) -> HintDeliveryDecisionV1 {
    if !input.category_was_delivered {
        if input.delivered_in_session >= input.session_limit {
            HintDeliveryDecisionV1::SuppressBudget
        } else {
            HintDeliveryDecisionV1::Deliver
        }
    } else if input.escalation_was_delivered
        || input.triggers_after_delivery.saturating_add(1) < input.escalation_threshold
    {
        HintDeliveryDecisionV1::SuppressDuplicate
    } else {
        HintDeliveryDecisionV1::DeliverEscalation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_is_budgeted_and_escalates_once() {
        assert_eq!(
            decide_hint_delivery(HintDeliveryInputV1 {
                category_was_delivered: false,
                escalation_was_delivered: false,
                triggers_after_delivery: 0,
                delivered_in_session: 3,
                session_limit: 3,
                escalation_threshold: 3,
            }),
            HintDeliveryDecisionV1::SuppressBudget
        );
        assert_eq!(
            decide_hint_delivery(HintDeliveryInputV1 {
                category_was_delivered: true,
                escalation_was_delivered: false,
                triggers_after_delivery: 2,
                delivered_in_session: 1,
                session_limit: 3,
                escalation_threshold: 3,
            }),
            HintDeliveryDecisionV1::DeliverEscalation
        );
    }
}
