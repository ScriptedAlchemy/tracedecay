//! Aggregate facts derived from immutable sealed code-index generations.

use serde::{Deserialize, Serialize};
use tracedecay_domain::ExtractionCoverageV1;

use super::{CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1};

/// Checked aggregate facts derived from one immutable sealed generation.
///
/// These values describe generation evidence, rather than a mutable database
/// projection of that evidence. Callers therefore cannot mistake a runtime
/// SQLite schema for the code-index authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeIndexGenerationStatisticsV1 {
    pub source_total_bytes: u64,
    pub symbol_count: u64,
    pub edge_count: u64,
}

impl CodeIndexPublishedGenerationV1 {
    /// Return checked aggregate facts for this immutable generation.
    ///
    /// Each file's extraction coverage partitions the captured source bytes,
    /// including parsed, error, and unsupported spans. Keeping the checked
    /// accumulation here makes a census faithful to the sealed generation and
    /// prevents downstream runtime telemetry from reading removed SQL tables.
    pub fn generation_statistics(
        &self,
    ) -> Result<CodeIndexGenerationStatisticsV1, CodeIndexProductionErrorV1> {
        let source_total_bytes =
            checked_source_total(self.files.iter().map(|file| &file.extraction.coverage))?;
        let symbol_count = u64::try_from(self.symbols.symbols.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("generation symbol count exceeds u64".to_owned())
        })?;
        let edge_count = u64::try_from(self.edges.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("generation edge count exceeds u64".to_owned())
        })?;
        Ok(CodeIndexGenerationStatisticsV1 {
            source_total_bytes,
            symbol_count,
            edge_count,
        })
    }
}

fn checked_source_total<'a>(
    mut coverages: impl Iterator<Item = &'a ExtractionCoverageV1>,
) -> Result<u64, CodeIndexProductionErrorV1> {
    coverages.try_fold(0_u64, |total, coverage| {
        let file_total = coverage
            .parsed_bytes
            .checked_add(coverage.error_bytes)
            .and_then(|total| total.checked_add(coverage.unsupported_bytes))
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "generation file coverage byte total overflowed".to_owned(),
                )
            })?;
        total.checked_add(file_total).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "generation source byte total overflowed".to_owned(),
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_total_includes_parsed_error_and_unsupported_coverage() {
        let coverages = [
            ExtractionCoverageV1 {
                parsed_bytes: 5,
                error_bytes: 7,
                unsupported_bytes: 11,
                symbols_extracted: 0,
                relations_extracted: 0,
                ambiguity_count: 0,
            },
            ExtractionCoverageV1 {
                parsed_bytes: 13,
                error_bytes: 0,
                unsupported_bytes: 17,
                symbols_extracted: 0,
                relations_extracted: 0,
                ambiguity_count: 0,
            },
        ];

        assert_eq!(
            checked_source_total(coverages.iter()).expect("coverage total"),
            53
        );
    }
}
