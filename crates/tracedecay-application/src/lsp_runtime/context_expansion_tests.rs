use super::feedback_source::{context_expansion_scope_is_current, valid_context_expansion_record};
use super::{
    LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION, LspFeedbackProjectionScope,
    StoredLspContextExpansionV1,
};
use crate::feedback::owner::FeedbackReadOperationV1;
use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest, ManifestDigest, UtcMicros};
use tracedecay_lsp::{AdmittedRoot, ContextProjectionIdentity, ContextProjectionKind};

fn record() -> StoredLspContextExpansionV1 {
    StoredLspContextExpansionV1 {
        schema_version: LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION,
        root_uri: "file:///root".to_owned(),
        document_uri: Some("file:///root/src/lib.rs".to_owned()),
        kind: ContextProjectionKind::diagnostics(),
        stable_id: "finding.1".to_owned(),
        scope_digest: "sha256:scope".to_owned(),
        identity: ContextProjectionIdentity {
            head_commit_id: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            code_generation_id: "generation.v1.aaaaaaaa.00000001".to_owned(),
            snapshot_digest: format!("sha256:{}", "a".repeat(64)),
            invalidation_digest: format!("sha256:{}", "b".repeat(64)),
            snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
            document_content_digest: Some(format!("sha256:{}", "d".repeat(64))),
        },
        generation: 1,
        issued_at: UtcMicros(10),
        expires_at: UtcMicros(20),
        canonical_operation: FeedbackReadOperationV1::Expand,
        canonical_handle: "rh_0123456789abcdef01234567".to_owned(),
    }
}

#[test]
fn expansion_handles_deny_expiry_wrong_root_and_wrong_scope() {
    let record = record();
    let root = AdmittedRoot::new("file:///root");
    assert!(valid_context_expansion_record(
        &record,
        &root,
        "sha256:scope",
        UtcMicros(19)
    ));
    assert!(!valid_context_expansion_record(
        &record,
        &root,
        "sha256:scope",
        UtcMicros(20)
    ));
    assert!(!valid_context_expansion_record(
        &record,
        &AdmittedRoot::new("file:///other"),
        "sha256:scope",
        UtcMicros(19)
    ));
    assert!(!valid_context_expansion_record(
        &record,
        &root,
        "sha256:other",
        UtcMicros(19)
    ));
}

#[test]
fn expansion_handles_become_stale_on_exact_generation_drift() {
    let record = record();
    let current = LspFeedbackProjectionScope {
        head_commit_id: CommitId::new(record.identity.head_commit_id.clone()).expect("commit"),
        code_generation_id: CodeGenerationId::new(record.identity.code_generation_id.clone())
            .expect("generation"),
        snapshot_digest: ManifestDigest::new(record.identity.snapshot_digest.clone())
            .expect("snapshot digest"),
        invalidation_digest: ManifestDigest::new(record.identity.invalidation_digest.clone())
            .expect("invalidation digest"),
        snapshot_content_digest: ContentDigest::new(
            record.identity.snapshot_content_digest.clone(),
        )
        .expect("snapshot content digest"),
        document_file_occurrence_id: None,
        document_content_digest: record
            .identity
            .document_content_digest
            .as_ref()
            .map(|digest| ContentDigest::new(digest.clone()).expect("document content digest")),
        document_relative_path: None,
        generation: record.generation,
    };
    assert!(context_expansion_scope_is_current(&record, &current));

    let stale = LspFeedbackProjectionScope {
        generation: current.generation + 1,
        ..current
    };
    assert!(!context_expansion_scope_is_current(&record, &stale));
}
