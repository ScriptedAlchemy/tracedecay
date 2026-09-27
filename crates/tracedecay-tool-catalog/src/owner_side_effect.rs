//! Side-effecting operations the project's graph-tool owner serves beside its
//! reads.

use crate::{ApplicationSurfaceOperation, EffectClass};

/// The ceiling for an owner call whose requested work is itself a long job,
/// such as running a test suite.
pub const LONG_RUNNING_CEILING_MILLIS: u64 = 600_000;

/// The ceiling for an interactive owner call.
pub const INTERACTIVE_CEILING_MILLIS: u64 = 120_000;

/// How the server treats identical calls that are in flight together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdenticalCallPolicyV1 {
    /// Every call runs its own effect; a second identical call is never
    /// answered from the first one's result.
    RunEach,
}

/// One side-effecting owner entry: what the call does, how long it may run,
/// and how identical concurrent calls are treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerSideEffectEntryV1 {
    pub effect: EffectClass,
    pub ceiling_millis: u64,
    pub identical_calls: IdenticalCallPolicyV1,
}

impl ApplicationSurfaceOperation {
    /// The side-effect entry of an owner-served operation that acts beyond
    /// reading; `None` for every read.
    pub const fn owner_side_effect(self) -> Option<OwnerSideEffectEntryV1> {
        match self {
            Self::RunAffectedTests => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::SpawnsProcess,
                ceiling_millis: LONG_RUNNING_CEILING_MILLIS,
                identical_calls: IdenticalCallPolicyV1::RunEach,
            }),
            Self::Dashboard => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::BindsServer,
                ceiling_millis: INTERACTIVE_CEILING_MILLIS,
                identical_calls: IdenticalCallPolicyV1::RunEach,
            }),
            // Admission can wait out the scheduler's cold mount.
            Self::AdminSync => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::SchedulesWork,
                ceiling_millis: LONG_RUNNING_CEILING_MILLIS,
                identical_calls: IdenticalCallPolicyV1::RunEach,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owner_side_effects_carry_an_entry_and_none_merges_identical_calls() {
        let entries = ApplicationSurfaceOperation::ALL
            .into_iter()
            .filter_map(|operation| {
                operation
                    .owner_side_effect()
                    .map(|entry| (operation.as_str(), entry))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            [
                (
                    "run_affected_tests",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::SpawnsProcess,
                        ceiling_millis: 600_000,
                        identical_calls: IdenticalCallPolicyV1::RunEach,
                    }
                ),
                (
                    "dashboard",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::BindsServer,
                        ceiling_millis: 120_000,
                        identical_calls: IdenticalCallPolicyV1::RunEach,
                    }
                ),
                (
                    "admin_sync",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::SchedulesWork,
                        ceiling_millis: 600_000,
                        identical_calls: IdenticalCallPolicyV1::RunEach,
                    }
                ),
            ]
        );
        assert_eq!(ApplicationSurfaceOperation::Files.owner_side_effect(), None);
    }
}
