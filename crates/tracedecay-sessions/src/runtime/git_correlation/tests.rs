use super::*;

fn span(
    id: &str,
    provider: &str,
    session_id: &str,
    branch: Option<&str>,
    worktree: &str,
    first_ts: i64,
    last_ts: i64,
) -> SessionGitSpan {
    SessionGitSpan {
        span_id: id.to_owned(),
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        thread_id: None,
        branch: branch.map(str::to_owned),
        worktree: worktree.to_owned(),
        first_ts,
        last_ts,
        event_count: 2,
        source: SpanSource::Ingest,
        branch_provenance: BranchProvenance::Captured,
        capture_window: None,
    }
}

fn commit(
    sha: &str,
    provider: &str,
    session_id: &str,
    relation: CommitRelation,
    confidence: i64,
) -> CommitSessionRecord {
    CommitSessionRecord {
        commit_sha: sha.to_owned(),
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        branch: Some("main".to_owned()),
        worktree: Some("/repo".to_owned()),
        committed_at: 150,
        span_overlap_kind: SpanOverlapKind::Direct,
        span_id: None,
        relation,
        evidence: if relation == CommitRelation::Produced {
            CommitEvidence::ToolResult
        } else {
            CommitEvidence::HeadObservation
        },
        confidence,
        evidence_message_id: Some("message-1".to_owned()),
    }
}

fn projection() -> GitEvidenceProjectionV1 {
    GitEvidenceProjectionV1::new(
        "watermark-7",
        vec![
            span("span-a", "", "session-a", Some("main"), "/repo/", 100, 200),
            span(
                "span-b",
                "claude",
                "session-a",
                Some("feature"),
                "/repo",
                210,
                240,
            ),
            span(
                "span-c",
                "codex",
                "session-b",
                Some("main"),
                "/other",
                120,
                180,
            ),
        ],
        vec![
            commit(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "claude",
                "session-a",
                CommitRelation::Produced,
                100,
            ),
            commit(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
                "codex",
                "session-b",
                CommitRelation::Observed,
                60,
            ),
        ],
    )
    .unwrap()
}

#[test]
fn normalize_worktree_preserves_root_and_removes_aliases() {
    assert_eq!(normalize_worktree("/repo///"), "/repo");
    assert_eq!(normalize_worktree(r"C:\repo\\"), "C:/repo");
    assert_eq!(normalize_worktree("/"), "/");
    assert_eq!(normalize_worktree("/private/var/tmp/repo"), "/var/tmp/repo");
}

#[test]
fn canonical_worktree_key_folds_os_aliases_and_preserves_missing_paths() {
    let root = tempfile::tempdir().unwrap();
    let lexical = normalize_worktree(&root.path().to_string_lossy());
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    // Two spellings of one directory (an OS alias and the canonical name)
    // resolve to one identity.
    assert_eq!(
        canonical_worktree_key(&lexical).unwrap(),
        normalize_worktree(&canonical.to_string_lossy())
    );
    // A missing historical path keeps its recorded spelling untouched.
    let missing = normalize_worktree(&root.path().join("deleted").to_string_lossy());
    assert_eq!(canonical_worktree_key(&missing).unwrap(), missing);
}

#[test]
fn canonical_worktree_key_reresolves_a_path_created_after_the_miss() {
    let root = tempfile::tempdir().unwrap();
    let worktree = root.path().join("worktree");
    let spelling = normalize_worktree(&worktree.to_string_lossy());
    // First operation: the path does not exist, the spelling stays lexical.
    assert_eq!(canonical_worktree_key(&spelling).unwrap(), spelling);
    // Next operation after the directory is created resolves the canonical
    // identity — a stale miss may not be retained across operations.
    std::fs::create_dir(&worktree).unwrap();
    let canonical = std::fs::canonicalize(&worktree).unwrap();
    assert_eq!(
        canonical_worktree_key(&spelling).unwrap(),
        normalize_worktree(&canonical.to_string_lossy())
    );
}

#[cfg(unix)]
#[test]
fn canonical_worktree_key_reresolves_a_retargeted_link() {
    let root = tempfile::tempdir().unwrap();
    let target_a = root.path().join("target-a");
    let target_b = root.path().join("target-b");
    std::fs::create_dir(&target_a).unwrap();
    std::fs::create_dir(&target_b).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target_a, &link).unwrap();
    let spelling = normalize_worktree(&link.to_string_lossy());
    assert_eq!(
        canonical_worktree_key(&spelling).unwrap(),
        normalize_worktree(&std::fs::canonicalize(&target_a).unwrap().to_string_lossy())
    );
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&target_b, &link).unwrap();
    assert_eq!(
        canonical_worktree_key(&spelling).unwrap(),
        normalize_worktree(&std::fs::canonicalize(&target_b).unwrap().to_string_lossy())
    );
}

#[cfg(unix)]
#[test]
fn canonical_worktree_key_rejects_a_canonical_identity_outside_utf8() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    // A UTF-8 link name pointing into a non-UTF-8 directory: the resolved
    // identity cannot be stored byte-exactly, so the spelling must fail
    // typed rather than silently keep the caller's alias.
    let target = root
        .path()
        .join(std::ffi::OsString::from_vec(b"non-\xff-utf8".to_vec()));
    std::fs::create_dir(&target).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let spelling = normalize_worktree(&link.to_string_lossy());
    assert!(canonical_worktree_key(&spelling).is_err());
}

#[cfg(unix)]
#[test]
fn canonical_worktree_key_reports_non_not_found_failures() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    // A file where a directory should be fails with an error other than
    // NotFound: it must surface as a typed failure, not a lexical fallback
    // that would silently rekey the row.
    let bad = normalize_worktree(&file.join("nested").to_string_lossy());
    assert!(canonical_worktree_key(&bad).is_err());
}

#[cfg(windows)]
#[test]
fn canonical_worktree_key_reports_non_not_found_failures() {
    // A colon inside a component is an invalid name on Windows: resolving
    // it fails with an error other than NotFound, which must surface as a
    // typed failure rather than a silent lexical fallback.
    let bad = normalize_worktree(r"C:\invalid:component\sub");
    assert!(canonical_worktree_key(&bad).is_err());
}

#[test]
fn git_ref_filter_parses_and_validates_kinds() {
    assert_eq!(
        GitRefFilter::parse("worktree", r"C:\repo\\").unwrap(),
        GitRefFilter::Worktree("C:/repo".to_owned())
    );
    assert!(GitRefFilter::parse("commit", "abc").is_err());
    assert!(GitRefFilter::parse("tag", "v1").is_err());
}

#[test]
fn projection_is_canonical_and_normalizes_evidence() {
    let projection = projection();
    assert_eq!(projection.source_watermark(), "watermark-7");
    assert_eq!(
        projection
            .spans()
            .iter()
            .map(|span| span.span_id.as_str())
            .collect::<Vec<_>>(),
        vec!["span-a", "span-b", "span-c"]
    );
    assert_eq!(projection.spans()[0].worktree, "/repo");
    // span-a arrives with an empty provider; the canonical session map
    // must settle it from the sibling span of the same session.
    assert_eq!(projection.spans()[0].provider, "claude");
    assert_eq!(projection.commit_sessions()[0].provider, "claude");
}

#[test]
fn projection_rejects_duplicate_or_dangling_evidence() {
    let duplicate = span("same", "claude", "session-a", Some("main"), "/repo", 1, 2);
    assert!(matches!(
        GitEvidenceProjectionV1::new("watermark", vec![duplicate.clone(), duplicate], Vec::new()),
        Err(GitCorrelationError::Contract(message))
            if message == "Git evidence span identities must be unique"
    ));

    let mut dangling = commit(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "claude",
        "session-a",
        CommitRelation::Produced,
        100,
    );
    dangling.span_id = Some("missing".to_owned());
    assert!(matches!(
        GitEvidenceProjectionV1::new("watermark", Vec::new(), vec![dangling]),
        Err(GitCorrelationError::Contract(message))
            if message == "Git evidence relation references an absent span"
    ));
}

#[test]
fn branch_query_aggregates_spans_per_session() {
    let hits = projection().sessions_for(
        &SessionsForQuery {
            git_ref: GitRefFilter::Branch("main".to_owned()),
            since: Some(110),
            until: Some(210),
            limit: 10,
        },
        CommitRelationFilter::Produced,
    );
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].session_id, "session-a");
    assert_eq!(hits[0].provider, "claude");
    assert_eq!(hits[0].event_count, 2);
    assert_eq!(hits[0].span_count, 1);
}

#[test]
fn commit_query_defaults_to_producer_evidence() {
    let projection = projection();
    let query = SessionsForQuery {
        git_ref: GitRefFilter::Commit("aaaaaa".to_owned()),
        since: None,
        until: None,
        limit: 10,
    };
    let produced = projection.sessions_for(&query, CommitRelationFilter::Produced);
    assert_eq!(produced.len(), 1);
    assert_eq!(produced[0].session_id, "session-a");

    let all = projection.sessions_for(&query, CommitRelationFilter::All);
    assert_eq!(all.len(), 2);
}

#[test]
fn compound_scope_intersects_branch_worktree_and_commit() {
    let ids = projection()
        .session_ids_for_scope(&GitScopeFilter {
            branch: Some("main".to_owned()),
            worktree: Some("/repo".to_owned()),
            commit: Some("aaaaaa".to_owned()),
        })
        .unwrap();
    assert_eq!(ids, vec![("claude".to_owned(), "session-a".to_owned())]);
}

#[test]
fn commit_scope_falls_back_to_observed_when_no_producer_matches() {
    let ids = projection()
        .session_ids_for_scope(&GitScopeFilter {
            branch: None,
            worktree: None,
            commit: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab".to_owned()),
        })
        .unwrap();
    assert_eq!(ids, vec![("codex".to_owned(), "session-b".to_owned())]);
}
