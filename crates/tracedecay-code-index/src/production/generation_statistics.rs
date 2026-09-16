//! Aggregate facts derived from immutable sealed code-index generations.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1};
use crate::extract::ExtractionCoverageV1;

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
    #[hotpath::measure(label = "code_index.build.statistics")]
    pub fn generation_statistics(
        &self,
    ) -> Result<CodeIndexGenerationStatisticsV1, CodeIndexProductionErrorV1> {
        Ok(self.statistics.clone())
    }

    /// Counts current payload reuse and prior payload bindings that no longer
    /// occur in `current`, with duplicate payload digests matched as a multiset.
    pub fn clone_payload_change_accounting(&self, current: &Self) -> (u64, u64) {
        let mut prior_payloads = BTreeMap::<&str, u64>::new();
        for file in &self.files {
            for body in &file.artifacts.clone_bodies {
                let count = prior_payloads
                    .entry(body.payload.payload_digest.as_str())
                    .or_default();
                *count = count.saturating_add(1);
            }
        }
        let mut reused = 0_u64;
        for file in &current.files {
            for body in &file.artifacts.clone_bodies {
                let Some(count) = prior_payloads.get_mut(body.payload.payload_digest.as_str())
                else {
                    continue;
                };
                if *count > 0 {
                    *count -= 1;
                    reused = reused.saturating_add(1);
                }
            }
        }
        let invalidated = prior_payloads
            .into_values()
            .fold(0_u64, u64::saturating_add);
        (reused, invalidated)
    }
}

impl CodeIndexGenerationStatisticsV1 {
    pub(super) fn from_generation_parts(
        files: &[std::sync::Arc<super::FileGenerationArtifactsV1>],
        symbol_count: usize,
        edge_count: usize,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let source_total_bytes =
            checked_source_total(files.iter().map(|file| &file.extraction.coverage))?;
        let symbol_count = u64::try_from(symbol_count).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("generation symbol count exceeds u64".to_owned())
        })?;
        let edge_count = u64::try_from(edge_count).map_err(|_| {
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
