//! Effect versus read terminal, cancellation, and deadline shapes shared by
//! the public Git and native-integration catalogs. The two surfaces differ
//! in operations, not in these effect-class consequences.

use tracedecay_tool_catalog::{CancellationPoint, DeadlineBehavior, EffectClass, TerminalState};

pub(super) fn cancellation_points(effect: EffectClass) -> Vec<CancellationPoint> {
    if effect.is_effect() {
        vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::AfterCommit,
        ]
    } else {
        vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ]
    }
}

pub(super) fn deadline_behavior(effect: EffectClass) -> DeadlineBehavior {
    if effect.is_effect() {
        DeadlineBehavior::ReturnEffectReceipt
    } else {
        DeadlineBehavior::ReturnOperationReceipt
    }
}

pub(super) fn terminal_states(effect: EffectClass) -> Vec<TerminalState> {
    if effect.is_effect() {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ]
    } else {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ]
    }
}
