use tracedecay_code_extraction::incremental::{ParseCompleteness, ParseDocumentIdentity};
use tracedecay_code_extraction::{
    ExtractionArtifactV1, LanguageExtractor as ParserLanguageExtractor, SchemaEvidenceIssueV1,
};
use tracedecay_domain::SanitizedCodeFileV1;

use crate::retained_parse::SharedRetainedParsePool;

use super::{CodeIndexCapturedFileV1, CodeIndexExecutionControlV1, CodeIndexProductionErrorV1};

#[hotpath::measure(label = "code_index.extract.parser_artifact")]
pub(super) fn parse_for_indexing(
    retained_parses: &SharedRetainedParsePool,
    identity: ParseDocumentIdentity,
    file: &SanitizedCodeFileV1,
    captured: &CodeIndexCapturedFileV1,
    parser: &dyn ParserLanguageExtractor,
    control: &dyn CodeIndexExecutionControlV1,
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
    let parsed_len = crate::chunks::snap_down(
        source,
        source
            .len()
            .min(crate::extract::MAX_EXTRACTION_SOURCE_BYTES),
    );
    let admitted = || !control.is_cancelled() && !control.is_deadline_exceeded();
    let (report, mut extraction) = retained_parses.parse_and_extract_artifact_with_control(
        identity,
        language.as_str(),
        &source[..parsed_len],
        parser,
        Some(&admitted),
    )?;
    if let ParseCompleteness::Partial { reasons } = report.completeness {
        extraction
            .artifact
            .result
            .errors
            .push(format!("retained parse incomplete: {reasons:?}"));
        if let Some(evidence) = &mut extraction.artifact.schema_evidence {
            evidence.mark_partial(SchemaEvidenceIssueV1::ParseError);
        }
    }
    Ok((extraction.artifact, parsed_len))
}
