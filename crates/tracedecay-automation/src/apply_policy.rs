use serde_json::{Value, json};

use crate::AutomationRunRecord;
use crate::backend::AgentTaskKind;
use crate::config::AutomationConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryApplySubject {
    CurationOps,
    SessionFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryApplyDecision {
    AutoApplyAllowed,
    ApplyIncomplete,
    ProposalOnly,
    NoValidOps,
    NoValidFacts,
}

impl MemoryApplyDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoApplyAllowed => "auto_apply_allowed",
            Self::ApplyIncomplete => "apply_incomplete",
            Self::ProposalOnly => "proposal_only",
            Self::NoValidOps => "no_valid_ops",
            Self::NoValidFacts => "no_valid_facts",
        }
    }
}

impl MemoryApplySubject {
    fn no_valid_decision(self) -> MemoryApplyDecision {
        match self {
            Self::CurationOps => MemoryApplyDecision::NoValidOps,
            Self::SessionFacts => MemoryApplyDecision::NoValidFacts,
        }
    }

    fn incomplete_decision(self) -> MemoryApplyDecision {
        match self {
            Self::CurationOps => MemoryApplyDecision::ApplyIncomplete,
            Self::SessionFacts => MemoryApplyDecision::ProposalOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryApplyPolicy {
    subject: MemoryApplySubject,
    accepted_count: usize,
    auto_apply_memory_ops: bool,
    mutates_store: bool,
    fully_applied: bool,
}

impl MemoryApplyPolicy {
    pub fn curation_ops(config: &AutomationConfig, accepted_count: usize) -> Self {
        let should_apply = should_auto_apply_memory_ops(config, accepted_count);
        Self::new(
            MemoryApplySubject::CurationOps,
            config,
            accepted_count,
            should_apply,
            should_apply,
        )
    }

    pub fn applied_curation_ops(
        config: &AutomationConfig,
        accepted_count: usize,
        applied_count: usize,
    ) -> Self {
        Self::new(
            MemoryApplySubject::CurationOps,
            config,
            accepted_count,
            applied_count > 0,
            accepted_count > 0 && applied_count >= accepted_count,
        )
    }

    pub fn session_facts(accepted_count: usize, applied_count: usize, auto_managed: bool) -> Self {
        Self {
            subject: MemoryApplySubject::SessionFacts,
            accepted_count,
            auto_apply_memory_ops: accepted_count > 0,
            mutates_store: auto_managed,
            fully_applied: accepted_count > 0 && applied_count >= accepted_count,
        }
    }

    fn new(
        subject: MemoryApplySubject,
        config: &AutomationConfig,
        accepted_count: usize,
        mutates_store: bool,
        fully_applied: bool,
    ) -> Self {
        Self {
            subject,
            accepted_count,
            auto_apply_memory_ops: config.auto_apply_memory_ops,
            mutates_store,
            fully_applied,
        }
    }

    pub fn should_apply(accepted_count: usize) -> bool {
        accepted_count > 0
    }

    pub fn decision(self) -> MemoryApplyDecision {
        if self.accepted_count == 0 {
            self.subject.no_valid_decision()
        } else if self.fully_applied
            || (self.subject == MemoryApplySubject::SessionFacts && self.mutates_store)
        {
            MemoryApplyDecision::AutoApplyAllowed
        } else {
            self.subject.incomplete_decision()
        }
    }

    pub fn to_json(self) -> Value {
        let decision = self.decision();
        json!({
            "decision": decision.as_str(),
            "auto_apply_memory_ops": self.auto_apply_memory_ops,
            "require_dashboard_approval": false,
            "approval_required": false,
            "autonomous_memory_apply": self.mutates_store,
            "mutates_store": self.mutates_store,
        })
    }
}

pub fn record_has_auto_applied_memory_ops<R>(task: AgentTaskKind, record: &R) -> bool
where
    R: AutomationRunRecord + ?Sized,
{
    match task {
        AgentTaskKind::MemoryCurator => memory_curator_record_fully_applied(record),
        AgentTaskKind::SessionReflector => session_fact_record_fully_applied(record),
        _ => false,
    }
}

fn memory_curator_record_fully_applied<R>(record: &R) -> bool
where
    R: AutomationRunRecord + ?Sized,
{
    if record.accepted_count() == 0 {
        return false;
    }
    let applied_count = record
        .validation_report()
        .map_or(0, memory_curator_applied_count);
    applied_count >= record.accepted_count()
}

fn memory_curator_applied_count(report: &Value) -> usize {
    report
        .get("applied")
        .and_then(value_as_usize)
        .or_else(|| {
            report
                .get("results")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .filter(|result| {
                            matches!(
                                result.get("status").and_then(Value::as_str),
                                Some("deleted" | "merged")
                            )
                        })
                        .count()
                })
        })
        .unwrap_or(0)
}

fn session_fact_record_fully_applied<R>(record: &R) -> bool
where
    R: AutomationRunRecord + ?Sized,
{
    if record.accepted_count() == 0 {
        return false;
    }
    if record
        .validation_report()
        .is_some_and(session_fact_record_self_managed)
    {
        return true;
    }
    session_fact_applied_count(record) >= record.accepted_count()
}

fn session_fact_record_self_managed(report: &Value) -> bool {
    report.get("dry_run").and_then(Value::as_bool) == Some(false)
        && report
            .pointer("/session_fact_apply_policy/decision")
            .and_then(Value::as_str)
            == Some(MemoryApplyDecision::AutoApplyAllowed.as_str())
}

fn session_fact_applied_count<R>(record: &R) -> usize
where
    R: AutomationRunRecord + ?Sized,
{
    let report = record.validation_report();
    [
        record.applied_ops().and_then(array_len),
        report.and_then(|report| {
            report
                .pointer("/session_fact_apply_policy/applied_proposal_ids")
                .and_then(array_len)
        }),
        report.and_then(|report| {
            report
                .pointer("/session_fact_apply_policy/applied_fact_ids")
                .and_then(array_len)
        }),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(0)
}

fn array_len(value: &Value) -> Option<usize> {
    value.as_array().map(Vec::len)
}

pub(crate) fn value_as_usize(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
}

fn should_auto_apply_memory_ops(config: &AutomationConfig, accepted_count: usize) -> bool {
    accepted_count > 0 && config.auto_apply_memory_ops
}
