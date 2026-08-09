//! Exact read-only attempt-capacity evidence over the canonical admission rows.

use std::collections::BTreeSet;

use tracedecay_domain::{
    TaskId, WorkAuthority, WorkTopologyPolicyV1, configuration::TopologyConcurrencyPolicyV1,
};

use crate::work::work_authority;
use crate::{ApplicationProblem, RequestContext};

use super::{WorkAttemptService, WorkAttemptStoragePort, invalid_problem, storage_problem};

/// Maximum prospective task identities in one exact capacity census.
pub const MAX_WORK_ATTEMPT_CAPACITY_TASKS: usize = u16::MAX as usize;

/// One concurrency dimension that can refuse a prospective attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkAttemptCapacityScopeV1 {
    /// Every active attempt in the candidate's canonical project.
    Global,
    /// Every active attempt in the candidate's repository.
    Repository,
    /// Every active attempt for the candidate's task in that repository.
    Task,
}

/// Read-only answer for a prospective attempt. This is observational evidence;
/// only the bounded insertion transaction reserves capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkAttemptCapacityVerdictV1 {
    Available,
    Exhausted(BTreeSet<WorkAttemptCapacityScopeV1>),
}

/// Exact open-attempt counts and the registered limits used to interpret them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkAttemptCapacityV1 {
    global_active: u64,
    repository_active: u64,
    task_active: u64,
    concurrency: TopologyConcurrencyPolicyV1,
}

impl WorkAttemptCapacityV1 {
    /// Constructs the canonical verdict input returned by storage adapters.
    pub fn new(
        global_active: u64,
        repository_active: u64,
        task_active: u64,
        concurrency: TopologyConcurrencyPolicyV1,
    ) -> Self {
        Self {
            global_active,
            repository_active,
            task_active,
            concurrency,
        }
    }

    pub const fn global_active(&self) -> u64 {
        self.global_active
    }

    pub const fn repository_active(&self) -> u64 {
        self.repository_active
    }

    pub const fn task_active(&self) -> u64 {
        self.task_active
    }

    pub const fn concurrency(&self) -> &TopologyConcurrencyPolicyV1 {
        &self.concurrency
    }

    pub fn verdict(&self) -> WorkAttemptCapacityVerdictV1 {
        let mut exhausted = BTreeSet::new();
        if self.global_active >= u64::from(self.concurrency.maximum_global_active.get()) {
            exhausted.insert(WorkAttemptCapacityScopeV1::Global);
        }
        if self.repository_active >= u64::from(self.concurrency.maximum_active_per_repository.get())
        {
            exhausted.insert(WorkAttemptCapacityScopeV1::Repository);
        }
        if self.task_active >= u64::from(self.concurrency.maximum_parallel_per_task.get()) {
            exhausted.insert(WorkAttemptCapacityScopeV1::Task);
        }
        if exhausted.is_empty() {
            WorkAttemptCapacityVerdictV1::Available
        } else {
            WorkAttemptCapacityVerdictV1::Exhausted(exhausted)
        }
    }
}

impl<S> WorkAttemptService<S>
where
    S: WorkAttemptStoragePort,
{
    /// Reads exact current capacity for one task without reserving it.
    pub fn admission_capacity_against_registered_topology(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
        registered_topology: &WorkTopologyPolicyV1,
    ) -> Result<WorkAttemptCapacityV1, ApplicationProblem> {
        self.admission_capacities_against_registered_topology(
            context,
            std::slice::from_ref(task_id),
            registered_topology,
        )?
        .remove(task_id)
        .ok_or_else(capacity_query_problem)
    }

    /// Reads one coherent capacity snapshot for a canonical task set. Inputs
    /// must be strictly sorted and unique so callers cannot hide duplicate
    /// census work or produce order-dependent evidence.
    pub fn admission_capacities_against_registered_topology(
        &self,
        context: &RequestContext,
        task_ids: &[TaskId],
        registered_topology: &WorkTopologyPolicyV1,
    ) -> Result<std::collections::BTreeMap<TaskId, WorkAttemptCapacityV1>, ApplicationProblem> {
        if task_ids.len() > MAX_WORK_ATTEMPT_CAPACITY_TASKS
            || task_ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(capacity_query_problem());
        }
        let authority = work_authority(context)?;
        self.attempts
            .admission_capacities(&authority, task_ids, &registered_topology.concurrency)
            .map_err(storage_problem)
    }

    /// Whether this exact Work authority retains any non-terminal provider
    /// attempt. An unreadable attempt authority is never reported as clean.
    pub fn has_open_attempts(&self, context: &RequestContext) -> Result<bool, ApplicationProblem> {
        let authority = work_authority(context)?;
        self.has_open_attempts_for_authority(&authority)
    }

    pub fn has_open_attempts_for_authority(
        &self,
        authority: &WorkAuthority,
    ) -> Result<bool, ApplicationProblem> {
        self.attempts
            .open_attempts(authority)
            .map(|attempts| !attempts.is_empty())
            .map_err(storage_problem)
    }

    /// Cleanup-only exact-scope census. Unlike ordinary Work reads this is
    /// deliberately independent of actor and policy lineage.
    pub fn has_open_attempts_in_exact_scope(
        &self,
        project_id: &tracedecay_domain::ProjectId,
        repository_id: &tracedecay_domain::RepositoryId,
        worktree_id: &tracedecay_domain::WorktreeId,
    ) -> Result<bool, ApplicationProblem> {
        self.attempts
            .has_open_attempts_in_exact_scope(project_id, repository_id, worktree_id)
            .map_err(storage_problem)
    }
}

fn capacity_query_problem() -> ApplicationProblem {
    invalid_problem(
        "application.work-attempt.invalid-capacity-query",
        "Capacity task identities must be strictly sorted, unique, and within the batch bound.",
    )
}
