use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::GenerationSymbolIndexV1;
use tracedecay_contracts::{
    CallableCodeOperationKind, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope, callable_code_operation,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, ContentDigest, FileOccurrenceId, LanguageId, ProjectId, RefId,
    RepositoryId, SanitizedCodeFileV1, SnapshotFileDispositionV1, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_graph_query::{
    CodeGraphReadAdmissionFuture, CodeGraphReadFreshnessV1, CodeGraphReadFuture,
    VerifiedCodeGraphRead,
};
use tracedecay_lsp::analyzer::adapters::{DiagnosticMode, LspAdapterDefinition};
use tracedecay_runtime_core::native_runtime::block_on;

use super::*;

const FAKE_ANALYZER: &str = r#"
import json
import sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])).decode("utf-8"))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
            "uri": message["params"]["textDocument"]["uri"],
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 8}},
                "severity": 1,
                "source": "fake-ls",
                "message": "fake type error",
            }],
        }})
"#;

fn scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.native-diagnostics").expect("project"),
        RepositoryId::new("repository.native-diagnostics").expect("repository"),
        WorktreeId::new("worktree.native-diagnostics").expect("worktree"),
        Some(RefId::new("refs/heads/native-diagnostics").expect("reference")),
    )
    .expect("scope")
}

struct FixtureAdmission;

impl CodeGraphReadAdmissionPort for FixtureAdmission {
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> CodeGraphReadAdmissionFuture<'a> {
        Box::pin(async move {
            let actor = ActorId::new("actor.native-diagnostics").expect("actor");
            let grant = CapabilityGrantSnapshot::new(
                CapabilityGrantId::new("grant.native-diagnostics").expect("grant"),
                1,
                ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
                actor.clone(),
                request.observed_at,
                UtcMicros(i64::MAX),
                scope(),
                BTreeSet::from([request.operation.capability_id().clone()]),
                BTreeSet::from([request.operation.use_case_id().clone()]),
                DisclosureClass::Evidence,
            )
            .expect("grant");
            Ok(RequestContext::new(
                actor,
                scope(),
                grant,
                request.request_id.clone(),
                request.deadline.clone(),
                request.cancellation.context(),
            )
            .expect("context"))
        })
    }
}

struct FixtureProjection(Arc<CodeGraphProjectionStore>);

impl CodeGraphProjectionReadPort for FixtureProjection {
    fn open<'a>(&'a self, _request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        let store = Arc::clone(&self.0);
        Box::pin(async move {
            VerifiedCodeGraphRead::new(scope(), store, CodeGraphReadFreshnessV1::Current, None)
        })
    }
}

fn graph_with_file(logical_path: &str) -> Arc<CodeGraphProjectionStore> {
    let cancellation = CancellationSignal::active("cancel.native-diagnostics").expect("signal");
    let projection = HermeticCodeGraphProjectionStore::memory(&cancellation).expect("projection");
    let generation = CodeGenerationId::new("generation.native-diagnostics.1").expect("generation");
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: FileOccurrenceId::new(format!("file:native:{logical_path}"))
            .expect("file occurrence"),
        logical_path: logical_path.to_owned(),
        language: Some(LanguageId::new("rust").expect("language")),
        content_digest: ContentDigest::new(format!("sha256:{}", "b".repeat(64)))
            .expect("content digest"),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let symbols = GenerationSymbolIndexV1::new(generation.clone(), Vec::new()).expect("symbols");
    projection
        .publish_indexed_with_cancellation(
            &generation,
            &[],
            &[],
            &[file],
            &symbols,
            Arc::new(NeverCancelled),
        )
        .expect("publish");
    Arc::new(projection.verified_store(&generation).expect("store"))
}

fn python() -> &'static str {
    if std::path::Path::new("/usr/bin/python3").is_file() {
        "/usr/bin/python3"
    } else {
        "python3"
    }
}

#[test]
fn language_refresh_settles_diagnostics_without_a_tokio_runtime() {
    let project = tempfile::tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join("src")).expect("source dir");
    std::fs::write(project.path().join("src/lib.fake"), "let nope\n").expect("source");
    let analyzer = project.path().join("fake_analyzer.py");
    std::fs::write(&analyzer, FAKE_ANALYZER).expect("analyzer script");
    let settings = CodeDiagnosticsSettings {
        custom_adapters: vec![LspAdapterDefinition {
            language: "fake".to_owned(),
            language_id: "fake".to_owned(),
            command: python().to_owned(),
            args: vec![analyzer.display().to_string()],
            extensions: vec!["fake".to_owned()],
            root_markers: Vec::new(),
            install_options: Vec::new(),
            diagnostics: DiagnosticMode::Push,
        }],
        ..CodeDiagnosticsSettings::default()
    };
    let authority = DashboardDiagnosticsAuthorityV1::new(
        project.path().to_path_buf(),
        project.path().to_path_buf(),
        Arc::new(FixtureAdmission),
        Arc::new(FixtureProjection(graph_with_file("src/lib.fake"))),
        Arc::new(Mutex::new(diagnostic_broker(
            project.path().to_path_buf(),
            settings,
        ))),
    );
    let request = DashboardDiagnosticsGraphRequestV1::new(
        callable_code_operation(CallableCodeOperationKind::SourceMetadata).expect("operation"),
        RequestId::new("request.native-diagnostics").expect("request"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationSignal::active("cancel.native-diagnostics.request").expect("signal"),
        tracedecay_contracts::now_micros(),
    );

    assert!(tokio::runtime::Handle::try_current().is_err());
    let snapshot = block_on(authority.refresh_language(&request, "fake"))
        .expect("native runtime")
        .expect("the refresh settles off Tokio");

    let reported = snapshot
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.file.as_str(), diagnostic.message.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(reported, vec![("src/lib.fake", "fake type error")]);
}
