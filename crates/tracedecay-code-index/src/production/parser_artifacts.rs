use tracedecay_code_extraction::incremental::{ParseCompleteness, ParseDocumentIdentity};
use tracedecay_code_extraction::{
    ExtractionArtifactV1, LanguageExtractor as ParserLanguageExtractor,
};
use tracedecay_domain::{SanitizedCodeFileV1, SanitizedCodeSnapshotV1};

use crate::retained_parse::SharedRetainedParsePool;

use super::{
    CodeIndexCapturedFileV1, CodeIndexProductionConfigV1, CodeIndexProductionErrorV1,
    CodeIndexRepositoryParseIdentityV1,
};

pub(super) fn parse_for_indexing(
    retained_parses: &SharedRetainedParsePool,
    config: &CodeIndexProductionConfigV1,
    snapshot: &SanitizedCodeSnapshotV1,
    repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
    file: &SanitizedCodeFileV1,
    captured: &CodeIndexCapturedFileV1,
    parser: &dyn ParserLanguageExtractor,
) -> Result<(ExtractionArtifactV1, usize), CodeIndexProductionErrorV1> {
    let language = file.language.as_ref().ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "present snapshot file has no declared language".to_owned(),
        )
    })?;
    let source = std::str::from_utf8(&captured.sanitized_bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "admitted sanitized source is not UTF-8: {error}"
        ))
    })?;
    let mut parsed_len = source
        .len()
        .min(crate::extract::MAX_EXTRACTION_SOURCE_BYTES);
    while !source.is_char_boundary(parsed_len) {
        parsed_len = parsed_len.saturating_sub(1);
    }
    let (report, mut extraction) = retained_parses.parse_and_extract_artifact(
        ParseDocumentIdentity::Repository {
            project_id: config.project_id.clone(),
            repository_id: snapshot.repository.clone(),
            worktree_id: snapshot.worktree.clone(),
            reference: snapshot.reference.clone(),
            commit: snapshot.source_revision.clone(),
            tree: repository_parse_identity.tree.clone(),
            dirty: repository_parse_identity.dirty,
            logical_path: file.logical_path.clone(),
        },
        language.as_str(),
        &source[..parsed_len],
        parser,
    )?;
    if let ParseCompleteness::Partial { reasons } = report.completeness {
        extraction
            .artifact
            .result
            .errors
            .push(format!("retained parse incomplete: {reasons:?}"));
    }
    Ok((extraction.artifact, parsed_len))
}
