use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use tracedecay_domain::test_fixtures::{digest, fixture_abs_root, id};
use tracedecay_domain::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ProjectId, ProposalId, ProviderId,
    RepositoryId, UtcMicros, WorkApprovalPolicy, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkCancellationStateV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1,
    WorkFilesystemPolicy, WorkGraphVersionV1, WorkLeaseFenceV1, WorkLeaseId, WorkPlacementV1,
    WorkProductEventSequenceV1, WorkProductGraphV1, WorkProductSourceWatermarkV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkSandboxPolicy, WorkTerminalEvidenceV1, WorktreeId,
};

use super::*;

fn request() -> Value {
    let task = |key: &str| {
        json!({
            "key": key,
            "title": format!("Implement {key}"),
            "instructions": format!("Implement the {key} variant"),
            "placement": {
                "kind": "linked_worktree",
                "root": fixture_abs_root(&format!("/work/{key}")),
                "network_free": true,
                "in_place_acknowledged": false,
            },
        })
    };
    json!({
        "job": "rate-limit",
        "title": "Fix the rate limiter",
        "base": {
            "reference": "refs/heads/main",
            "commit": "0123456789abcdef0123456789abcdef01234567",
        },
        "effect_state": "observational",
        "tasks": [task("alpha"), task("beta")],
        "synthesis": {
            "task": "alpha",
            "output_name": "output.rate-limit",
            "instructions": "Pick the better variant",
        },
    })
}

fn parse(request: Value) -> Result<WorkJobV1, ApplicationProblem> {
    WorkJobV1::parse(serde_json::from_value(request).expect("job request decodes"))
}

fn job() -> WorkJobV1 {
    parse(request()).expect("job parses")
}

#[derive(Clone, Copy)]
enum Item {
    Created,
    Accepted,
    Admitted,
}

fn graph(job: &WorkJobV1, items: &[(&str, Item)]) -> WorkProductGraphV1 {
    let hierarchy = serde_json::to_value(job.hierarchy()).expect("hierarchy JSON");
    let items = items
        .iter()
        .map(|(key, stage)| {
            let task = job.task(&WorkJobKeyV1::new(*key).unwrap()).unwrap();
            let (proposal, route, admitted) = match stage {
                Item::Created => (Value::Null, Value::Null, Value::Null),
                Item::Accepted => (
                    json!(task.proposal_id),
                    json!({ "decision": "abstained", "reason": "fixture" }),
                    Value::Null,
                ),
                Item::Admitted => (
                    json!(task.proposal_id),
                    json!({ "decision": "abstained", "reason": "fixture" }),
                    json!(3),
                ),
            };
            json!({
                "input": {
                    "task_id": task.task_id,
                    "hierarchy": hierarchy,
                    "title": task.title,
                    "dependencies": [],
                    "informational_relations": [],
                    "causal_candidates": [],
                    "acceptance_criteria": [],
                    "effort": 1,
                    "scheduled_at": null,
                    "deadline": null,
                    "created_at": 1,
                    "updated_at": 5,
                },
                "accepted_proposal": proposal,
                "accepted_route": route,
                "execution_admitted_at": admitted,
                "evidence_links": [],
                "accepted_criteria": {},
                "accepted_attempts": [],
                "handoffs": [],
                "accepted_at": null,
                "archived_at": null,
            })
        })
        .collect::<Vec<_>>();
    serde_json::from_value(json!({
        "version": 4,
        "initiatives": [{ "id": hierarchy["initiative_id"], "title": job.title(), "created_at": 1 }],
        "plans": [{
            "id": hierarchy["plan_id"],
            "initiative_id": hierarchy["initiative_id"],
            "title": job.title(),
            "created_at": 1,
        }],
        "milestones": [{
            "id": hierarchy["milestone_id"],
            "plan_id": hierarchy["plan_id"],
            "title": job.title(),
            "created_at": 1,
        }],
        "items": items,
        "proposal_decisions": [],
        "relation_replan_decisions": [],
        "evidence": [],
    }))
    .expect("fixture graph is valid")
}

fn placement(task: &WorkJobTaskPlanV1, state: &str) -> WorkPlacementV1 {
    let (version, blockers) = match state {
        "admitted" => (1, json!([])),
        "released" => (2, json!([])),
        _ => (2, json!(["dirty_tracked_files"])),
    };
    serde_json::from_value(json!({
        "identity": { "task_id": task.task_id, "run_id": task.run_id },
        "target": task.placement,
        "state": state,
        "authority_version": version,
        "transitioned_at": 2,
        "blockers": blockers,
        "retention_eligible_at": null,
    }))
    .expect("fixture placement is valid")
}

fn route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.job.v1"),
    )
    .unwrap()
}

fn snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.job.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.job.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new("executable.job".to_owned(), digest('e')).unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(1_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn attempt(
    job: &WorkJobV1,
    identity: &WorkAttemptIdentityV1,
    instructions: &str,
    state: WorkAttemptStateV1,
) -> WorkAttemptV1 {
    let binding = WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(3).unwrap(),
        WorkProductEventSequenceV1::new(7).unwrap(),
        WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
        digest('f'),
        id::<ProposalId>("proposal.job.fixture"),
    )
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        job.operation().clone(),
        snapshot(),
        id::<ProjectId>("project.job"),
        id::<RepositoryId>("repository.job"),
        id::<WorktreeId>("worktree.job"),
        fixture_abs_root("/work/primary"),
        job.base().reference.clone(),
        job.base().commit.clone(),
        instructions.to_owned(),
        1,
        job.effect_state(),
    )
    .unwrap();
    let leased = WorkAttemptV1::new(
        identity.clone(),
        binding,
        envelope,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.job"),
            WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        route(),
        None,
        None,
    )
    .unwrap();
    if state == WorkAttemptStateV1::Leased {
        return leased;
    }
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    let terminal = match state {
        WorkAttemptStateV1::Running => return running,
        WorkAttemptStateV1::Succeeded => {
            WorkTerminalEvidenceV1::succeeded(digest('a'), UtcMicros(9))
        }
        WorkAttemptStateV1::Failed => WorkTerminalEvidenceV1::failed(digest('b'), UtcMicros(9)),
        state => panic!("the job fixture does not build {state:?} attempts"),
    }
    .unwrap();
    running
        .transition(
            state,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(route()),
            Some(terminal),
            running.lease().clone(),
        )
        .unwrap()
}

fn key(value: &str) -> WorkJobKeyV1 {
    WorkJobKeyV1::new(value).unwrap()
}

fn source(job: &WorkJobV1, task: &str, state: WorkAttemptStateV1) -> (WorkJobKeyV1, WorkAttemptV1) {
    let plan = job.task(&key(task)).unwrap();
    (
        key(task),
        attempt(job, &plan.source, &plan.instructions, state),
    )
}

fn synthesis(job: &WorkJobV1, state: WorkAttemptStateV1) -> WorkAttemptV1 {
    attempt(
        job,
        &job.synthesis().identity,
        &job.synthesis().instructions,
        state,
    )
}

fn placements(job: &WorkJobV1, states: &[(&str, &str)]) -> BTreeMap<WorkJobKeyV1, WorkPlacementV1> {
    states
        .iter()
        .map(|(task, state)| (key(task), placement(job.task(&key(task)).unwrap(), state)))
        .collect()
}

/// The comparable shape of a planner answer: which step, which attempts it
/// waits on, which refusal code, or which attempts it completed with.
#[derive(Debug, PartialEq, Eq)]
enum Expect {
    Act(WorkJobStepV1),
    Wait(Vec<(String, WorkAttemptStateV1)>),
    Refuse(WorkJobStepV1, &'static str),
    Complete(Vec<String>, String),
}

fn outcome(next: WorkJobNextV1) -> Expect {
    match next {
        WorkJobNextV1::Act(step) => Expect::Act(step),
        WorkJobNextV1::Wait(waits) => Expect::Wait(
            waits
                .into_iter()
                .map(|wait| match wait {
                    WorkJobWaitV1::Attempt { identity, state } => {
                        (identity.attempt_id().as_str().to_owned(), state)
                    }
                    wait => panic!("the planner never waits on {wait:?}"),
                })
                .collect(),
        ),
        WorkJobNextV1::Refuse(step, problem) => {
            let code = problem
                .diagnostic()
                .map(|diagnostic| diagnostic.code.clone());
            let code = match code.as_deref() {
                Some(WORK_JOB_CONFLICT_CODE) => WORK_JOB_CONFLICT_CODE,
                Some(WORK_JOB_NEEDS_OPERATOR_CODE) => WORK_JOB_NEEDS_OPERATOR_CODE,
                other => panic!("unexpected planner refusal code {other:?}"),
            };
            Expect::Refuse(step, code)
        }
        WorkJobNextV1::Complete { sources, synthesis } => Expect::Complete(
            sources
                .iter()
                .map(|attempt| attempt.identity().attempt_id().as_str().to_owned())
                .collect(),
            synthesis.identity().attempt_id().as_str().to_owned(),
        ),
    }
}

fn step(name: &str, task: &str) -> WorkJobStepV1 {
    let task = key(task);
    match name {
        "create" => WorkJobStepV1::CreateTask { task },
        "accept" => WorkJobStepV1::AcceptProposal { task },
        "admit" => WorkJobStepV1::AdmitExecution { task },
        "place" => WorkJobStepV1::AdmitPlacement { task },
        "start" => WorkJobStepV1::StartSource { task },
        "release" => WorkJobStepV1::ReleasePlacement { task },
        name => panic!("unknown fixture step {name}"),
    }
}

#[test]
fn the_planner_walks_the_job_stage_major_and_converges() {
    use Item::{Accepted, Admitted, Created};
    use WorkAttemptStateV1::{Running, Succeeded};
    let job = job();
    let both_admitted = || Some(graph(&job, &[("alpha", Admitted), ("beta", Admitted)]));
    let observation =
        |graph, placed: &[(&str, &str)], sources: Vec<_>, synthesis| WorkJobObservationV1 {
            graph,
            placements: placements(&job, placed),
            sources: sources.into_iter().collect(),
            synthesis,
        };
    let placed = [("alpha", "admitted"), ("beta", "admitted")];
    let cases = [
        (
            "nothing exists",
            observation(None, &[], vec![], None),
            Expect::Act(step("create", "alpha")),
        ),
        (
            "every create precedes any acceptance",
            observation(Some(graph(&job, &[("alpha", Created)])), &[], vec![], None),
            Expect::Act(step("create", "beta")),
        ),
        (
            "acceptance follows creation",
            observation(
                Some(graph(&job, &[("alpha", Created), ("beta", Created)])),
                &[],
                vec![],
                None,
            ),
            Expect::Act(step("accept", "alpha")),
        ),
        (
            "admission follows every acceptance",
            observation(
                Some(graph(&job, &[("alpha", Accepted), ("beta", Accepted)])),
                &[],
                vec![],
                None,
            ),
            Expect::Act(step("admit", "alpha")),
        ),
        (
            "an admitted task is not admitted again",
            observation(
                Some(graph(&job, &[("alpha", Admitted), ("beta", Accepted)])),
                &[],
                vec![],
                None,
            ),
            Expect::Act(step("admit", "beta")),
        ),
        (
            "placement follows every admission",
            observation(both_admitted(), &[("alpha", "admitted")], vec![], None),
            Expect::Act(step("place", "beta")),
        ),
        (
            "sources start once every run is placed",
            observation(both_admitted(), &placed, vec![], None),
            Expect::Act(step("start", "alpha")),
        ),
        (
            "a started source is not started again",
            observation(
                both_admitted(),
                &placed,
                vec![source(&job, "alpha", Running)],
                None,
            ),
            Expect::Act(step("start", "beta")),
        ),
        (
            "live sources are waited on, not synthesized",
            observation(
                both_admitted(),
                &placed,
                vec![
                    source(&job, "alpha", Running),
                    source(&job, "beta", Succeeded),
                ],
                None,
            ),
            Expect::Wait(vec![(
                "attempt.rate-limit.alpha.source".to_owned(),
                Running,
            )]),
        ),
        (
            "settled sources are synthesized",
            observation(
                both_admitted(),
                &placed,
                vec![
                    source(&job, "alpha", Succeeded),
                    source(&job, "beta", Succeeded),
                ],
                None,
            ),
            Expect::Act(WorkJobStepV1::Synthesize),
        ),
        (
            "a live synthesis is waited on",
            observation(
                both_admitted(),
                &placed,
                vec![
                    source(&job, "alpha", Succeeded),
                    source(&job, "beta", Succeeded),
                ],
                Some(synthesis(&job, Running)),
            ),
            Expect::Wait(vec![(
                "attempt.rate-limit.alpha.synthesis".to_owned(),
                Running,
            )]),
        ),
        (
            "a settled synthesis releases each placement",
            observation(
                both_admitted(),
                &[("alpha", "released"), ("beta", "admitted")],
                vec![
                    source(&job, "alpha", Succeeded),
                    source(&job, "beta", Succeeded),
                ],
                Some(synthesis(&job, Succeeded)),
            ),
            Expect::Act(step("release", "beta")),
        ),
        (
            "a released job is complete",
            observation(
                both_admitted(),
                &[("alpha", "released"), ("beta", "released")],
                vec![
                    source(&job, "beta", Succeeded),
                    source(&job, "alpha", Succeeded),
                ],
                Some(synthesis(&job, Succeeded)),
            ),
            Expect::Complete(
                vec![
                    "attempt.rate-limit.alpha.source".to_owned(),
                    "attempt.rate-limit.beta.source".to_owned(),
                ],
                "attempt.rate-limit.alpha.synthesis".to_owned(),
            ),
        ),
    ];
    for (label, seen, expected) in cases {
        assert_eq!(
            outcome(next_work_job_step(&job, &seen)),
            expected,
            "{label}"
        );
    }
}

#[test]
fn the_planner_refuses_objects_only_an_operator_or_a_new_job_can_move() {
    use Item::Admitted;
    use WorkAttemptStateV1::{Failed, Running, Succeeded};
    let job = job();
    let admitted = || Some(graph(&job, &[("alpha", Admitted), ("beta", Admitted)]));
    let placed = placements(&job, &[("alpha", "admitted"), ("beta", "admitted")]);
    let mut renamed = request();
    renamed["tasks"][1]["title"] = json!("A different title");
    let mut reworded = request();
    reworded["tasks"][0]["instructions"] = json!("Different instructions");
    let mut moved = request();
    moved["tasks"][1]["placement"]["root"] = json!(fixture_abs_root("/work/elsewhere"));
    let mut reproposed_graph = serde_json::to_value(graph(&job, &[("alpha", Admitted)])).unwrap();
    reproposed_graph["items"][0]["accepted_proposal"] = json!("proposal.someone-else");
    let reproposed: WorkProductGraphV1 = serde_json::from_value(reproposed_graph).unwrap();

    let cases = [
        (
            "a failed source needs an operator",
            job.clone(),
            WorkJobObservationV1 {
                graph: admitted(),
                placements: placed.clone(),
                sources: [source(&job, "alpha", Running), source(&job, "beta", Failed)].into(),
                synthesis: None,
            },
            Expect::Refuse(WorkJobStepV1::AwaitSources, WORK_JOB_NEEDS_OPERATOR_CODE),
        ),
        (
            "a failed synthesis needs an operator",
            job.clone(),
            WorkJobObservationV1 {
                graph: admitted(),
                placements: placed.clone(),
                sources: [
                    source(&job, "alpha", Succeeded),
                    source(&job, "beta", Succeeded),
                ]
                .into(),
                synthesis: Some(synthesis(&job, Failed)),
            },
            Expect::Refuse(WorkJobStepV1::AwaitSynthesis, WORK_JOB_NEEDS_OPERATOR_CODE),
        ),
        (
            "a quarantined placement needs an operator",
            job.clone(),
            WorkJobObservationV1 {
                graph: admitted(),
                placements: placements(&job, &[("alpha", "released"), ("beta", "quarantined")]),
                sources: [
                    source(&job, "alpha", Succeeded),
                    source(&job, "beta", Succeeded),
                ]
                .into(),
                synthesis: Some(synthesis(&job, Succeeded)),
            },
            Expect::Refuse(step("release", "beta"), WORK_JOB_NEEDS_OPERATOR_CODE),
        ),
        (
            "a task created under another title is a different job",
            parse(renamed).unwrap(),
            WorkJobObservationV1 {
                graph: admitted(),
                ..WorkJobObservationV1::default()
            },
            Expect::Refuse(step("create", "beta"), WORK_JOB_CONFLICT_CODE),
        ),
        (
            "a task that accepted another proposal is a different job",
            job.clone(),
            WorkJobObservationV1 {
                graph: Some(reproposed),
                ..WorkJobObservationV1::default()
            },
            Expect::Refuse(step("accept", "alpha"), WORK_JOB_CONFLICT_CODE),
        ),
        (
            "a run placed elsewhere is a different job",
            parse(moved).unwrap(),
            WorkJobObservationV1 {
                graph: admitted(),
                placements: placed.clone(),
                ..WorkJobObservationV1::default()
            },
            Expect::Refuse(step("place", "beta"), WORK_JOB_CONFLICT_CODE),
        ),
        (
            "a source started with other instructions is a different job",
            parse(reworded).unwrap(),
            WorkJobObservationV1 {
                graph: admitted(),
                placements: placed.clone(),
                sources: [source(&job, "alpha", Running)].into(),
                synthesis: None,
            },
            Expect::Refuse(step("start", "alpha"), WORK_JOB_CONFLICT_CODE),
        ),
    ];
    for (label, job, seen, expected) in cases {
        assert_eq!(
            outcome(next_work_job_step(&job, &seen)),
            expected,
            "{label}"
        );
    }
}

#[test]
fn a_malformed_job_is_refused_before_anything_runs() {
    let code = |request: Value| {
        parse(request)
            .expect_err("malformed job must be refused")
            .diagnostic()
            .map(|diagnostic| diagnostic.code.clone())
    };
    let mut duplicate = request();
    duplicate["tasks"][1]["key"] = json!("alpha");
    let mut orphan = request();
    orphan["synthesis"]["task"] = json!("gamma");
    let mut crowded = request();
    crowded["tasks"] = Value::Array(
        (0..=MAX_WORK_JOB_TASKS)
            .map(|index| {
                let mut task = request()["tasks"][0].clone();
                task["key"] = json!(format!("task-{index}"));
                task
            })
            .collect(),
    );
    let mut silent = request();
    silent["tasks"][0]["instructions"] = json!("");
    for malformed in [duplicate, orphan, crowded, silent] {
        assert_eq!(
            code(malformed),
            Some("application.work-guided.invalid-job".to_owned())
        );
    }

    let mut shouting = request();
    shouting["job"] = json!("Rate-Limit");
    assert!(
        serde_json::from_value::<AdvanceWorkJobRequestV1>(shouting).is_err(),
        "a job name outside the slug alphabet must not decode"
    );
}

#[test]
fn every_identity_derives_from_the_job_name_and_task_key() {
    let job = job();
    let beta = job.task(&key("beta")).unwrap();
    assert_eq!(beta.task_id.as_str(), "task.rate-limit.beta");
    assert_eq!(beta.run_id.as_str(), "run.rate-limit.beta");
    assert_eq!(beta.proposal_id.as_str(), "proposal.rate-limit.beta");
    assert_eq!(
        beta.source.attempt_id().as_str(),
        "attempt.rate-limit.beta.source"
    );
    let synthesis = &job.synthesis().identity;
    assert_eq!(synthesis.run_id().as_str(), "run.rate-limit.alpha");
    assert_eq!(
        synthesis.attempt_id().as_str(),
        "attempt.rate-limit.alpha.synthesis"
    );
    assert_eq!(
        step("start", "beta").request_id(job.name()),
        "guided.rate-limit.start_source.beta"
    );
    assert_eq!(
        WorkJobStepV1::Synthesize.request_id(job.name()),
        "guided.rate-limit.synthesize"
    );
}
