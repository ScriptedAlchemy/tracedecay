//! Side-effecting operations the project's graph-tool owner serves beside its
//! reads.

use crate::{ApplicationSurfaceOperation, EffectClass};

/// The ceiling for an owner call whose requested work is itself a long job,
/// such as running a test suite.
pub const LONG_RUNNING_CEILING_MILLIS: u64 = 600_000;

/// The ceiling for an interactive owner call.
pub const INTERACTIVE_CEILING_MILLIS: u64 = 120_000;

/// One side-effecting owner entry: what the call does and how long it may
/// run. Every such call runs its own effect; a second identical call is never
/// answered from the first one's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerSideEffectEntryV1 {
    pub effect: EffectClass,
    pub ceiling_millis: u64,
}

impl ApplicationSurfaceOperation {
    /// The side-effect entry of an owner-served operation that acts beyond
    /// reading; `None` for every read.
    pub const fn owner_side_effect(self) -> Option<OwnerSideEffectEntryV1> {
        match self {
            Self::RunAffectedTests => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::SpawnsProcess,
                ceiling_millis: LONG_RUNNING_CEILING_MILLIS,
            }),
            Self::Dashboard => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::BindsServer,
                ceiling_millis: INTERACTIVE_CEILING_MILLIS,
            }),
            // Admission can wait out the scheduler's cold mount.
            Self::AdminSync => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::SchedulesWork,
                ceiling_millis: LONG_RUNNING_CEILING_MILLIS,
            }),
            // A transcript import or registry collection is the operator's
            // own long job.
            Self::AdminCli => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::MaintainsProfileState,
                ceiling_millis: LONG_RUNNING_CEILING_MILLIS,
            }),
            // A project automation reconcile waits on that project's scheduler.
            Self::AdminProject => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::MaintainsOwnerState,
                ceiling_millis: LONG_RUNNING_CEILING_MILLIS,
            }),
            Self::HookRuntime => Some(OwnerSideEffectEntryV1 {
                effect: EffectClass::RecordsHostEvidence,
                ceiling_millis: INTERACTIVE_CEILING_MILLIS,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owner_side_effects_carry_an_entry() {
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
                    }
                ),
                (
                    "dashboard",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::BindsServer,
                        ceiling_millis: 120_000,
                    }
                ),
                (
                    "admin_sync",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::SchedulesWork,
                        ceiling_millis: 600_000,
                    }
                ),
                (
                    "admin_cli",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::MaintainsProfileState,
                        ceiling_millis: 600_000,
                    }
                ),
                (
                    "admin_project",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::MaintainsOwnerState,
                        ceiling_millis: 600_000,
                    }
                ),
                (
                    "hook_runtime",
                    OwnerSideEffectEntryV1 {
                        effect: EffectClass::RecordsHostEvidence,
                        ceiling_millis: 120_000,
                    }
                ),
            ]
        );
        assert_eq!(ApplicationSurfaceOperation::Files.owner_side_effect(), None);
    }
}
