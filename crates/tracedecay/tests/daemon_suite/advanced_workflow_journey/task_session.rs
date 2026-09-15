//! Typed SDK proof for provider-qualified Work-to-TaskSession availability.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay_code_index_retention::code_index_generations::{
    DurablePublicationPointerV1, scoped_code_index_store_root,
};
use tracedecay_contracts::{
    VerifiedWorkGraphVersionV1, WorkAttemptReceiptV1, WorkEvidenceContinuationV1,
    WorkEvidenceExpansionSelectorV1, WorkEvidenceOmissionReasonV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1, WorkEvidenceSourceV1, WorkProductSelectionScopeV1,
    WorkTaskSessionEvidenceV1, WorkTaskSessionHydrationStateV1,
};
use tracedecay_domain::{
    ProjectId, RetrieverKind, TaskId, TemporalModeV1, UtcMicros, WorkAttemptIdentityV1,
};
use tracedecay_sdk::client::Client;
use tracedecay_sdk::operations::WorkRetrieveEvidence;

use super::{
    PROVIDER_SESSION_ID, advance_provider_transcript_participant_generation, common,
    daemon_fixture::{
        sdk_client, spawn_project_daemon, wait_for_application_mount, wait_for_work_mount,
    },
    now, seeded_provider_transcript_contents,
};

/// A daemon-hosted dashboard mounted against the journey's real project.
///
/// The launcher only publishes the browser-facing address; the server remains
/// owned by the daemon. Holding and draining its pipes keeps later dashboard
/// logs from breaking the mounted route with SIGPIPE while the retrieval
/// assertions are still running.
pub(super) struct DashboardProcess {
    process: Child,
    base_url: String,
    diagnostics: std::sync::Arc<std::sync::Mutex<String>>,
}

impl DashboardProcess {
    pub(super) fn start(home: &Path, project: &Path) -> Self {
        let mut process = common::tracedecay_command_with_home(home)
            .args(["dashboard", "--host", "127.0.0.1", "--port", "0"])
            .current_dir(project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start dashboard mount");
        let diagnostics = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(stderr) = process.stderr.take() {
            let sink = std::sync::Arc::clone(&diagnostics);
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};

                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                while matches!(reader.read_line(&mut line), Ok(read) if read > 0) {
                    if let Ok(mut sink) = sink.lock() {
                        sink.push_str(&line);
                    }
                    line.clear();
                }
            });
        }
        let stdout = process.stdout.take().expect("dashboard stdout");
        let base_url = read_listening_url(stdout, &mut process);
        let dashboard = Self {
            process,
            base_url,
            diagnostics,
        };
        dashboard.wait_until_serving();
        dashboard
    }

    fn wait_until_serving(&self) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if agent.get(&format!("{}/", self.base_url)).call().is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon-hosted dashboard at {} never accepted connections\nlauncher stderr:\n{}",
                self.base_url,
                self.diagnostics
                    .lock()
                    .map(|captured| captured.clone())
                    .unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn retrieve_evidence(&self, request: &WorkEvidenceRetrieveRequestV1) -> (u16, Value) {
        // The public dashboard request contract carries only evidence input.
        // Its cancellation signal is daemon-owned per HTTP request and cannot
        // be supplied here without inventing a test-only control surface.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        let url = format!("{}/api/work/retrieve-evidence", self.base_url);
        let mut response = agent
            .post(&url)
            .content_type("application/json")
            .send(
                serde_json::to_string(request)
                    .expect("encode canonical dashboard evidence request"),
            )
            .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|error| panic!("POST {url} body failed: {error}"));
        let body = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("POST {url} answered non-JSON `{text}`: {error}"));
        (status, body)
    }
}

impl Drop for DashboardProcess {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn read_listening_url(stdout: std::process::ChildStdout, process: &mut Child) -> String {
    use std::io::{BufRead, BufReader};

    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut seen = String::new();
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                seen.push_str(&line);
                if let Some(rest) = line.split_once("listening on ")
                    && let Some(url) = rest.1.split_whitespace().next()
                {
                    std::thread::spawn(move || {
                        let mut line = String::new();
                        while matches!(reader.read_line(&mut line), Ok(read) if read > 0) {
                            line.clear();
                        }
                    });
                    return url.trim_end_matches('/').to_owned();
                }
            }
            Err(error) => panic!("dashboard stdout failed: {error}; seen:\n{seen}"),
        }
    }
    let mut stderr = String::new();
    if let Some(mut piped) = process.stderr.take() {
        let _ = piped.read_to_string(&mut stderr);
    }
    panic!("dashboard never announced a listen URL\nstdout:\n{seen}\nstderr:\n{stderr}");
}

pub(super) fn seed_probe_source(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn advanced_workflow_probe() -> &'static str { \"provider session\" }\n",
    )
    .expect("probe fixture source");
}

pub(super) fn assert_restored_provider_session_receipt(
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &tracedecay_domain::TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
) -> WorkAttemptReceiptV1 {
    let mut restored_receipt = None;
    for temporal in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf { cutoff: now() },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let (receipt, evidence, omissions) = retrieve(
            client,
            selection,
            task_id,
            verified_version,
            identity,
            temporal,
        )
        .unwrap_or_else(|error| panic!("typed SDK retrieval failed in {temporal:?}: {error}"));
        let receipt = receipt.unwrap_or_else(|| {
            panic!("typed SDK omitted attempt receipt in {temporal:?}: omissions={omissions:?}")
        });
        assert!(
            evidence.is_some()
                || omissions.iter().any(|omission| {
                    omission.relation == "task_session"
                        && omission.reason == WorkEvidenceOmissionReasonV1::Unavailable
                }),
            "TaskSession is either hydrated through the mounted query authority or a typed \
             unavailable omission in {temporal:?}: {omissions:?}"
        );
        let provider_session = receipt
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.provider_session.as_ref())
            .expect("provider-qualified attempt receipt");
        assert_eq!(provider_session.provider().as_str(), "claude");
        assert_eq!(provider_session.session_id().as_str(), PROVIDER_SESSION_ID);
        if let Some(restored) = &restored_receipt {
            assert_eq!(
                restored, &receipt,
                "temporal modes must preserve the receipt"
            );
        } else {
            restored_receipt = Some(receipt);
        }
    }

    restored_receipt.expect("restored provider receipt")
}

/// Physically restarts the daemon and proves the accepted-attempt receipt
/// survives byte-exactly, then waits until the restored code generation and
/// the mounted exact/lexical/graph query authority serve TaskSession evidence.
pub(super) fn restart_and_wait_for_task_session(
    home: &Path,
    project: &Path,
    client: &Client,
    project_id: &ProjectId,
    mut daemon: common::DaemonProcess,
    scope: &TaskSessionEvidenceScope<'_>,
    expected_receipt: &WorkAttemptReceiptV1,
) -> (common::DaemonProcess, Client) {
    let receipt = assert_restored_provider_session_receipt(
        client,
        scope.selection,
        scope.task_id,
        scope.verified_version,
        scope.identity,
    );
    assert_eq!(
        receipt, *expected_receipt,
        "the accepted-attempt receipt must survive restart exactly"
    );

    daemon
        .kill_and_wait()
        .expect("physically restart daemon before the TaskSession availability sweep");
    let restarted_daemon = spawn_project_daemon(home, project);
    let restarted_client = sdk_client(home, project_id.as_str());
    let _ = wait_for_application_mount(&restarted_client);
    wait_for_work_mount(&restarted_client);
    let restarted_receipt = assert_restored_provider_session_receipt(
        &restarted_client,
        scope.selection,
        scope.task_id,
        scope.verified_version,
        scope.identity,
    );
    assert_eq!(
        restarted_receipt, *expected_receipt,
        "a second physical restart must preserve the receipt exactly"
    );
    wait_for_code_generation(home, project);
    wait_for_task_session_available(&restarted_client, scope);
    (restarted_daemon, restarted_client)
}

fn wait_for_code_generation(home: &Path, project: &Path) {
    let deadline = Instant::now() + Duration::from_secs(180);
    while read_active_code_generation(home, project).is_none() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the restored code generation"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The core query authority mounts after the first sealed generation is
/// seated, on a deferred owner. Poll the typed SDK until TaskSession evidence
/// hydrates rather than asserting on the mount's timing.
fn wait_for_task_session_available(client: &Client, scope: &TaskSessionEvidenceScope<'_>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let (_, evidence, omissions) = retrieve(
            client,
            scope.selection,
            scope.task_id,
            scope.verified_version,
            scope.identity,
            TemporalModeV1::Current,
        )
        .unwrap_or_else(|error| panic!("typed SDK retrieval failed while waiting: {error}"));
        if evidence.is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the mounted query authority to serve TaskSession: {omissions:?}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn read_active_code_generation(
    home: &Path,
    project: &Path,
) -> Option<tracedecay_code_index::production::CodeIndexPublishedGenerationV1> {
    let layout =
        tracedecay_runtime_core::storage::resolve_layout(project, &home.join(".tracedecay"))
            .ok()?;
    let scope = scoped_code_index_store_root(&layout.data_root.join("code-index-v1"), project);
    let pointer = serde_json::from_slice::<DurablePublicationPointerV1>(
        &std::fs::read(scope.join("active-code-generation-v1.json")).ok()?,
    )
    .ok()?;
    tracedecay_code_index::production::CodeIndexPublishedGenerationV1::decode_sealed(
        &std::fs::read(
            scope
                .join("code-generations-v1")
                .join(pointer.generation_file),
        )
        .ok()?,
    )
    .ok()
}

/// The exact Work evidence scope one TaskSession availability sweep reads:
/// the product selection, task, verified graph version, and attempt identity.
pub(super) struct TaskSessionEvidenceScope<'a> {
    pub(super) selection: &'a WorkProductSelectionScopeV1,
    pub(super) task_id: &'a TaskId,
    pub(super) verified_version: &'a VerifiedWorkGraphVersionV1,
    pub(super) identity: &'a WorkAttemptIdentityV1,
}

pub(super) fn assert_available_over_sdk_mcp_and_dashboard(
    home: &Path,
    project: &Path,
    client: &Client,
    dashboard: &DashboardProcess,
    scope: TaskSessionEvidenceScope<'_>,
) -> WorkTaskSessionEvidenceV1 {
    let TaskSessionEvidenceScope {
        selection,
        task_id,
        verified_version,
        identity,
    } = scope;
    let mut current = None;
    for temporal in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf { cutoff: now() },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let expansion = Some(WorkEvidenceExpansionSelectorV1::TaskSession {
            attempt: identity.clone(),
        });
        let first = retrieve_over_sdk_mcp_and_dashboard(
            home,
            project,
            client,
            dashboard,
            WorkEvidenceRetrieveRequestV1 {
                selection: selection.clone(),
                task_id: task_id.clone(),
                verified_version: verified_version.clone(),
                temporal,
                page_size: 1,
                expansion: expansion.clone(),
                continuation: None,
                observed_at: now(),
            },
        );
        let continuation = first
            .continuations
            .iter()
            .find_map(|continuation| match continuation {
                WorkEvidenceContinuationV1::TaskSession { continuation }
                    if continuation.attempt == *identity =>
                {
                    Some(continuation.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("{temporal:?} first page must expose an exact TaskSession continuation")
            });
        assert!(
            continuation.temporal_cursor.is_some(),
            "{temporal:?} first page must expose the exact temporal TaskSession continuation"
        );
        let (first_receipt, first_evidence, omissions) = evidence_for_attempt(first, identity);
        let first_receipt = first_receipt
            .unwrap_or_else(|| panic!("{temporal:?} first page omitted the attempt receipt"));
        assert_available(&omissions, temporal);
        let first_evidence = first_evidence
            .unwrap_or_else(|| panic!("{temporal:?} first page omitted TaskSession evidence"));
        assert_eq!(
            first_evidence.continuation,
            Some(continuation.as_ref().clone())
        );
        assert_eq!(first_evidence.ranked_anchors.len(), 1);
        assert_eq!(first_evidence.hydrated.len(), 1);
        let first_ranked = &first_evidence.ranked_anchors[0];
        let first_hydrated = &first_evidence.hydrated[0];
        assert_eq!(
            first_hydrated.state,
            WorkTaskSessionHydrationStateV1::Available
        );
        assert_eq!(
            first_hydrated.anchor_id, first_ranked.anchor_id,
            "{temporal:?} page one must hydrate the exact ranked anchor"
        );
        assert_eq!(
            first_hydrated.rank, first_ranked.final_ordinal,
            "{temporal:?} page one hydration rank must equal its ranked ordinal"
        );
        assert!(
            first_ranked
                .contributions
                .iter()
                .any(|contribution| contribution.retriever == RetrieverKind::TaskSession),
            "{temporal:?} page one must retain canonical TaskSession provenance: {first_ranked:?}"
        );
        assert_eq!(first_evidence.source.provider().as_str(), "claude");
        assert_eq!(
            first_evidence.source.session_id().as_str(),
            PROVIDER_SESSION_ID
        );

        let second = retrieve_over_sdk_mcp_and_dashboard(
            home,
            project,
            client,
            dashboard,
            WorkEvidenceRetrieveRequestV1 {
                selection: selection.clone(),
                task_id: task_id.clone(),
                verified_version: verified_version.clone(),
                temporal,
                page_size: 1,
                expansion,
                continuation: Some(WorkEvidenceContinuationV1::TaskSession {
                    continuation: continuation.clone(),
                }),
                observed_at: now(),
            },
        );
        let (second_receipt, second_evidence, omissions) = evidence_for_attempt(second, identity);
        assert_eq!(second_receipt.as_ref(), Some(&first_receipt));
        assert_available(&omissions, temporal);
        let second_evidence = second_evidence
            .unwrap_or_else(|| panic!("{temporal:?} continuation omitted TaskSession evidence"));
        assert_eq!(
            second_evidence.participant_epoch,
            first_evidence.participant_epoch
        );
        assert_eq!(second_evidence.source, first_evidence.source);
        assert_eq!(second_evidence.ranked_anchors.len(), 1);
        assert_eq!(second_evidence.hydrated.len(), 1);
        let second_ranked = &second_evidence.ranked_anchors[0];
        let second_hydrated = &second_evidence.hydrated[0];
        assert_eq!(
            second_hydrated.state,
            WorkTaskSessionHydrationStateV1::Available
        );
        assert_eq!(
            second_hydrated.anchor_id, second_ranked.anchor_id,
            "{temporal:?} continuation must hydrate the exact ranked anchor"
        );
        assert_eq!(
            second_hydrated.rank, second_ranked.final_ordinal,
            "{temporal:?} continuation hydration rank must equal its ranked ordinal"
        );
        assert!(
            second_ranked
                .contributions
                .iter()
                .any(|contribution| contribution.retriever == RetrieverKind::TaskSession),
            "{temporal:?} continuation must retain canonical TaskSession provenance: {second_ranked:?}"
        );
        assert_ne!(
            second_hydrated.anchor_id, first_hydrated.anchor_id,
            "{temporal:?} continuation repeated a hydrated TaskSession anchor"
        );
        assert_eq!(
            second_ranked.final_ordinal,
            first_ranked.final_ordinal + 1,
            "{temporal:?} continuation must advance by exactly one ranked TaskSession anchor"
        );
        let actual_contents = BTreeSet::from([
            first_hydrated
                .content
                .clone()
                .expect("page one seeded transcript content"),
            second_hydrated
                .content
                .clone()
                .expect("continuation seeded transcript content"),
        ]);
        let expected_contents = BTreeSet::from(seeded_provider_transcript_contents(identity));
        assert_eq!(
            actual_contents, expected_contents,
            "{temporal:?} ranked TaskSession pages must hydrate the exact seeded transcript messages"
        );
        if temporal == TemporalModeV1::Current {
            current = Some((first_evidence, continuation));
        }
    }

    let (current, initial_continuation) =
        current.expect("Current TaskSession evidence and continuation");
    assert_eq!(
        initial_continuation.participant_epoch, current.participant_epoch,
        "the initial continuation must carry the epoch returned by the activated evaluated query"
    );
    assert_eq!(
        current.ranked_anchors.len(),
        1,
        "page-one TaskSession ranking must complete before the participant roster changes"
    );
    assert_eq!(
        current.hydrated.len(),
        1,
        "page-one TaskSession hydration must complete before the participant roster changes"
    );
    let previous_participant_epoch = initial_continuation.participant_epoch;
    advance_provider_transcript_participant_generation(home, project, identity);
    let refreshed = retrieve_over_sdk_mcp_and_dashboard(
        home,
        project,
        client,
        dashboard,
        WorkEvidenceRetrieveRequestV1 {
            selection: selection.clone(),
            task_id: task_id.clone(),
            verified_version: verified_version.clone(),
            temporal: TemporalModeV1::Current,
            page_size: 1,
            expansion: Some(WorkEvidenceExpansionSelectorV1::TaskSession {
                attempt: identity.clone(),
            }),
            continuation: None,
            observed_at: now(),
        },
    );
    let (_refreshed_receipt, refreshed_evidence, omissions) =
        evidence_for_attempt(refreshed, identity);
    assert_available(&omissions, TemporalModeV1::Current);
    let refreshed_evidence =
        refreshed_evidence.expect("participant refresh must return TaskSession evidence");
    assert_ne!(
        refreshed_evidence.participant_epoch, current.participant_epoch,
        "the public transcript import must produce a newly frozen participant epoch"
    );
    let mut rank_final_continuation = refreshed_evidence
        .continuation
        .clone()
        .expect("the refreshed TaskSession page must expose its signed continuation");
    assert_eq!(
        rank_final_continuation.participant_epoch, refreshed_evidence.participant_epoch,
        "the refreshed continuation must carry the epoch produced by the public roster change"
    );
    let signed_temporal_cursor = rank_final_continuation.temporal_cursor.clone();
    let signed_ranking_cursor = rank_final_continuation.ranking_cursor.clone();
    assert!(
        signed_temporal_cursor.is_some(),
        "the refreshed page must retain its current signed temporal continuation"
    );
    rank_final_continuation.participant_epoch = previous_participant_epoch;
    assert_eq!(
        rank_final_continuation.temporal_cursor, signed_temporal_cursor,
        "the stale request must retain the current signed temporal continuation"
    );
    assert_eq!(
        rank_final_continuation.ranking_cursor, signed_ranking_cursor,
        "the stale request must retain the current signed ranking continuation"
    );

    let (status, revoked) = dashboard.retrieve_evidence(&WorkEvidenceRetrieveRequestV1 {
        selection: selection.clone(),
        task_id: task_id.clone(),
        verified_version: verified_version.clone(),
        temporal: TemporalModeV1::Current,
        page_size: 1,
        expansion: Some(WorkEvidenceExpansionSelectorV1::TaskSession {
            attempt: identity.clone(),
        }),
        continuation: Some(WorkEvidenceContinuationV1::TaskSession {
            continuation: Box::new(rank_final_continuation),
        }),
        observed_at: now(),
    });
    assert_eq!(status, 409, "rank-final participant revocation: {revoked}");
    assert_eq!(
        revoked["kind"], "problem",
        "rank-final participant revocation must use the canonical problem envelope: {revoked}"
    );
    assert_eq!(
        revoked["value"]["problem"]["kind"], "stale",
        "the previous real participant epoch must be stale after public transcript import: {revoked}"
    );
    assert_eq!(
        revoked["value"]["problem"]["retryable"], true,
        "rank-final participant revocation must tell the dashboard to restart its read: {revoked}"
    );
    current
}

fn assert_available(
    omissions: &[tracedecay_contracts::WorkEvidenceOmissionV1],
    temporal: TemporalModeV1,
) {
    assert!(
        !omissions
            .iter()
            .any(|omission| { omission.relation == "task_session" }),
        "the mounted query authority must not omit TaskSession in {temporal:?}: {omissions:?}"
    );
}

fn retrieve_over_sdk_mcp_and_dashboard(
    home: &Path,
    project: &Path,
    client: &Client,
    dashboard: &DashboardProcess,
    request: WorkEvidenceRetrieveRequestV1,
) -> WorkEvidenceRetrievalV1 {
    let sdk = client
        .execute::<WorkRetrieveEvidence>(&request)
        .unwrap_or_else(|error| panic!("typed SDK TaskSession retrieval failed: {error}"))
        .result;
    let mcp_envelope = serve_tool_call(
        home,
        project,
        "tracedecay_work_retrieve_evidence",
        serde_json::to_value(&request).expect("MCP Work evidence request"),
    );
    assert_eq!(
        mcp_envelope["kind"], "success",
        "MCP Work retrieval must return the canonical success envelope: {mcp_envelope}"
    );
    let mcp = serde_json::from_value::<WorkEvidenceRetrievalV1>(
        mcp_envelope["value"]["outcome"]["value"]["payload"].clone(),
    )
    .expect("canonical MCP Work evidence payload");
    assert_eq!(
        mcp, sdk,
        "typed SDK and real tracedecay serve must expose the same Work payload"
    );
    let (status, dashboard_envelope) = dashboard.retrieve_evidence(&request);
    assert_eq!(
        status, 200,
        "dashboard Work retrieval must be mounted and successful: {dashboard_envelope}"
    );
    assert_eq!(
        dashboard_envelope["kind"], "success",
        "dashboard Work retrieval must return the canonical success envelope: {dashboard_envelope}"
    );
    assert_eq!(
        dashboard_envelope["value"]["outcome"]["outcome"], "evidence",
        "dashboard Work retrieval must return evidence: {dashboard_envelope}"
    );
    let dashboard = serde_json::from_value::<WorkEvidenceRetrievalV1>(
        dashboard_envelope["value"]["outcome"]["value"]["payload"].clone(),
    )
    .expect("canonical dashboard Work evidence payload");
    assert_eq!(
        dashboard, sdk,
        "dashboard, SDK, and MCP must preserve the same TaskSession page"
    );
    sdk
}

fn serve_tool_call(home: &Path, project: &Path, tool_name: &str, arguments: Value) -> Value {
    let mut command = common::tracedecay_command_with_home(home);
    let child = command
        .args(["serve", "--path"])
        .arg(project)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tracedecay serve should start");
    let mut child = common::TestChildProcess::new(child);
    {
        let stdin = child.stdin_mut().expect("serve stdin");
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "advanced-workflow-journey", "version": "1"}
                }
            })
        )
        .expect("write MCP initialize");
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": tool_name, "arguments": arguments}
            })
        )
        .expect("write MCP tools/call");
    }
    let output = child
        .wait_with_output(Duration::from_secs(120))
        .expect("tracedecay serve should exit after stdin closes");
    assert!(
        output.status.success(),
        "tracedecay serve failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("MCP stdout UTF-8");
    let response = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response["id"] == 2)
        .unwrap_or_else(|| panic!("missing MCP tools/call response in stdout:\n{stdout}"));
    assert!(
        response.get("error").is_none(),
        "MCP tools/call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .expect("MCP tool content");
    content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| {
            let start = text.find('{').or_else(|| text.find('['))?;
            serde_json::from_str(&text[start..]).ok()
        })
        .unwrap_or_else(|| panic!("MCP tool response omitted JSON content: {response}"))
}

/// One attempt's receipt, TaskSession evidence, and omissions from a page.
type AttemptEvidencePage = (
    Option<WorkAttemptReceiptV1>,
    Option<WorkTaskSessionEvidenceV1>,
    Vec<tracedecay_contracts::WorkEvidenceOmissionV1>,
);

fn retrieve(
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &tracedecay_domain::TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
    temporal: TemporalModeV1,
) -> Result<AttemptEvidencePage, String> {
    let result = client
        .execute::<WorkRetrieveEvidence>(&WorkEvidenceRetrieveRequestV1 {
            selection: selection.clone(),
            task_id: task_id.clone(),
            verified_version: verified_version.clone(),
            temporal,
            page_size: 100,
            expansion: None,
            continuation: None,
            observed_at: UtcMicros(now().0),
        })
        .map_err(|error| error.to_string())?
        .result;
    Ok(evidence_for_attempt(result, identity))
}

fn evidence_for_attempt(
    result: WorkEvidenceRetrievalV1,
    identity: &WorkAttemptIdentityV1,
) -> (
    Option<WorkAttemptReceiptV1>,
    Option<WorkTaskSessionEvidenceV1>,
    Vec<tracedecay_contracts::WorkEvidenceOmissionV1>,
) {
    let omissions = result.omissions;
    let mut receipt = None;
    let mut task_session = None;
    for source in result.sources {
        match source {
            WorkEvidenceSourceV1::AttemptReceipt { receipt: candidate }
                if candidate.identity == *identity =>
            {
                receipt = Some(candidate);
            }
            WorkEvidenceSourceV1::TaskSession { attempt, evidence } if attempt == *identity => {
                task_session = Some(evidence)
            }
            _ => {}
        }
    }
    (receipt, task_session, omissions)
}
