use crate::AutomationRunRecord;
use crate::backend::AgentTaskKind;

#[derive(Debug, Clone, Copy)]
pub struct TaskArtifactPolicy {
    pub optimizer_action: &'static str,
    accepted_next_actions: &'static [&'static str],
    rejected_next_actions: &'static [&'static str],
    handoff_test: &'static str,
    eval_replay_command: &'static str,
}

impl TaskArtifactPolicy {
    pub fn next_actions<R>(self, record: &R) -> Vec<&'static str>
    where
        R: AutomationRunRecord + ?Sized,
    {
        if record.accepted_count() > 0 {
            self.accepted_next_actions.to_vec()
        } else {
            self.rejected_next_actions.to_vec()
        }
    }

    pub fn handoff_tests(self) -> Vec<&'static str> {
        vec![self.handoff_test]
    }

    pub fn eval_replay_commands(self) -> Vec<&'static str> {
        vec![self.eval_replay_command]
    }
}

pub fn artifact_policy(task: AgentTaskKind) -> TaskArtifactPolicy {
    match task {
        AgentTaskKind::MemoryCurator => TaskArtifactPolicy {
            optimizer_action: "update memory curation evidence or validation repair",
            accepted_next_actions: &[
                "inspect autonomously applied memory curation outcomes",
                "restore or roll back through administrative controls if needed",
            ],
            rejected_next_actions: &[
                "inspect quarantined validation failures",
                "collect stronger evidence before rerunning curation",
            ],
            handoff_test: "cargo test --test automation_runner_test memory_curator",
            eval_replay_command: "cargo test --test automation_runner_test memory_curator_repairs_then_applies_validated_ops_and_records_ledger -- --nocapture",
        },
        AgentTaskKind::SessionReflector => TaskArtifactPolicy {
            optimizer_action: "update automatic fact receipt evidence or dedupe policy",
            accepted_next_actions: &[
                "inspect automatically applied fact receipts and canonical fact ids",
                "use administrative controls to restore or roll back automatic fact receipts if needed",
            ],
            rejected_next_actions: &[
                "inspect quarantined automatic fact receipts",
                "adjust evidence query before rerunning",
            ],
            handoff_test: "cargo test --test automation_runner_test session_reflector",
            eval_replay_command: "cargo test --test automation_runner_test session_reflector_runner_applies_valid_automatic_facts_by_default -- --nocapture",
        },
        AgentTaskKind::SkillWriter => TaskArtifactPolicy {
            optimizer_action: "update skill writer evidence or activation validation",
            accepted_next_actions: &[
                "inspect automatically activated managed skill changes",
                "disable or archive through managed skill controls if needed",
            ],
            rejected_next_actions: &[
                "inspect rejected skill validation outcomes",
                "collect stronger usage evidence before rerunning",
            ],
            handoff_test: "cargo test --test automation_runner_test skill_writer",
            eval_replay_command: "cargo test --test automation_runner_test skill_writer_runner_activates_validated_skills -- --nocapture",
        },
        AgentTaskKind::CombinedReview => TaskArtifactPolicy {
            optimizer_action: "update combined automation evidence or per-task validation",
            accepted_next_actions: &[
                "inspect applied automatic fact receipts and activated managed skills",
                "confirm the atomic commit receipt before consuming either automatic outcome",
            ],
            rejected_next_actions: &[
                "inspect rejected automatic fact and skill validation outcomes",
                "collect more evidence before rerunning",
            ],
            handoff_test: "cargo test --test automation_runner_test combined_review",
            eval_replay_command: "cargo test --test automation_runner_test combined_review_runner_records_both_tasks_from_one_backend_call -- --nocapture",
        },
        AgentTaskKind::UserJob => TaskArtifactPolicy {
            optimizer_action: "update the job prompt, schedule, or delivery target",
            accepted_next_actions: &[
                "inspect the delivered job output",
                "adjust the job definition from the dashboard if needed",
            ],
            rejected_next_actions: &[
                "inspect the job failure reason",
                "adjust the job definition before the next scheduled run",
            ],
            handoff_test: "cargo test --test automation_runner_test jobs",
            eval_replay_command: "cargo test --test automation_runner_test jobs -- --nocapture",
        },
    }
}
