use tracedecay_domain::{ExtractionResult, NodeKind};

use super::{
    ParseCompleteness, ParseError, ParseInputEdit, ParseReport, ParseResetReason, ParseReuse,
    RetainedParseDocument,
};
use crate::LanguageExtractor;
use crate::extraction_artifact::{ExtractedImportEvidenceV1, ExtractionArtifactV1};
use crate::parsed_extraction::{
    ParsedExtraction, ParsedExtractionArtifactV1, ParsedExtractionDisposition,
    ParsedExtractionResetReason, ParsedExtractionScope, ParsedTraversalMetrics,
    merge_changed_extraction, node_intersects_edit,
};

impl RetainedParseDocument {
    /// Produce a complete canonical legacy graph from the retained tree.
    pub fn extract_canonical(
        &self,
        extractor: &dyn LanguageExtractor,
        report: &ParseReport,
        previous: Option<&ExtractionResult>,
    ) -> Result<ParsedExtraction, ParseError> {
        let previous_artifact = previous.cloned().map(ExtractionArtifactV1::from_result);
        self.extract_canonical_artifact(extractor, report, previous_artifact.as_ref())
            .map(ParsedExtractionArtifactV1::into_parsed)
    }

    /// Produce a complete graph and structured evidence artifact from the
    /// current retained tree. Incremental deltas replace every affected import
    /// statement's rows, including when deletion produces no replacement row.
    pub fn extract_canonical_artifact(
        &self,
        extractor: &dyn LanguageExtractor,
        report: &ParseReport,
        previous: Option<&ExtractionArtifactV1>,
    ) -> Result<ParsedExtractionArtifactV1, ParseError> {
        if report.state_epoch != self.state_epoch {
            return Err(ParseError::StaleReport);
        }

        match report.reuse {
            ParseReuse::Initial => Ok(self.full_artifact(extractor, None)),
            ParseReuse::Reset { reason } => {
                Ok(self.full_artifact(extractor, Some(parse_reset_reason(reason))))
            }
            ParseReuse::Noop => match previous {
                Some(previous) => Ok(ParsedExtractionArtifactV1::complete(
                    previous.clone(),
                    ParsedExtractionScope::ChangedRegions(&[]),
                    ParsedTraversalMetrics::default(),
                )),
                None => Ok(self.full_artifact(
                    extractor,
                    Some(ParsedExtractionResetReason::MissingPriorExtraction),
                )),
            },
            ParseReuse::Incremental => {
                self.extract_incremental_artifact(extractor, report, previous)
            }
        }
    }

    fn extract_incremental_artifact(
        &self,
        extractor: &dyn LanguageExtractor,
        report: &ParseReport,
        previous: Option<&ExtractionArtifactV1>,
    ) -> Result<ParsedExtractionArtifactV1, ParseError> {
        if !matches!(report.completeness, ParseCompleteness::Complete) {
            return Ok(
                self.full_artifact(extractor, Some(ParsedExtractionResetReason::PartialParse))
            );
        }
        let Some(previous) = previous else {
            return Ok(self.full_artifact(
                extractor,
                Some(ParsedExtractionResetReason::MissingPriorExtraction),
            ));
        };
        let Some(edit) = report.source_edit else {
            return Ok(
                self.full_artifact(extractor, Some(ParsedExtractionResetReason::PartialParse))
            );
        };
        let old_lines = edit
            .old_end_position
            .row
            .saturating_sub(edit.start_position.row);
        let new_lines = edit
            .new_end_position
            .row
            .saturating_sub(edit.start_position.row);
        if old_lines != new_lines {
            return Ok(
                self.full_artifact(extractor, Some(ParsedExtractionResetReason::MultilineEdit))
            );
        }

        let delta = extractor.extract_parsed_artifact(
            self.identity.logical_path(),
            &self.source,
            &self.tree,
            ParsedExtractionScope::ChangedRegions(&report.extraction_ranges),
        );
        if matches!(delta.disposition, ParsedExtractionDisposition::Reset { .. }) {
            return Ok(self.complete_composite_reset(extractor, delta));
        }
        let metrics = delta.metrics;
        let old_end_row =
            u32::try_from(edit.old_end_position.row).map_err(|_| ParseError::InvalidEdit {
                detail: "edit end row does not fit canonical extraction rows".to_owned(),
            })?;
        match merge_changed_artifact(previous, delta.artifact, edit, old_end_row) {
            Some(artifact) => Ok(ParsedExtractionArtifactV1::complete(
                artifact,
                ParsedExtractionScope::ChangedRegions(&report.extraction_ranges),
                metrics,
            )),
            None => Ok(self.full_artifact(
                extractor,
                Some(ParsedExtractionResetReason::ChangedRootIdentity),
            )),
        }
    }

    fn full_artifact(
        &self,
        extractor: &dyn LanguageExtractor,
        reason: Option<ParsedExtractionResetReason>,
    ) -> ParsedExtractionArtifactV1 {
        let extracted = extractor.extract_parsed_artifact(
            self.identity.logical_path(),
            &self.source,
            &self.tree,
            ParsedExtractionScope::FullDocument,
        );
        let extracted = self.complete_composite_reset(extractor, extracted);
        match reason {
            Some(reason) => {
                ParsedExtractionArtifactV1::reset(extracted.artifact, reason, self.source.len())
            }
            None => extracted,
        }
    }

    fn complete_composite_reset(
        &self,
        extractor: &dyn LanguageExtractor,
        extracted: ParsedExtractionArtifactV1,
    ) -> ParsedExtractionArtifactV1 {
        match extracted.disposition {
            ParsedExtractionDisposition::Reset {
                reason: ParsedExtractionResetReason::CompositeGrammar,
            } => ParsedExtractionArtifactV1::reset(
                extractor.extract_artifact(self.identity.logical_path(), &self.source),
                ParsedExtractionResetReason::CompositeGrammar,
                self.source.len(),
            ),
            _ => extracted,
        }
    }
}

fn parse_reset_reason(reason: ParseResetReason) -> ParsedExtractionResetReason {
    match reason {
        ParseResetReason::FullReplacement => ParsedExtractionResetReason::FullReplacement,
        ParseResetReason::LanguageChanged => ParsedExtractionResetReason::LanguageChanged,
    }
}

fn merge_changed_artifact(
    previous: &ExtractionArtifactV1,
    delta: ExtractionArtifactV1,
    edit: ParseInputEdit,
    old_end_row: u32,
) -> Option<ExtractionArtifactV1> {
    let delta_ids = delta
        .result
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::File)
        .map(|node| node.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let affected_import_statements = previous
        .result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Use)
        .filter(|node| {
            node_intersects_edit(node, edit.start_position, edit.old_end_position)
                || delta_ids.contains(&node.id)
        })
        .map(|node| {
            (
                node.start_line,
                node.start_column,
                node.end_line,
                node.end_column,
            )
        })
        .collect::<Vec<_>>();
    let result = merge_changed_extraction(
        &previous.result,
        delta.result,
        edit.start_position,
        edit.old_end_position,
    )?;
    let mut imports = previous
        .imports
        .iter()
        .filter(|row| {
            !affected_import_statements
                .iter()
                .any(|range| import_position_is_within(row, *range))
        })
        .cloned()
        .map(|row| shift_unaffected_import(row, edit, old_end_row))
        .collect::<Option<Vec<_>>>()?;
    imports.extend(delta.imports);
    let mut artifact = ExtractionArtifactV1 { result, imports };
    artifact.canonicalize_order();
    Some(artifact)
}

fn import_position_is_within(
    row: &ExtractedImportEvidenceV1,
    statement: (u32, u32, u32, u32),
) -> bool {
    let point = (row.start_line, row.start_column);
    let start = (statement.0, statement.1);
    let end = (statement.2, statement.3);
    start <= point && point < end
}

fn shift_unaffected_import(
    mut row: ExtractedImportEvidenceV1,
    edit: ParseInputEdit,
    old_end_row: u32,
) -> Option<ExtractedImportEvidenceV1> {
    let edit_start = u64::try_from(edit.start_byte).ok()?;
    let old_end = u64::try_from(edit.old_end_byte).ok()?;
    if row.span.end_byte <= edit_start {
        return Some(row);
    }
    if row.span.start_byte < old_end {
        return None;
    }

    row.span.start_byte = shift_value(row.span.start_byte, edit.old_end_byte, edit.new_end_byte)?;
    row.span.end_byte = shift_value(row.span.end_byte, edit.old_end_byte, edit.new_end_byte)?;
    if row.start_line == old_end_row {
        row.start_column = shift_value(
            u64::from(row.start_column),
            edit.old_end_position.column,
            edit.new_end_position.column,
        )?
        .try_into()
        .ok()?;
    }
    Some(row)
}

fn shift_value(value: u64, old_end: usize, new_end: usize) -> Option<u64> {
    if new_end >= old_end {
        value.checked_add(u64::try_from(new_end - old_end).ok()?)
    } else {
        value.checked_sub(u64::try_from(old_end - new_end).ok()?)
    }
}
