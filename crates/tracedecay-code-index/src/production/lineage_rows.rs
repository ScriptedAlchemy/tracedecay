//! Persisted lineage rows of one generation's evidence.
//!
//! Nearly every lineage row of a successor generation says that a symbol
//! continued unchanged from the prior occurrence of its exact identity
//! tuple, and such a row is a pure function of the two generation ids, that
//! prior occurrence, and the current symbol record. The persisted form names
//! those rows by the current symbol's position in the generation's
//! occurrence-ordered symbol roster (as runs when the prior occurrence is the
//! current one) and keeps every other row whole. Encoding admits a compact
//! row only when rebuilding it reproduces the resolver's candidate exactly.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{CodeGenerationId, SymbolOccurrenceId};

use super::{CodeIndexProductionErrorV1, collect_bounded_ordered};
use crate::lineage::{
    LineageKindV1, LineageResolutionErrorV1, LineageSymbolRecordV1, SymbolLineageCandidateV1,
};

/// Rows verified or rebuilt per unit of pool work.
const LINEAGE_BATCH_ROWS_V1: usize = 4096;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedLineageV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prior_generation: Option<CodeGenerationId>,
    rows: Vec<PersistedLineageRowV1>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum PersistedLineageRowV1 {
    /// Roster symbols `start..start + count`, each unchanged from itself.
    Unchanged {
        start: u32,
        count: u32,
    },
    /// Roster symbol `current`, unchanged from the prior occurrence `prior`.
    UnchangedFrom {
        current: u32,
        prior: SymbolOccurrenceId,
    },
    Candidate(Box<SymbolLineageCandidateV1>),
}

/// A generation's symbols in canonical occurrence order. Encoding and
/// restore both derive it from the file artifacts, so positions agree.
pub(super) fn occurrence_roster<'a>(
    symbols: impl Iterator<Item = &'a Arc<LineageSymbolRecordV1>>,
) -> Vec<&'a LineageSymbolRecordV1> {
    let mut roster = symbols.map(Arc::as_ref).collect::<Vec<_>>();
    roster.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
    roster
}

fn lineage_error(error: LineageResolutionErrorV1) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Lineage(error)
}

impl PersistedLineageV1 {
    pub(super) fn compact(
        lineage: &[SymbolLineageCandidateV1],
        current_generation: &CodeGenerationId,
        roster: &[&LineageSymbolRecordV1],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let prior_generation = lineage
            .first()
            .map(|row| row.evidence.prior_generation.clone());
        let positions = roster
            .iter()
            .enumerate()
            .map(|(position, symbol)| {
                u32::try_from(position)
                    .map(|position| (&symbol.occurrence, position))
                    .map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lineage roster exceeds u32".to_owned(),
                        )
                    })
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let batches = lineage.chunks(LINEAGE_BATCH_ROWS_V1).collect::<Vec<_>>();
        let implicit = collect_bounded_ordered(&batches, |batch, _worker| {
            batch
                .iter()
                .map(|candidate| {
                    let (Some(prior_generation), Some(&position)) = (
                        prior_generation.as_ref(),
                        positions.get(&candidate.current_occurrence),
                    ) else {
                        return Ok(None);
                    };
                    if candidate.kind != LineageKindV1::Unchanged {
                        return Ok(None);
                    }
                    let rebuilt = SymbolLineageCandidateV1::exact_unchanged(
                        prior_generation,
                        current_generation,
                        &candidate.prior_occurrence,
                        roster[position as usize],
                    )
                    .map_err(lineage_error)?;
                    Ok((rebuilt == *candidate).then_some(position))
                })
                .collect::<Result<Vec<_>, CodeIndexProductionErrorV1>>()
        })?;
        let mut rows = Vec::new();
        for (candidate, position) in lineage.iter().zip(implicit.into_iter().flatten()) {
            match position {
                Some(position) if candidate.prior_occurrence == candidate.current_occurrence => {
                    match rows.last_mut() {
                        Some(PersistedLineageRowV1::Unchanged { start, count })
                            if start.checked_add(*count) == Some(position) =>
                        {
                            *count += 1;
                        }
                        _ => rows.push(PersistedLineageRowV1::Unchanged {
                            start: position,
                            count: 1,
                        }),
                    }
                }
                Some(position) => rows.push(PersistedLineageRowV1::UnchangedFrom {
                    current: position,
                    prior: candidate.prior_occurrence.clone(),
                }),
                None => rows.push(PersistedLineageRowV1::Candidate(Box::new(
                    candidate.clone(),
                ))),
            }
        }
        Ok(Self {
            prior_generation,
            rows,
        })
    }

    pub(super) fn expand(
        self,
        current_generation: &CodeGenerationId,
        roster: &[&LineageSymbolRecordV1],
    ) -> Result<Vec<SymbolLineageCandidateV1>, CodeIndexProductionErrorV1> {
        enum RowV1 {
            Implicit(usize, Option<SymbolOccurrenceId>),
            Whole(SymbolLineageCandidateV1),
        }
        let symbol = |position: u32| {
            usize::try_from(position)
                .ok()
                .filter(|position| *position < roster.len())
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed lineage row names a symbol outside its roster".to_owned(),
                    )
                })
        };
        let mut work = Vec::new();
        for row in self.rows {
            match row {
                PersistedLineageRowV1::Unchanged { start, count } => {
                    let end = start.checked_add(count).ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed lineage run exceeds u32".to_owned(),
                        )
                    })?;
                    if count > 0 {
                        symbol(end - 1)?;
                    }
                    work.extend(
                        (start..end).map(|position| RowV1::Implicit(position as usize, None)),
                    );
                }
                PersistedLineageRowV1::UnchangedFrom { current, prior } => {
                    work.push(RowV1::Implicit(symbol(current)?, Some(prior)));
                }
                PersistedLineageRowV1::Candidate(candidate) => work.push(RowV1::Whole(*candidate)),
            }
        }
        let prior_generation = self.prior_generation;
        let batches = work.chunks(LINEAGE_BATCH_ROWS_V1).collect::<Vec<_>>();
        let expanded = collect_bounded_ordered(&batches, |batch, _worker| {
            batch
                .iter()
                .map(|row| match row {
                    RowV1::Whole(candidate) => Ok(candidate.clone()),
                    RowV1::Implicit(position, prior) => {
                        let prior_generation = prior_generation.as_ref().ok_or_else(|| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed lineage has implicit rows without a prior generation"
                                    .to_owned(),
                            )
                        })?;
                        let symbol = roster[*position];
                        SymbolLineageCandidateV1::exact_unchanged(
                            prior_generation,
                            current_generation,
                            prior.as_ref().unwrap_or(&symbol.occurrence),
                            symbol,
                        )
                        .map_err(lineage_error)
                    }
                })
                .collect::<Result<Vec<_>, CodeIndexProductionErrorV1>>()
        })?;
        Ok(expanded.into_iter().flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        ComplexityAnalysisV1, ContentDigest, FileIdentityDigest, SymbolIdentityDigest,
    };

    use super::*;
    use crate::lineage::{GenerationSymbolIndexV1, SymbolLineageResolver};

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn generation(sequence: u64) -> CodeGenerationId {
        CodeGenerationId::new(format!("generation.v1.aaaaaaaa.{sequence:08}"))
            .expect("valid generation id")
    }

    fn record(occurrence: &str, identity: char, content: char) -> Arc<LineageSymbolRecordV1> {
        Arc::new(LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new(occurrence).expect("occurrence"),
            identity: SymbolIdentityDigest::new(digest(identity)).expect("identity"),
            qualified_name: format!("crate::{identity}"),
            simple_name: identity.to_string(),
            kind: "function".to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            line_span: 1,
            start_line: 0,
            signature: None,
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: FileIdentityDigest::new(digest('f')).expect("file identity"),
            content_digest: ContentDigest::new(digest(content)).expect("content"),
        })
    }

    #[test]
    fn continuity_rows_are_implicit_and_every_row_restores_exactly() {
        let prior = GenerationSymbolIndexV1::new(
            generation(1),
            vec![
                record("sym.a", 'a', '0'),
                record("sym.b", 'b', '1'),
                record("sym.c", 'c', '2'),
                record("sym.e", 'e', '4'),
                record("sym.f", 'f', '5'),
            ],
        )
        .expect("prior");
        let current = GenerationSymbolIndexV1::new(
            generation(2),
            vec![
                record("sym.a", 'a', '0'),
                // The same identity and content under a new occurrence.
                record("sym.b2", 'b', '1'),
                // The same identity with new content.
                record("sym.c", 'c', '3'),
                // No ancestor at all, so no row; the runs around it split.
                record("sym.d", 'd', '9'),
                record("sym.e", 'e', '4'),
                record("sym.f", 'f', '5'),
            ],
        )
        .expect("current");
        let lineage = SymbolLineageResolver::new()
            .resolve(&prior, &current)
            .expect("lineage");
        assert_eq!(lineage.len(), 5);
        let roster = occurrence_roster(current.symbols.iter());

        let persisted = PersistedLineageV1::compact(&lineage, &current.generation_id, &roster)
            .expect("compact");
        assert_eq!(persisted.prior_generation, Some(generation(1)));
        assert_eq!(
            persisted.rows,
            [
                PersistedLineageRowV1::Unchanged { start: 0, count: 1 },
                PersistedLineageRowV1::UnchangedFrom {
                    current: 1,
                    prior: SymbolOccurrenceId::new("sym.b").expect("occurrence"),
                },
                PersistedLineageRowV1::Candidate(Box::new(lineage[2].clone())),
                PersistedLineageRowV1::Unchanged { start: 4, count: 2 },
            ]
        );

        let bytes = serde_json::to_vec(&persisted).expect("serialize");
        let restored = serde_json::from_slice::<PersistedLineageV1>(&bytes)
            .expect("deserialize")
            .expand(&current.generation_id, &roster)
            .expect("expand");
        assert_eq!(restored, lineage);
    }

    #[test]
    fn rows_outside_the_roster_or_without_a_prior_generation_are_refused() {
        let symbols = [record("sym.a", 'a', '0')];
        let roster = occurrence_roster(symbols.iter());
        let outside = PersistedLineageV1 {
            prior_generation: Some(generation(1)),
            rows: vec![PersistedLineageRowV1::Unchanged { start: 0, count: 2 }],
        };
        assert!(outside.expand(&generation(2), &roster).is_err());
        let unanchored = PersistedLineageV1 {
            prior_generation: None,
            rows: vec![PersistedLineageRowV1::Unchanged { start: 0, count: 1 }],
        };
        assert!(unanchored.expand(&generation(2), &roster).is_err());
    }
}
