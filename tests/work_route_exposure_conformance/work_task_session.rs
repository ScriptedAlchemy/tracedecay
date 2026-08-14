//! Provider-qualified Work→TaskSession evidence on both published mounts.
//!
//! `work_evidence.rs` proves the TaskId-rooted read at its *root*: a freshly
//! created task with no accepted attempt, whose only truthful answer is zero
//! selected sources in every temporal mode. That is the floor, not the journey
//! a dashboard user actually walks: they open a task to find out **who worked
//! on it**, and the answer is a provider-qualified session relation published
//! by an accepted attempt.
//!
//! This module drives that journey to the same standard on both mounts. It
//! admits execution, runs one real pinned provider through the production
//! spawn path, links the accepted attempt, imports the provider transcript,
//! and then grades the TaskId-rooted read across all four temporal modes, on
//! the daemon mount and the dashboard mount, before and after a physical
//! daemon restart.
//!
//! Two honest boundaries are recorded rather than papered over:
//!
//! * TaskSession *hydration* is owned by the activated evaluated federated
//!   query authority (`DaemonWorkFederatedQueryAuthorityV1::authority_for`).
//!   This fixture activates no evaluated profile, so hydration is legitimately
//!   absent — and the point of grading it here is that absence stays a typed
//!   `unavailable` omission on both mounts rather than a fabricated empty
//!   session. The hydrated path, its exact continuation, and the rank-final
//!   `stale` revocation verdict are proven where that authority is real, in
//!   `tests/daemon_suite/advanced_workflow_journey/task_session.rs`.
//! * Because the same authority gate runs before the participant-epoch check,
//!   a continuation carrying a foreign epoch is refused here as `unavailable`,
//!   not as `stale`. That ordering is asserted explicitly so a future change
//!   that leaks a fabricated hydration past the gate fails loudly.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProviderId, RefId,
    WorkApprovalPolicy, WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference,
    WorkExecutionLimits, WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology,
    WorkFilesystemPolicy, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
    WorkProviderRouteV1, WorkSandboxPolicy, WorkflowOperationRef, safe_work_topology_policy_v1,
};

use super::{
    DashboardProcess, ProductionDaemon, assert_canonical_envelope, post_dashboard_envelope,
    post_envelope,
};

/// The task the parent journey already created through both mounts.
const TASK_ID: &str = "task.work-surface-conformance";
const RUN_ID: &str = "run.work-surface-task-session";
const ATTEMPT_ID: &str = "attempt.work-surface-task-session.1";
const EXECUTABLE_ID: &str = "executable.work-surface-task-session";
const BINDINGS_KEY: &str = "work.executable_bindings.v1";

/// The session id the pinned provider announces in its stream-json init frame.
/// The receipt must carry exactly this, qualified by the `claude` provider.
const PROVIDER_SESSION_ID: &str = "session.work-surface-task-session";
const PROVIDER_PROVIDER_ID: &str = "claude";

const INSTRUCTIONS: &str = "Execute the admitted Work surface task-session step.";
const POLL_BUDGET: Duration = Duration::from_secs(180);

/// Extends the TaskId-rooted read past its verified root.
///
/// `verified_version` is the graph identity the parent journey observed after
/// creating the task; every mutation below advances it, so the caller's pin is
/// deliberately *not* reused for the reads — a read that still answered under
/// the stale pin would be the bug.
pub(super) fn assert_provider_qualified_task_session_evidence(
    agent: &ureq::Agent,
    fixture: &mut ProductionDaemon,
) {
    let scripts = tempfile::tempdir().expect("provider script directory");

    // -- Pin one real provider through the production control plane. ---------
    // The resolver canonicalizes the path and re-digests the bytes at spawn
    // time, so this is the same fail-closed admission a shipped provider goes
    // through. The init frame is what makes the receipt provider-qualified.
    //
    // Provider bindings are a project-layer configuration write, and writing
    // one rebinds the per-project runtime. Production configures providers
    // first and then works, so this journey does too: pinning the binding
    // before any Work exists keeps the rebind out of the middle of a graph
    // mutation, where it would surface as an unauthorised graph read.
    let (executable_path, artifact_digest) = pinned_executable(
        scripts.path(),
        "task-session-provider",
        &format!(
            "#!/bin/sh\ncat > /dev/null\n\
             printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\
             \"session_id\":\"{session}\"}}'\n\
             printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":\
             [{{\"type\":\"text\",\"text\":\"work surface task-session evidence\"}}]}}}}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}}'\n\
             exit 0\n",
            session = PROVIDER_SESSION_ID,
        ),
    );
    let pinned = pin_executable_binding(agent, fixture, &executable_path, &artifact_digest);
    fixture.restart();

    // -- The task root itself, through the prepared-mutation handoff. --------
    let mut create_draft = super::product_task_create_draft();
    create_draft["selection"] = repository_selection(fixture);
    let (status, prepared) =
        super::poll_past_warming("daemon work/prepare-graph-mutation", &mut || {
            post_envelope(
                agent,
                &fixture.external_url("/application/work/prepare-graph-mutation"),
                fixture,
                &create_draft,
            )
        });
    assert_canonical_envelope("daemon work/prepare-graph-mutation", status, &prepared);
    let create_mutation = prepared["value"]["outcome"]["value"]["payload"].clone();
    payload(
        agent,
        fixture,
        "create task",
        "/application/work/mutate-graph",
        &create_mutation,
    );

    // -- Accept the proposal, then admit execution. --------------------------
    // Acceptance and admission are separate authorities; the attempt below is
    // refused outright until admission has happened, so walking both is the
    // only way to reach a real provider run.
    let proposal = payload(
        agent,
        fixture,
        "generate proposal",
        "/application/work/generate-proposal",
        &json!({
            "selection": repository_selection(fixture),
            "task_id": TASK_ID,
            "proposal_id": "proposal.work-surface-task-session",
            "live_git_evidence": Value::Null,
            "occurred_at": now_micros(),
        }),
    );
    mutate(
        agent,
        fixture,
        "accept proposal",
        json!({
            "change": "decide_proposal",
            "proposal": proposal["proposal"].clone(),
            "disposition": "accepted",
        }),
    );
    mutate(
        agent,
        fixture,
        "admit execution",
        json!({ "change": "admit_execution", "task_id": TASK_ID }),
    );

    // -- One real attempt, settled through the production spawn path. --------
    let start = json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": ATTEMPT_ID,
        "operation": typed::<WorkflowOperationRef>("operation.work-surface-task-session"),
        "execution_snapshot": execution_snapshot(&pinned, &artifact_digest),
        "worktree_root": fixture.project.to_string_lossy(),
        "reference": typed::<RefId>("refs/heads/work-surface-task-session"),
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "instructions": INSTRUCTIONS,
        "effect_state": WorkEffectStateV1::Observational,
        "occurred_at": now_micros(),
    });
    let leased = payload(
        agent,
        fixture,
        "start attempt",
        "/application/work/start-attempt",
        &start,
    );
    assert_eq!(leased["state"], "leased", "{leased}");

    let status_request = json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": ATTEMPT_ID,
    });
    let settled = poll_until("the provider attempt terminal receipt", || {
        let attempt = payload(
            agent,
            fixture,
            "attempt status",
            "/application/work/attempt-status",
            &status_request,
        );
        match attempt["state"].as_str() {
            Some("succeeded") => Ok(attempt),
            Some("leased" | "running") => Err(format!("{attempt}")),
            _ => panic!("the provider attempt reached an unexpected state: {attempt}"),
        }
    });
    assert_eq!(settled["terminal"]["outcome"], "succeeded", "{settled}");

    // The provider-qualified session relation is published by the accepted
    // attempt, so the link is the step that makes "who worked on this task"
    // answerable at all.
    let identity = json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": ATTEMPT_ID,
    });
    // The accepted-attempt relation is what makes "who worked on this task"
    // answerable, so the journey asserts it exists rather than assuming which
    // authority published it. A settled attempt may already have been linked
    // by the runtime; if it has not, the explicit product command is the only
    // other way to get there, and either path must end at the same relation.
    let graph = current_graph(agent, fixture);
    let verified_version = if accepted_attempts(&graph, &identity) {
        graph["snapshot"]["verified_version"].clone()
    } else {
        let linked = mutate(
            agent,
            fixture,
            "link accepted attempt",
            json!({
                "change": "link_accepted_attempt",
                "task_id": TASK_ID,
                "identity": identity.clone(),
            }),
        );
        let graph = current_graph(agent, fixture);
        assert!(
            accepted_attempts(&graph, &identity),
            "linking must publish the accepted-attempt relation: {linked} {graph}"
        );
        graph["snapshot"]["verified_version"].clone()
    };
    assert!(
        verified_version["graph_version"].is_number(),
        "the reads must pin the exact published graph identity: {verified_version}"
    );

    // -- Import the provider transcript. -------------------------------------
    // Without this the session named on the receipt would not exist at all,
    // and the `unavailable` verdict below could be read as "no such session"
    // rather than "no activated evaluated query authority". Importing it first
    // makes the omission unambiguously about the missing authority.
    write_provider_transcript(fixture.home.path(), &fixture.project);
    super::run_ok(
        super::isolated(fixture.home.path(), &fixture.profile)
            .args(["sessions", "import", "--project-path"])
            .arg(&fixture.project)
            .current_dir(&fixture.project),
        "tracedecay sessions import",
    );

    // -- Grade the read on both mounts, in every temporal mode. --------------
    // `tracedecay dashboard` is a launcher, not a server: it asks the daemon to
    // host the dashboard, prints the bound URL, and exits. The server therefore
    // lives inside the daemon, so it is started here — against the daemon that
    // is actually serving — rather than at test start against a predecessor
    // whose in-process server died with it.
    let selection = repository_selection(fixture);
    let dashboard = DashboardProcess::start(fixture);
    dashboard.wait_until_serving(agent, "the graded pass");
    assert_both_mounts(
        agent,
        fixture,
        Some(&dashboard),
        &selection,
        &verified_version,
        &identity,
        "before restart",
    );

    // -- Physical daemon restart. --------------------------------------------
    // The accepted-attempt relation, the provider-qualified session on its
    // receipt, and the typed unavailability of hydration are all durable
    // facts; a restart that reconstructed any of them differently would be a
    // silent authority change.
    //
    // The dashboard is hosted inside the daemon, so the restart takes its
    // server down with it and the pre-restart URL is dead. Relaunching the
    // launcher against the *new* daemon is therefore part of what durability
    // means here: the browser-facing mount has to come back, on its own fresh
    // port, and answer the same graded reads. Grading only the daemon mount
    // would let a dashboard that never resumes pass unnoticed.
    drop(dashboard);
    fixture.restart();
    let dashboard = DashboardProcess::start(fixture);
    dashboard.wait_until_serving(agent, "the post-restart graded pass");
    assert_both_mounts(
        agent,
        fixture,
        Some(&dashboard),
        &selection,
        &verified_version,
        &identity,
        "after physical daemon restart",
    );
}

/// One graded pass over both mounts and all four temporal modes.
fn assert_both_mounts(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
    dashboard: Option<&DashboardProcess>,
    selection: &Value,
    verified_version: &Value,
    identity: &Value,
    phase: &str,
) {
    for (mode, temporal) in [
        ("current", json!({ "kind": "current" })),
        ("as-of", json!({ "kind": "as_of", "cutoff": now_micros() })),
        ("evolution", json!({ "kind": "evolution" })),
        ("forensic", json!({ "kind": "forensic" })),
    ] {
        // 1. The task root, unexpanded: who worked on this task.
        let rooted = evidence_request(
            selection.clone(),
            verified_version,
            temporal.clone(),
            None,
            None,
        );
        let payload = both_mounts(
            agent,
            fixture,
            dashboard,
            &rooted,
            &format!("{phase} {mode} rooted"),
        );
        assert_eq!(payload["task_id"], TASK_ID, "{payload}");
        assert_eq!(payload["verified_version"], *verified_version, "{payload}");
        let receipt = attempt_receipt(&payload, identity).unwrap_or_else(|| {
            panic!("{phase} {mode} must expose the accepted attempt receipt: {payload}")
        });
        // Decode through the canonical identity rather than reading the raw
        // fields: `provider` is elided on the wire exactly when it is the
        // built-in default, so a raw comparison would either fail on a
        // correctly-encoded default or pass on an unqualified session. The
        // decoder is what every typed client uses, and it is what must say
        // `claude`.
        let session: tracedecay_domain::ObservationSourceIdentityV1 =
            serde_json::from_value(receipt["evidence"]["provider_session"].clone()).unwrap_or_else(
                |error| panic!("{phase} {mode} provider session must decode: {error}; {payload}"),
            );
        assert_eq!(
            session.provider().as_str(),
            PROVIDER_PROVIDER_ID,
            "{phase} {mode} must qualify the session by its provider: {payload}"
        );
        assert_eq!(
            session.session_id().as_str(),
            PROVIDER_SESSION_ID,
            "{phase} {mode} must name the exact provider session the CLI announced: {payload}"
        );
        assert_task_session_unavailable(&payload, &format!("{phase} {mode} rooted"));

        // 2. The exact TaskSession expansion the dashboard evidence panel
        //    issues. Hydration is owned by an authority this fixture has not
        //    activated, so the only truthful answer is the same typed
        //    omission — never a fabricated empty session, and never a
        //    silently dropped relation.
        let expanded = evidence_request(
            selection.clone(),
            verified_version,
            temporal.clone(),
            Some(json!({ "kind": "task_session", "attempt": identity })),
            None,
        );
        let payload = both_mounts(
            agent,
            fixture,
            dashboard,
            &expanded,
            &format!("{phase} {mode} expanded"),
        );
        assert_task_session_unavailable(&payload, &format!("{phase} {mode} expanded"));
        assert!(
            payload["sources"].as_array().is_some_and(|sources| !sources
                .iter()
                .any(|source| source["kind"] == "task_session")),
            "{phase} {mode} must not publish a TaskSession source without its authority: {payload}"
        );
        assert!(
            payload["continuations"]
                .as_array()
                .is_some_and(|continuations| continuations.is_empty()),
            "{phase} {mode} must not mint a continuation it cannot honour: {payload}"
        );

        // 3. A continuation carrying an epoch no participant manifest ever
        //    produced. The authority gate runs before the epoch check, so the
        //    verdict here is `unavailable`, not the rank-final `stale`
        //    revocation. Asserting the exact reason pins that ordering: a
        //    change that reached the epoch comparison without an authority
        //    would have hydrated something it was never entitled to read.
        let revoked = evidence_request(
            selection.clone(),
            verified_version,
            temporal.clone(),
            Some(json!({ "kind": "task_session", "attempt": identity })),
            Some(json!({
                "kind": "task_session",
                "continuation": {
                    "verified_version": verified_version,
                    "attempt": identity,
                    "source": {
                        "provider": PROVIDER_PROVIDER_ID,
                        "session_id": PROVIDER_SESSION_ID,
                        "source_key": Value::Null,
                    },
                    "participant_epoch": format!("sha256:{}", "e".repeat(64)),
                    "temporal_cursor": Value::Null,
                    "ranking_cursor": Value::Null,
                },
            })),
        );
        let payload = both_mounts(
            agent,
            fixture,
            dashboard,
            &revoked,
            &format!("{phase} {mode} foreign epoch"),
        );
        assert_task_session_unavailable(&payload, &format!("{phase} {mode} foreign epoch"));
    }
}

/// Posts one request to both published mounts and returns the payload they
/// must agree on exactly. A drift on either side fails with a named side.
fn both_mounts(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
    dashboard: Option<&DashboardProcess>,
    request: &Value,
    label: &str,
) -> Value {
    // A daemon that has just restarted is still binding its project runtime,
    // and that window is a real production state the dashboard renders as
    // retryable warming. Every answer inside it must hold to the typed
    // contract, so it is polled through rather than slept past.
    let daemon_label = format!("daemon work/retrieve-evidence ({label})");
    let (status, body) = super::poll_past_warming(&daemon_label, &mut || {
        post_envelope(
            agent,
            &fixture.external_url("/application/work/retrieve-evidence"),
            fixture,
            request,
        )
    });
    assert_canonical_envelope(&daemon_label, status, &body);
    assert_eq!(
        body["value"]["outcome"]["outcome"], "evidence",
        "{daemon_label}: {body}"
    );
    let daemon_payload = body["value"]["outcome"]["value"]["payload"].clone();

    let Some(dashboard) = dashboard else {
        return daemon_payload;
    };
    let dashboard_label = format!("dashboard api/work/retrieve-evidence ({label})");
    let (status, body) = post_dashboard_envelope(
        agent,
        &format!("{}/api/work/retrieve-evidence", dashboard.base_url),
        request,
    );
    assert_canonical_envelope(&dashboard_label, status, &body);
    assert_eq!(
        body["value"]["outcome"]["outcome"], "evidence",
        "{dashboard_label}: {body}"
    );
    let dashboard_payload = body["value"]["outcome"]["value"]["payload"].clone();

    assert_eq!(
        daemon_payload, dashboard_payload,
        "both published mounts must answer the same Work payload for {label}"
    );
    daemon_payload
}

/// The typed absence of the evaluated federated query authority.
fn assert_task_session_unavailable(payload: &Value, label: &str) {
    let omissions = payload["omissions"]
        .as_array()
        .unwrap_or_else(|| panic!("{label} must carry its omission list: {payload}"));
    assert!(
        omissions.iter().any(|omission| {
            omission["relation"] == "task_session" && omission["reason"] == "unavailable"
        }),
        "{label} must keep a missing evaluated query authority typed: {payload}"
    );
}

fn attempt_receipt(payload: &Value, identity: &Value) -> Option<Value> {
    payload["sources"]
        .as_array()?
        .iter()
        .find(|source| {
            source["kind"] == "attempt_receipt" && source["receipt"]["identity"] == *identity
        })
        .map(|source| source["receipt"].clone())
}

fn evidence_request(
    selection: Value,
    verified_version: &Value,
    temporal: Value,
    expansion: Option<Value>,
    continuation: Option<Value>,
) -> Value {
    json!({
        "selection": selection,
        "task_id": TASK_ID,
        "verified_version": verified_version,
        "temporal": temporal,
        // One selected source per page, so any future continuation this read
        // learns to mint is exercised rather than hidden behind a large page.
        "page_size": 1,
        "expansion": expansion.unwrap_or(Value::Null),
        "continuation": continuation.unwrap_or(Value::Null),
        "observed_at": now_micros(),
    })
}

/// Prepares and commits one product mutation, returning the committed effect.
fn mutate(agent: &ureq::Agent, fixture: &ProductionDaemon, label: &str, change: Value) -> Value {
    let prepared = payload(
        agent,
        fixture,
        &format!("prepare {label}"),
        "/application/work/prepare-graph-mutation",
        &json!({
            "selection": repository_selection(fixture),
            "causation_event_id": Value::Null,
            "evidence": [],
            "change": change,
        }),
    );
    payload(
        agent,
        fixture,
        label,
        "/application/work/mutate-graph",
        &prepared,
    )
}

/// Posts one operation to the daemon mount and returns its success payload.
fn payload(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
    label: &str,
    route_path: &str,
    body: &Value,
) -> Value {
    let (status, answer) = post_envelope(agent, &fixture.external_url(route_path), fixture, body);
    assert_canonical_envelope(label, status, &answer);
    assert_eq!(
        answer["kind"], "success",
        "{label} must succeed on the daemon mount: {answer}"
    );
    answer["value"]["outcome"]["value"]["payload"].clone()
}

struct PinnedConfiguration {
    revision_id: String,
    snapshot_id: String,
    effective_behavior_digest: String,
    resolution_provenance_digest: String,
}

/// Writes the executable binding through the production control plane and
/// reads back the exact configuration identity the attempt must pin.
fn pin_executable_binding(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
    executable_path: &Path,
    artifact_digest: &ManifestDigest,
) -> PinnedConfiguration {
    let observed = payload(
        agent,
        fixture,
        "configuration observed state",
        "/application/configuration/configuration_observed_state",
        &json!({}),
    );
    let base_revision = observed
        .as_array()
        .and_then(|components| components.first())
        .and_then(|component| component["desired_revision_id"].as_str())
        .unwrap_or_else(|| panic!("configuration observed state: {observed}"))
        .to_owned();

    let set = payload(
        agent,
        fixture,
        "configuration set (work executable bindings)",
        "/application/configuration/configuration_set",
        &json!({
            "layer": { "kind": "project", "project_id": fixture.project_id },
            "key": BINDINGS_KEY,
            "value": {
                "kind": "work_executable_bindings",
                "value": [{
                    "executable": {
                        "executable_id": EXECUTABLE_ID,
                        "artifact_digest": artifact_digest,
                    },
                    "canonical_path": executable_path,
                    "capabilities": ["claude_code_stream_json"],
                }],
            },
            "expected_revision": base_revision,
            "idempotency_key": "configuration.idempotency.work-surface-task-session",
        }),
    );
    let resolved = payload(
        agent,
        fixture,
        "configuration get after pinning",
        "/application/configuration/configuration_get",
        &json!({ "key": BINDINGS_KEY }),
    );
    PinnedConfiguration {
        revision_id: set["result_revision_id"]
            .as_str()
            .expect("result revision id")
            .to_owned(),
        snapshot_id: resolved["snapshot_id"]
            .as_str()
            .expect("pinned snapshot id")
            .to_owned(),
        effective_behavior_digest: resolved["effective_behavior_digest"]
            .as_str()
            .expect("effective behavior digest")
            .to_owned(),
        resolution_provenance_digest: resolved["resolution_provenance_digest"]
            .as_str()
            .expect("resolution provenance digest")
            .to_owned(),
    }
}

fn execution_snapshot(
    configuration: &PinnedConfiguration,
    artifact_digest: &ManifestDigest,
) -> Value {
    let snapshot = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: typed::<ConfigurationRevisionId>(&configuration.revision_id),
        configuration_snapshot_id: typed::<ConfigurationSnapshotId>(&configuration.snapshot_id),
        effective_behavior_digest: ManifestDigest::new(
            configuration.effective_behavior_digest.clone(),
        )
        .expect("effective behavior digest"),
        resolution_provenance_digest: ManifestDigest::new(
            configuration.resolution_provenance_digest.clone(),
        )
        .expect("resolution provenance digest"),
        route: WorkProviderRouteV1::new(
            typed::<ProviderId>("provider.work.claude-code-cli"),
            typed::<WorkProviderRouteId>("route.work-surface-task-session.v1"),
        )
        .expect("provider route"),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "model.work-surface-task-session".to_owned(),
        executable: WorkExecutableReference::new(EXECUTABLE_ID.to_owned(), artifact_digest.clone())
            .expect("pinned executable reference"),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 65_536, 65_536, 65_536, 1)
            .expect("execution limits"),
        deadline: tracedecay_domain::UtcMicros(now_micros().saturating_add(600 * 1_000_000)),
        fallback: WorkFallbackTopology::Disabled,
        topology: safe_work_topology_policy_v1(),
    })
    .expect("valid execution snapshot");
    serde_json::to_value(snapshot).expect("execution snapshot encodes")
}

/// The transcript the provider session left behind, in the host layout the
/// importer reads.
fn write_provider_transcript(home: &Path, project: &Path) {
    let directory = home.join(".claude/projects/work-surface-task-session");
    std::fs::create_dir_all(&directory).expect("provider transcript directory");
    let cwd = project.to_string_lossy();
    let records = [
        json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": PROVIDER_SESSION_ID,
            "uuid": "work-surface-task-session-user",
            "timestamp": "2026-08-13T12:00:00.000Z",
            "message": {"role": "user", "content": INSTRUCTIONS},
        }),
        json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": PROVIDER_SESSION_ID,
            "uuid": "work-surface-task-session-assistant",
            "timestamp": "2026-08-13T12:00:01.000Z",
            "message": {
                "id": "message.work-surface-task-session",
                "role": "assistant",
                "model": "model.work-surface-task-session",
                "content": [{
                    "type": "text",
                    "text": "work surface task-session evidence",
                }],
            },
        }),
    ];
    let contents = records
        .into_iter()
        .map(|record| record.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        directory.join(format!("{PROVIDER_SESSION_ID}.jsonl")),
        format!("{contents}\n"),
    )
    .expect("provider transcript");
}

#[cfg(unix)]
fn pinned_executable(directory: &Path, name: &str, body: &str) -> (PathBuf, ManifestDigest) {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join(name);
    std::fs::write(&path, body).expect("fixture executable");
    let mut permissions = std::fs::metadata(&path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).expect("executable mode");
    let digest = ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(body.as_bytes()))
    ))
    .expect("sha256 digest");
    (
        path.canonicalize().expect("canonical executable path"),
        digest,
    )
}

/// The current product graph, as the dashboard Work workspace opens it.
fn current_graph(agent: &ureq::Agent, fixture: &ProductionDaemon) -> Value {
    let request = json!({
        "selection": repository_selection(fixture),
        "mode": { "mode": "current" },
        "continuation": Value::Null,
        "observed_at": now_micros(),
    });
    let graph = payload(
        agent,
        fixture,
        "current product graph",
        "/application/work/views",
        &request,
    );
    assert_eq!(graph["mode"], "current", "{graph}");
    graph
}

/// Whether the task carries the exact accepted-attempt relation.
fn accepted_attempts(graph: &Value, identity: &Value) -> bool {
    graph["snapshot"]["graph"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["input"]["task_id"] == TASK_ID)
        .and_then(|item| item["accepted_attempts"].as_array())
        .is_some_and(|attempts| attempts.iter().any(|attempt| attempt == identity))
}

/// The repository-relation selection this journey admits every Work event
/// under.
///
/// `profile_owned_no_git` cannot be used here: it covers exactly the events
/// that named no relation scope, and a settled provider attempt publishes
/// repository-scoped Work events. Reading a journal that mixes both under the
/// no-Git selection is refused outright — a partially folded graph is a
/// falsified graph — so the selection has to be the one the whole journey's
/// events are actually admitted under.
fn repository_selection(fixture: &ProductionDaemon) -> Value {
    let common_dir =
        tracedecay::worktree::git_common_dir(&fixture.project).expect("Git common directory");
    let repository_id = format!(
        "repository.daemon.{}",
        hex::encode(Sha256::digest(common_dir.to_string_lossy().as_bytes()))
    );
    json!({
        "selection": "relations",
        "relation_scopes": [{
            "kind": "repository",
            "project_id": fixture.project_id,
            "repository_id": repository_id,
        }],
    })
}

fn typed<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid typed identity")
}

fn now_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_micros(),
    )
    .expect("test clock fits")
}

fn poll_until<T>(label: &str, mut probe: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        match probe() {
            Ok(value) => return value,
            Err(detail) => assert!(
                Instant::now() < deadline,
                "timed out waiting for {label}; last answer: {detail}"
            ),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
