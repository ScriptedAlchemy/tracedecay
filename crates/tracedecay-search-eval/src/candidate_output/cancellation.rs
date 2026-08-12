use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1, CodeIndexProductionOwnerV1,
    CodeIndexRepositoryParseIdentityV1,
};
use tracedecay_domain::{
    ChunkerRevision, FileOccurrenceId, LanguageId, PolicyRevisionId, PrivacyDomainId, ProjectId,
    ProjectionKeyV1, ProjectionKindV1, RepositoryDirtyStateV1, RepositoryId, SanitizationReceiptId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    UtcMicros,
};

use super::control::CancelledControl;
use super::{
    ApplyingProjectionSink, CandidateOutputError, CandidateWorkloadV1, SharedPublicationStore, id,
    lexical_projection_profile_digest,
};

pub(super) fn prove_cancellation(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
) -> Result<(), CandidateOutputError> {
    // Cancellation is proven against the production code-index control surface
    // (typed Cancelled interruption, no publish). The generator records the
    // required receipt string after this check succeeds.
    let mut files = Vec::new();
    let mut captured = Vec::new();
    let document = workload
        .corpus
        .first()
        .ok_or_else(|| CandidateOutputError::Contract("corpus empty".to_owned()))?;
    let absolute = repo_root.join(&document.path);
    let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
        path: absolute,
        source,
    })?;
    let file_occurrence_id = id::<FileOccurrenceId>("file.cancel.probe")?;
    files.push(SanitizedCodeFileV1 {
        file_occurrence_id: file_occurrence_id.clone(),
        logical_path: document.path.clone(),
        language: Some(id::<LanguageId>(&document.language)?),
        content_digest: content_digest(&bytes),
        disposition: SnapshotFileDispositionV1::Present,
    });
    captured.push(CodeIndexCapturedFileV1 {
        file_occurrence_id,
        sanitized_bytes: bytes.clone(),
        sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
    });
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.candidate.cancel")?,
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.cancel")?],
        content_identity: content_digest(&bytes),
        captured_at: UtcMicros(1_000_000),
        files,
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(1_100_000),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.candidate.v1".to_owned(),
            profile_digest: lexical_projection_profile_digest()?,
        },
    };
    let generation_scope = CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot);
    let config = CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.candidate.cancel")?,
        repository: id::<RepositoryId>("repository.candidate.cancel")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.candidate.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.candidate.v2")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.candidate.cancel")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config, store.clone(), ApplyingProjectionSink)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let error = match owner.build_and_publish(request, &CancelledControl) {
        Err(error) => error,
        Ok(_) => {
            return Err(CandidateOutputError::Contract(
                "cancelled publish must fail".to_owned(),
            ));
        }
    };
    if !format!("{error:?}").contains("Cancelled") {
        return Err(CandidateOutputError::Contract(format!(
            "expected cancelled interruption, got {error:?}"
        )));
    }
    if store
        .load_active(&generation_scope)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .is_some()
    {
        return Err(CandidateOutputError::Contract(
            "cancelled publish must not activate a generation".to_owned(),
        ));
    }
    Ok(())
}
