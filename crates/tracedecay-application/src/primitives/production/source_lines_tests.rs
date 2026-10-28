use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_code_index::grep_search::MAX_INTERACTIVE_SOURCE_BYTES;
use tracedecay_contracts::git::GIT_HISTORICAL_BLOB_MAX_BYTES;
use tracedecay_contracts::retrieval::catalog::primitive_read_operation;
use tracedecay_contracts::retrieval::{
    ResultProjection, RetrievalPortContext, RetrievalPortOutcome, SourceLinesRequest,
    SourceLinesResult, SourceRetrievalPort,
};
use tracedecay_contracts::{
    ApplicationOperation, ApplicationProblemKind, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, CoverageCompleteness, Deadline, DisclosureClass, OmissionReason,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, FileOccurrenceId, GitOidV1, IndexPathPolicyV1, ManifestDigest,
    ProjectId, RepositoryId, SourceSpan, TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_graph_query::SourceReadContext;
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_runtime_core::git::{GitCommandBounds, bounded_git_output};

use super::{MAX_REVISION_SOURCE_BYTES, TraceDecaySourceLinesPortV1};
use crate::diagnostics_publication::{
    CodeIndexPublicationIdentityFuture, CodeIndexPublicationIdentityPortV1,
    CodeIndexPublicationIdentityV1,
};

struct UnavailableIndex;

impl CodeIndexPublicationIdentityPortV1 for UnavailableIndex {
    fn resolve(&self, _project_root: PathBuf) -> CodeIndexPublicationIdentityFuture<'_> {
        panic!("an exact historical source read must not consult the current index")
    }
}

struct StaticIndex(CodeIndexPublicationIdentityV1);

impl CodeIndexPublicationIdentityPortV1 for StaticIndex {
    fn resolve(&self, _project_root: PathBuf) -> CodeIndexPublicationIdentityFuture<'_> {
        Box::pin(async { Some(self.0.clone()) })
    }
}

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    port: TraceDecaySourceLinesPortV1,
    operation: ApplicationOperation,
    _database_authority: DatabaseAuthority,
}

impl Fixture {
    async fn new(exclude: &[&str], include: &[&str]) -> Self {
        crate::register_test_schema_installer();
        let directory = tempfile::tempdir().expect("isolated source fixture");
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).expect("repository root");
        git(&root, &["init", "-b", "main"]);
        let scope = ResolvedScope::new(
            ProjectId::new("project.source-lines").unwrap(),
            RepositoryId::new("repository.source-lines").unwrap(),
            WorktreeId::new("worktree.source-lines").unwrap(),
            None,
        )
        .unwrap();
        let database_path = directory.path().join("source.db");
        let database_authority =
            DatabaseAuthority::acquire_test(&database_path, "historical source fixture")
                .expect("database authority");
        let (database, _) = Database::publish_test_runtime(
            &database_path,
            &database_authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("isolated database");
        let source_runtime = Arc::new(SourceReadContext::new(
            root.clone(),
            database,
            true,
            scope.project_id.as_str().to_owned(),
            IndexPathPolicyV1::new(
                exclude.iter().map(|value| (*value).to_owned()).collect(),
                include.iter().map(|value| (*value).to_owned()).collect(),
            )
            .unwrap(),
        ));
        let port =
            TraceDecaySourceLinesPortV1::new(source_runtime, Arc::new(UnavailableIndex), scope)
                .expect("bound source port");
        Self {
            _directory: directory,
            root,
            port,
            operation: primitive_read_operation("source_lines").unwrap().unwrap(),
            _database_authority: database_authority,
        }
    }

    fn write(&self, file: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn commit(&self) -> GitOidV1 {
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-m", "source fixture"]);
        GitOidV1::new(git(&self.root, &["rev-parse", "HEAD"]).trim()).unwrap()
    }

    fn context(
        &self,
        scope: ResolvedScope,
        disclosure: DisclosureClass,
        allowed: bool,
    ) -> RequestContext {
        let now = tracedecay_contracts::now_micros();
        let expires_at = UtcMicros(now.0 + 60_000_000);
        let granted_operation = if allowed {
            self.operation.clone()
        } else {
            primitive_read_operation("code_symbol_search")
                .unwrap()
                .unwrap()
        };
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.source-lines").unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            ActorId::new("actor.source-lines.issuer").unwrap(),
            now,
            expires_at,
            scope.clone(),
            BTreeSet::from([granted_operation.capability_id().clone()]),
            BTreeSet::from([granted_operation.use_case_id().clone()]),
            disclosure,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.source-lines.requester").unwrap(),
            scope,
            grant,
            RequestId::new("request.source-lines").unwrap(),
            Deadline::new(expires_at).unwrap(),
            CancellationContext::active("cancel.source-lines").unwrap(),
        )
        .unwrap()
    }

    fn admitted(&self) -> RequestContext {
        self.context(self.port.scope.clone(), DisclosureClass::Evidence, true)
    }

    async fn read(
        &self,
        context: &RequestContext,
        revision: &GitOidV1,
        file: &str,
        start_line: u32,
        end_line: u32,
    ) -> RetrievalPortOutcome<SourceLinesResult> {
        self.port
            .source_lines(
                RetrievalPortContext {
                    request: context,
                    operation: &self.operation,
                },
                &SourceLinesRequest::Revision {
                    revision: revision.clone(),
                    file: file.to_owned(),
                    start_line,
                    end_line,
                },
            )
            .await
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let mut configured = vec![
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "maintenance.auto=false",
        "-c",
        "gc.auto=0",
    ];
    configured.extend_from_slice(args);
    let output = bounded_git_output(root, &configured, &GitCommandBounds::default()).unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn payload(outcome: RetrievalPortOutcome<SourceLinesResult>) -> serde_json::Value {
    assert!(matches!(
        outcome,
        RetrievalPortOutcome::Completed(_) | RetrievalPortOutcome::Partial(_)
    ));
    serde_json::to_value(outcome.evidence().payload.as_ref().unwrap()).unwrap()
}

#[test]
fn selectors_keep_indexed_identity_and_refuse_ambiguous_or_mutable_revisions() {
    let indexed = json!({
        "file": "file.source-lines",
        "span": {"start_byte":0,"end_byte":10},
        "meta": {
            "temporal":{"kind":"current"},
            "page":{"page_size":1,"cursor":null},
            "projection":"evidence", "order":"source_position"
        }
    });
    let request: SourceLinesRequest = serde_json::from_value(indexed.clone()).unwrap();
    assert!(
        matches!(&request, SourceLinesRequest::Indexed { meta, .. } if meta.projection == ResultProjection::Evidence)
    );
    request.validate().unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), indexed);

    let revision =
        json!({"revision":"a".repeat(40),"file":"src/space name.rs","start_line":1,"end_line":999});
    let request: SourceLinesRequest = serde_json::from_value(revision.clone()).unwrap();
    request.validate().unwrap();
    assert!(matches!(request, SourceLinesRequest::Revision { .. }));
    for value in ["HEAD", "main~1", "aaaaaaa", &"A".repeat(40)] {
        let mut invalid = revision.clone();
        invalid["revision"] = json!(value);
        assert!(serde_json::from_value::<SourceLinesRequest>(invalid).is_err());
    }
    let mut ambiguous = revision.clone();
    ambiguous["span"] = indexed["span"].clone();
    ambiguous["meta"] = indexed["meta"].clone();
    assert!(serde_json::from_value::<SourceLinesRequest>(ambiguous).is_err());
    for file in [
        "../outside",
        "/outside",
        "src//file",
        "src/./file",
        "src\\file",
        "src/\nfile",
    ] {
        let mut invalid = revision.clone();
        invalid["file"] = json!(file);
        let request: SourceLinesRequest = serde_json::from_value(invalid).unwrap();
        assert!(request.validate().is_err(), "{file:?}");
    }
    for (start, end) in [(0, 1), (3, 2)] {
        let mut invalid = revision.clone();
        invalid["start_line"] = json!(start);
        invalid["end_line"] = json!(end);
        assert!(
            serde_json::from_value::<SourceLinesRequest>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    for mode in [
        json!({"kind":"forensic"}),
        json!({"kind":"as_of","cutoff":1}),
    ] {
        let mut invalid = indexed.clone();
        invalid["meta"]["temporal"] = mode;
        assert!(
            serde_json::from_value::<SourceLinesRequest>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[tokio::test]
async fn indexed_source_preserves_disclosure_and_bounds_a_replaced_file() {
    let mut fixture = Fixture::new(&[], &[]).await;
    let source = "first\nsecond\n";
    let file = FileOccurrenceId::new("file.source-lines").unwrap();
    fixture.write("src/indexed.rs", source);
    fixture.port.code_index_identity = Arc::new(StaticIndex(CodeIndexPublicationIdentityV1::new(
        CodeGenerationId::new("generation.source-lines").unwrap(),
        fixture.port.scope.repository_id.clone(),
        Some(fixture.port.scope.worktree_id.clone()),
        None,
        None,
        [(
            "src/indexed.rs".to_owned(),
            file.clone(),
            tracedecay_code_index::intake::content_digest(source.as_bytes()),
        )],
    )));
    let context = fixture.admitted();
    let mut request: SourceLinesRequest = serde_json::from_value(json!({
        "file": file,
        "span": {"start_byte":0,"end_byte":5},
        "meta": {
            "temporal":{"kind":"current"},
            "page":{"page_size":1,"cursor":null},
            "projection":"evidence", "order":"source_position"
        }
    }))
    .unwrap();
    let port_context = RetrievalPortContext {
        request: &context,
        operation: &fixture.operation,
    };
    let full = payload(fixture.port.source_lines(port_context, &request).await);
    assert_eq!(full["body"], "first");
    assert_eq!(full["file"], "src/indexed.rs");
    if let SourceLinesRequest::Indexed { meta, .. } = &mut request {
        meta.projection = ResultProjection::ReferencesOnly;
    }
    let references = payload(fixture.port.source_lines(port_context, &request).await);
    assert!(references.get("body").is_none());
    assert!(references.get("file").is_none());
    assert_eq!(references["references"], full["references"]);
    fixture.write("src/indexed.rs", "changed\n");
    let stale = fixture.port.source_lines(port_context, &request).await;
    assert!(matches!(stale, RetrievalPortOutcome::Unavailable(_)));
    assert_eq!(stale.evidence().omissions[0].reason, OmissionReason::Stale);
    std::fs::File::create(fixture.root.join("src/indexed.rs"))
        .unwrap()
        .set_len(MAX_INTERACTIVE_SOURCE_BYTES + 1)
        .unwrap();
    let oversized = fixture.port.source_lines(port_context, &request).await;
    assert!(matches!(oversized, RetrievalPortOutcome::Unavailable(_)));
    assert_eq!(
        oversized.evidence().omissions[0].reason,
        OmissionReason::Budget
    );
    assert!(oversized.evidence().payload.is_none());
}

#[tokio::test]
async fn revision_source_reads_before_and_after_rename_without_index_or_worktree_fallback() {
    let fixture = Fixture::new(&[], &[]).await;
    let before = "first\r\nfn café() {\r\n    old();\r\n}\r\n";
    fixture.write("src/old name.rs", before);
    let base = fixture.commit();
    git(&fixture.root, &["mv", "src/old name.rs", "src/new name.rs"]);
    let after = "first\r\nfn café() {\r\n    new();\r\n}\r\n";
    fixture.write("src/new name.rs", after);
    let head = fixture.commit();
    fixture.write("src/new name.rs", "uncommitted replacement");
    let context = fixture.admitted();
    for (revision, file, source) in [
        (&base, "src/old name.rs", before),
        (&head, "src/new name.rs", after),
    ] {
        let outcome = fixture.read(&context, revision, file, 2, 3).await;
        assert!(matches!(outcome, RetrievalPortOutcome::Completed(_)));
        assert_eq!(
            outcome.evidence().temporal.requested_mode,
            TemporalModeV1::Forensic
        );
        let result = payload(outcome);
        let span: SourceSpan = serde_json::from_value(result["span"].clone()).unwrap();
        assert_eq!(result["revision"], revision.as_str());
        assert_eq!(result["file"], file);
        assert_eq!(result["start_line"], 2);
        assert_eq!(result["end_line"], 3);
        assert_eq!(
            result["body"],
            &source[span.start_byte as usize..span.end_byte as usize]
        );
        assert_eq!(
            result["blob_oid"],
            git(&fixture.root, &["rev-parse", &format!("{revision}:{file}")]).trim()
        );
        assert!(result.get("references").is_none());
        assert!(result.get("next_line").is_none());
    }
    let absent = fixture.read(&context, &head, "src/old name.rs", 1, 2).await;
    assert!(
        matches!(absent, RetrievalPortOutcome::Refused(_, ref problem) if problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized)
    );
    assert!(absent.evidence().payload.is_none());
}

#[tokio::test]
async fn revision_source_continues_at_exact_whole_line_boundaries() {
    let fixture = Fixture::new(&[], &[]).await;
    let lines = (1..=260)
        .map(|number| format!("é {number}\r\n"))
        .collect::<String>();
    fixture.write("many.rs", &lines);
    let wide = format!("{}\n", "λ".repeat(64)).repeat(100);
    fixture.write("wide.rs", &wide);
    let revision = fixture.commit();
    let context = fixture.admitted();
    for (file, source, expected_next) in [("many.rs", &lines, 201), ("wide.rs", &wide, 32)] {
        let first = fixture.read(&context, &revision, file, 1, 999).await;
        assert!(matches!(first, RetrievalPortOutcome::Partial(_)));
        assert_eq!(
            first.evidence().coverage.completeness,
            CoverageCompleteness::Partial
        );
        assert_eq!(first.evidence().omissions[0].reason, OmissionReason::Budget);
        let first = payload(first);
        assert_eq!(first["next_line"], expected_next);
        let mut reconstructed = first["body"].as_str().unwrap().to_owned();
        let mut next = expected_next;
        loop {
            let page = payload(fixture.read(&context, &revision, file, next, 999).await);
            let span: SourceSpan = serde_json::from_value(page["span"].clone()).unwrap();
            assert_eq!(span.start_byte as usize, reconstructed.len());
            let body = page["body"].as_str().unwrap();
            assert!(body.len() <= MAX_REVISION_SOURCE_BYTES);
            assert_eq!(
                body,
                &source[span.start_byte as usize..span.end_byte as usize]
            );
            reconstructed.push_str(body);
            let Some(line) = page.get("next_line") else {
                break;
            };
            next = u32::try_from(line.as_u64().unwrap()).unwrap();
        }
        assert_eq!(&reconstructed, source);
    }
}

#[tokio::test]
async fn revision_source_enforces_scope_disclosure_policy_and_cancellation() {
    let fixture = Fixture::new(&["private/**", "dist/**"], &["private/allowed.rs"]).await;
    for file in [
        "src/lib.rs",
        "private/hidden.rs",
        "private/allowed.rs",
        "dist/output.rs",
    ] {
        fixture.write(file, "authorized source\n");
    }
    let revision = fixture.commit();
    let admitted = fixture.admitted();
    let scope = &fixture.port.scope;
    for foreign in [
        ResolvedScope::new(
            ProjectId::new("project.foreign").unwrap(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            None,
        )
        .unwrap(),
        ResolvedScope::new(
            scope.project_id.clone(),
            RepositoryId::new("repository.foreign").unwrap(),
            scope.worktree_id.clone(),
            None,
        )
        .unwrap(),
        ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            WorktreeId::new("worktree.foreign").unwrap(),
            None,
        )
        .unwrap(),
    ] {
        let context = fixture.context(foreign, DisclosureClass::Evidence, true);
        let outcome = fixture.read(&context, &revision, "src/lib.rs", 1, 1).await;
        assert!(
            matches!(outcome, RetrievalPortOutcome::Refused(_, ref problem) if problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized)
        );
        assert!(outcome.evidence().payload.is_none());
    }
    let denied = fixture.context(scope.clone(), DisclosureClass::Evidence, false);
    assert!(matches!(
        fixture.read(&denied, &revision, "src/lib.rs", 1, 1).await,
        RetrievalPortOutcome::Refused(_, _)
    ));
    let metadata = fixture.context(scope.clone(), DisclosureClass::Metadata, true);
    let redacted = fixture.read(&metadata, &revision, "src/lib.rs", 1, 1).await;
    assert!(matches!(redacted, RetrievalPortOutcome::Unavailable(_)));
    assert_eq!(
        redacted.evidence().omissions[0].reason,
        OmissionReason::Redacted
    );
    assert!(redacted.evidence().payload.is_none());
    assert!(matches!(
        fixture
            .read(&admitted, &revision, "private/hidden.rs", 1, 1)
            .await,
        RetrievalPortOutcome::Refused(_, _)
    ));
    for file in ["private/allowed.rs", "dist/output.rs"] {
        assert_eq!(
            payload(fixture.read(&admitted, &revision, file, 1, 1).await)["body"],
            "authorized source\n"
        );
    }
    let cancelled = admitted.clone().with_cancellation(
        CancellationContext::cancelled("cancel.source-lines", tracedecay_contracts::now_micros())
            .unwrap(),
    );
    let cancelled = fixture
        .read(&cancelled, &revision, "src/lib.rs", 1, 1)
        .await;
    assert!(matches!(cancelled, RetrievalPortOutcome::Cancelled(_)));
    assert!(cancelled.evidence().payload.is_none());
    let timed_out = admitted.with_deadline(Deadline::new(UtcMicros(1)).unwrap());
    let timed_out = fixture
        .read(&timed_out, &revision, "src/lib.rs", 1, 1)
        .await;
    assert!(matches!(timed_out, RetrievalPortOutcome::TimedOut(_)));
    assert!(timed_out.evidence().payload.is_none());
}

#[tokio::test]
async fn revision_source_reports_unreadable_content_without_empty_success() {
    let fixture = Fixture::new(&[], &[]).await;
    fixture.write(
        "line-too-wide.rs",
        "a".repeat(MAX_REVISION_SOURCE_BYTES + 1),
    );
    fixture.write(
        "blob-too-large.rs",
        vec![b'a'; GIT_HISTORICAL_BLOB_MAX_BYTES as usize + 1],
    );
    fixture.write("binary.rs", b"a\0b");
    fixture.write("non-utf8.rs", [0xff, 0xfe]);
    fixture.write("empty.rs", b"");
    let revision = fixture.commit();
    let context = fixture.admitted();
    for (file, reason) in [
        ("line-too-wide.rs", OmissionReason::Budget),
        ("blob-too-large.rs", OmissionReason::Budget),
        ("binary.rs", OmissionReason::Unsupported),
        ("non-utf8.rs", OmissionReason::Unsupported),
    ] {
        let outcome = fixture.read(&context, &revision, file, 1, 999).await;
        assert!(
            matches!(outcome, RetrievalPortOutcome::Unavailable(_)),
            "{file}: {outcome:?}"
        );
        assert_eq!(outcome.evidence().omissions[0].reason, reason);
        assert!(outcome.evidence().payload.is_none());
    }
    let empty = fixture.read(&context, &revision, "empty.rs", 1, 1).await;
    assert!(
        matches!(empty, RetrievalPortOutcome::Refused(_, ref problem) if problem.kind() == ApplicationProblemKind::InvalidRequest)
    );
    let unknown = GitOidV1::new("f".repeat(40)).unwrap();
    let unknown = fixture.read(&context, &unknown, "binary.rs", 1, 1).await;
    assert!(matches!(unknown, RetrievalPortOutcome::Unavailable(_)));
    assert!(unknown.evidence().payload.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn revision_source_reads_symlink_blob_without_following_it_or_submodules() {
    let fixture = Fixture::new(&[], &[]).await;
    fixture.write("plain.rs", "initial\n");
    let base = fixture.commit();
    let outside = fixture._directory.path().join("outside.rs");
    std::fs::write(&outside, "outside source must never be read").unwrap();
    std::os::unix::fs::symlink("../outside.rs", fixture.root.join("link.rs")).unwrap();
    git(
        &fixture.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            base.as_str(),
            "nested",
        ],
    );
    git(&fixture.root, &["add", "link.rs"]);
    git(&fixture.root, &["commit", "-m", "link fixtures"]);
    let revision = GitOidV1::new(git(&fixture.root, &["rev-parse", "HEAD"]).trim()).unwrap();
    let context = fixture.admitted();
    assert_eq!(
        payload(fixture.read(&context, &revision, "link.rs", 1, 1).await)["body"],
        "../outside.rs"
    );
    for file in ["nested", "nested/plain.rs"] {
        let outcome = fixture.read(&context, &revision, file, 1, 1).await;
        assert!(
            !matches!(
                outcome,
                RetrievalPortOutcome::Completed(_) | RetrievalPortOutcome::Partial(_)
            ),
            "{file}: {outcome:?}"
        );
        assert!(outcome.evidence().payload.is_none());
    }
}
