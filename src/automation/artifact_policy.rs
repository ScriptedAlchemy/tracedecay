use super::backend::AgentTaskKind;
use super::run_ledger::AutomationRunLedgerRecord;

#[derive(Debug, Clone, Copy)]
pub(super) struct TaskArtifactPolicy {
    pub(super) optimizer_action: &'static str,
    accepted_next_actions: &'static [&'static str],
    rejected_next_actions: &'static [&'static str],
    handoff_test: &'static str,
    eval_replay_command: &'static str,
}

impl TaskArtifactPolicy {
    pub(super) fn next_actions(self, record: &AutomationRunLedgerRecord) -> Vec<&'static str> {
        if record.accepted_count > 0 {
            self.accepted_next_actions.to_vec()
        } else {
            self.rejected_next_actions.to_vec()
        }
    }

    pub(super) fn handoff_tests(self) -> Vec<&'static str> {
        vec![self.handoff_test]
    }

    pub(super) fn eval_replay_commands(self) -> Vec<&'static str> {
        vec![self.eval_replay_command]
    }
}

pub(super) fn artifact_policy(task: AgentTaskKind) -> TaskArtifactPolicy {
    match task {
        AgentTaskKind::MemoryCurator => TaskArtifactPolicy {
            optimizer_action: "update memory curation evidence or apply policy",
            accepted_next_actions: &[
                "review accepted memory curation ops",
                "apply through dashboard or CLI if approved",
            ],
            rejected_next_actions: &[
                "review rejected curation reasons",
                "collect more evidence before applying changes",
            ],
            handoff_test: "cargo test --test automation_runner_test memory_curator",
            eval_replay_command: "cargo test --test automation_runner_test memory_curator_runner_validates_backend_ops_and_records_ledger -- --nocapture",
        },
        AgentTaskKind::SessionReflector => TaskArtifactPolicy {
            optimizer_action: "update fact proposal evidence or dedupe policy",
            accepted_next_actions: &[
                "review pending fact proposals",
                "approve or reject fact proposals from the dashboard",
            ],
            rejected_next_actions: &[
                "review rejected fact proposals",
                "adjust evidence query before rerunning",
            ],
            handoff_test: "cargo test --test automation_runner_test session_reflector",
            eval_replay_command: "cargo test --test automation_runner_test session_reflector_runner_validates_fact_proposals_without_applying -- --nocapture",
        },
        AgentTaskKind::SkillWriter => TaskArtifactPolicy {
            optimizer_action: "update skill writer evidence or draft validation",
            accepted_next_actions: &[
                "review managed skill drafts or auto-enabled changes",
                "approve, disable, or archive through managed skill controls",
            ],
            rejected_next_actions: &[
                "review rejected skill proposals",
                "collect stronger usage evidence before rerunning",
            ],
            handoff_test: "cargo test --test automation_runner_test skill_writer",
            eval_replay_command: "cargo test --test automation_runner_test skill_writer_runner_creates_pending_skill_drafts_for_approval -- --nocapture",
        },
    }
}
