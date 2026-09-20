//! `tracedecay_stack_snapshot` as an MCP client sees it.
//!
//! The call is `tools/call` on the server the production project composition
//! mounts. Argument adaptation, the daemon invocation service, and the
//! project-open native-integration owner all run. Expected values are the
//! refs, epoch, and typed outcomes a caller observes.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tracedecay_code_index_runtime::resolved_scope_for_project;
use tracedecay_contracts::{
    AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, NativeIntegrationSelectionDeclarationV1,
    NativeIntegrationStackSnapshotSurfaceRequest, RequestContext, RequestId, ResolvedScope,
    native_integration_surface_operation,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
    UtcMicros, WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_mcp::McpTransport;
use tracedecay_runtime_core::git::try_git_program;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::daemon::ProductionProjectCompositionHarnessV1;
use crate::mcp::McpServer;

const SOURCE_REF: &str = "refs/heads/source";
const DESTINATION_REF: &str = "refs/heads/destination";
const INVENTORY_SNAPSHOT_ID: &str = "inventory.snapshot.proof";
const INVENTORY_EPOCH: u64 = 7;
const PROPOSAL_DIGEST_BYTE: char = 'c';

struct CaptureTransport {
    incoming: Option<String>,
    output: String,
}

impl McpTransport for CaptureTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(self.incoming.take())
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.output.push_str(line);
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

use tracedecay_domain::test_fixtures::digest;

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new(try_git_program().expect("git program"))
        .args(arguments)
        .current_dir(root)
        .status()
        .expect("git command");
    assert!(status.success(), "git {arguments:?} failed");
}

fn prepare_repository(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "stack-snapshot@example.com"],
    );
    git(root, &["config", "user.name", "Stack Snapshot"]);
    std::fs::write(root.join("README"), "base\n").expect("readme");
    git(root, &["add", "README"]);
    git(root, &["commit", "-m", "base"]);
    git(root, &["checkout", "-b", "source"]);
    std::fs::write(root.join("source.txt"), "source\n").expect("source file");
    git(root, &["add", "source.txt"]);
    git(root, &["commit", "-m", "source"]);
    git(root, &["checkout", "main"]);
    git(root, &["checkout", "-b", "destination"]);
    std::fs::write(root.join("destination.txt"), "destination\n").expect("destination file");
    git(root, &["add", "destination.txt"]);
    git(root, &["commit", "-m", "destination"]);
}

fn stack_snapshot_capability() -> (CapabilityId, UseCaseId) {
    let operation = native_integration_surface_operation(
        tracedecay_contracts::NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
    )
    .expect("stack snapshot operation")
    .expect("stack snapshot is declared");
    (
        operation.capability_id().clone(),
        operation.use_case_id().clone(),
    )
}

fn request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
    let (capability, use_case) = stack_snapshot_capability();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.stack-snapshot.{suffix}")).expect("grant id"),
        1,
        digest('a'),
        ActorId::new("actor.stack-snapshot.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(1_000_000_000),
        scope.clone(),
        std::collections::BTreeSet::from([capability.clone()]),
        std::collections::BTreeSet::from([use_case.clone()]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.stack-snapshot.requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.stack-snapshot.{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(1_000_000_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.stack-snapshot.{suffix}")).expect("cancel"),
    )
    .expect("request context")
}

fn authorized_scope_set(
    id: &str,
    source: ResolvedScope,
    destination: ResolvedScope,
) -> AuthorizedScopeSet {
    let (capability, use_case) = stack_snapshot_capability();
    AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new(id).expect("scope set id"),
        ScopeSetRevision::new(1).expect("scope set revision"),
        vec![
            request_context(destination, &format!("{id}.destination")),
            request_context(source, &format!("{id}.source")),
        ],
        &capability,
        &use_case,
        UtcMicros(100),
    )
    .expect("authorized scope set")
}

fn scope(
    project: &ProjectId,
    repository: &RepositoryId,
    worktree: WorktreeId,
    reference: &str,
) -> ResolvedScope {
    ResolvedScope::new(
        project.clone(),
        repository.clone(),
        worktree,
        Some(RefId::new(reference).expect("reference")),
    )
    .expect("resolved scope")
}

fn snapshot_arguments(
    source: &ResolvedScope,
    destination: &ResolvedScope,
    scope_set: &AuthorizedScopeSet,
    source_ref: &str,
    destination_ref: &str,
) -> Value {
    let request = NativeIntegrationStackSnapshotSurfaceRequest {
        source: source.clone(),
        destination: destination.clone(),
        authorized_scope_set_id: scope_set.scope_set_id().clone(),
        authorized_scope_set_revision: scope_set.revision(),
        authorized_scope_set_digest: scope_set.digest().clone(),
        inventory_snapshot_id: WorktreeInventorySnapshotId::new(INVENTORY_SNAPSHOT_ID)
            .expect("inventory snapshot"),
        inventory_epoch: WorktreeInventoryEpoch::new(INVENTORY_EPOCH).expect("inventory epoch"),
        selection: NativeIntegrationSelectionDeclarationV1::IndependentBranch {
            proposal_digest: digest(PROPOSAL_DIGEST_BYTE),
            source_ref: RefId::new(source_ref).expect("source ref"),
            destination_ref: RefId::new(destination_ref).expect("destination ref"),
        },
        grant_digest: digest('a'),
        policy_digest: digest('d'),
    };
    serde_json::to_value(request).expect("snapshot arguments")
}

fn persist_scope_set(server: &McpServer, scope_set: &AuthorizedScopeSet) {
    let database = server
        .project_session_db()
        .expect("production project session database");
    let storage = database
        .authorized_scope_set_storage()
        .expect("scope-set storage");
    let persisted = storage
        .compare_and_swap(None, scope_set)
        .expect("persist scope set");
    assert!(
        format!("{persisted:?}").starts_with("Applied"),
        "scope set was not stored: {persisted:?}"
    );
}

fn evidence_payload(response: &Value) -> Value {
    assert!(
        response["error"].is_null(),
        "stack snapshot call failed: {response}"
    );
    let text = response["result"]["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item.get("text")?.as_str()?;
                let start = text.find('{')?;
                Some(text[start..].to_owned())
            })
        })
        .unwrap_or_else(|| panic!("tool result has no JSON content: {response}"));
    let envelope: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("tool result is not JSON: {error}\n{text}"));
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("tool result has no evidence payload: {envelope}"))
}

async fn call_stack_snapshot(server: &McpServer, mut arguments: Value) -> Value {
    if let Some(object) = arguments.as_object_mut() {
        object
            .entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_stack_snapshot",
            "arguments": arguments,
        }
    });
    let mut transport = CaptureTransport {
        incoming: Some(request.to_string()),
        output: String::new(),
    };
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("production MCP server tools/call");
    serde_json::from_str(transport.output.trim()).expect("JSON-RPC response")
}

#[tokio::test(flavor = "multi_thread")]
async fn stack_snapshot_freezes_enrolled_refs_and_refuses_the_other_inputs() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let repository_root = directory.path().join("repo");
    std::fs::create_dir_all(&repository_root).expect("repository directory");
    prepare_repository(&repository_root);
    let repository_root = repository_root
        .canonicalize()
        .expect("canonical repository");

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        directory.path(),
        [repository_root.clone()],
    )
    .await
    .expect("production composition");
    let server = harness
        .server(&repository_root)
        .expect("mounted project server");
    let project_id = ProjectId::new(
        harness
            .project_id(&repository_root)
            .await
            .expect("enrolled project id"),
    )
    .expect("project id");
    let enrolled_scope = resolved_scope_for_project(&repository_root, &project_id)
        .expect("enrolled repository scope");
    let repository_id = enrolled_scope.repository_id.clone();
    let source_scope = scope(
        &project_id,
        &repository_id,
        WorktreeId::new("worktree.stack-snapshot.source").expect("source worktree"),
        SOURCE_REF,
    );
    let destination_scope = scope(
        &project_id,
        &repository_id,
        enrolled_scope.worktree_id.clone(),
        DESTINATION_REF,
    );
    let enrolled = authorized_scope_set(
        "scope-set.stack-snapshot.proof",
        source_scope.clone(),
        destination_scope.clone(),
    );
    let foreign_project = ProjectId::new("project.stack-snapshot.foreign").expect("foreign");
    let foreign_source = scope(
        &foreign_project,
        &repository_id,
        WorktreeId::new("worktree.stack-snapshot.source").expect("source worktree"),
        SOURCE_REF,
    );
    let foreign_destination = scope(
        &foreign_project,
        &repository_id,
        enrolled_scope.worktree_id.clone(),
        DESTINATION_REF,
    );
    let foreign = authorized_scope_set(
        "scope-set.stack-snapshot.foreign",
        foreign_source.clone(),
        foreign_destination.clone(),
    );
    persist_scope_set(&server, &enrolled);
    persist_scope_set(&server, &foreign);

    let proposal_digest = format!("sha256:{}", PROPOSAL_DIGEST_BYTE.to_string().repeat(64));
    let frozen = evidence_payload(
        &call_stack_snapshot(
            &server,
            snapshot_arguments(
                &source_scope,
                &destination_scope,
                &enrolled,
                SOURCE_REF,
                DESTINATION_REF,
            ),
        )
        .await,
    );
    assert_eq!(frozen["outcome"], "stack_snapshot");
    assert_eq!(frozen["selection"]["project_id"], project_id.as_str());
    assert_eq!(frozen["selection"]["repository_id"], repository_id.as_str());
    assert_eq!(frozen["selection"]["source_ref"], SOURCE_REF);
    assert_eq!(frozen["selection"]["destination_ref"], DESTINATION_REF);
    assert_eq!(frozen["selection"]["inventory_epoch"], INVENTORY_EPOCH);
    assert_ne!(
        frozen["selection"]["selection_digest"], proposal_digest,
        "the frozen selection digest must be the daemon's, not the proposal echoed back"
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["kind"],
        "independent_branch"
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["binding"]["source_ref"],
        SOURCE_REF
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["binding"]["destination_ref"],
        DESTINATION_REF
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["binding"]["proposal_digest"],
        proposal_digest
    );
    assert_eq!(
        frozen["sealed_snapshot"]["inventory_snapshot_id"],
        INVENTORY_SNAPSHOT_ID
    );
    assert_eq!(
        frozen["sealed_snapshot"]["inventory_epoch"],
        INVENTORY_EPOCH
    );

    let mut missing_ref = snapshot_arguments(
        &source_scope,
        &destination_scope,
        &enrolled,
        SOURCE_REF,
        DESTINATION_REF,
    );
    missing_ref["selection"]["binding"]["source_ref"] = json!("refs/heads/absent");
    let missing = evidence_payload(&call_stack_snapshot(&server, missing_ref).await);
    assert_eq!(missing["outcome"], "unavailable");
    assert_eq!(missing["reason"], "partial");

    let foreign_result = evidence_payload(
        &call_stack_snapshot(
            &server,
            snapshot_arguments(
                &foreign_source,
                &foreign_destination,
                &foreign,
                SOURCE_REF,
                DESTINATION_REF,
            ),
        )
        .await,
    );
    assert_eq!(foreign_result["outcome"], "unavailable");
    assert_eq!(foreign_result["reason"], "denied");

    let mut path_bearing = snapshot_arguments(
        &source_scope,
        &destination_scope,
        &enrolled,
        SOURCE_REF,
        DESTINATION_REF,
    );
    path_bearing["repository_path"] = json!("/tmp/not-a-repository");
    let rejected = call_stack_snapshot(&server, path_bearing).await;
    assert_eq!(
        rejected["error"]["data"]["reason_code"],
        "application_surface_invalid_request"
    );
    assert!(
        rejected.get("result").is_none() || rejected["result"].is_null(),
        "a path-bearing request must not return a frozen snapshot: {rejected}"
    );

    harness.shutdown().await;
}
