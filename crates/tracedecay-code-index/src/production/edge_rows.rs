//! Persisted cross-file edge rows of one generation's evidence.
//!
//! Cross-file edges are the only edges a generation derives across its
//! files rather than reading from one file's artifacts, and deriving them
//! is a whole-corpus resolution. Sealing records them so a restore takes
//! them as sealed instead of resolving the corpus again. A row names both
//! endpoints by their position in the generation's occurrence-ordered symbol
//! roster; an edge with an endpoint outside the roster is kept whole.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, EdgeAuthorityV1, RelationEdgeKindV1, SourceSpan, SymbolOccurrenceId,
};

use super::CodeIndexProductionErrorV1;
use crate::lineage::LineageSymbolRecordV1;

/// `(from, to, kind, evidence start byte, evidence end byte)`, both
/// endpoints roster positions; the authority is the name resolution every
/// cross-file edge carries.
type RosterEdgeRowV1 = (u32, u32, RelationEdgeKindV1, u64, u64);

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedCrossFileEdgesV1 {
    roster: Vec<RosterEdgeRowV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    whole: Vec<CanonicalRelationEdgeV1>,
}

impl PersistedCrossFileEdgesV1 {
    /// The name-resolved edges of `edges`, which only cross-file resolution
    /// emits.
    pub(super) fn compact(
        edges: &[CanonicalRelationEdgeV1],
        roster: &[&LineageSymbolRecordV1],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let positions = roster
            .iter()
            .enumerate()
            .map(|(position, symbol)| {
                u32::try_from(position)
                    .map(|position| (&symbol.occurrence, position))
                    .map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed edge roster exceeds u32".to_owned(),
                        )
                    })
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let mut persisted = Self::default();
        for edge in edges
            .iter()
            .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
        {
            match (
                positions.get(&edge.from_occurrence),
                positions.get(&edge.to_occurrence),
            ) {
                (Some(&from), Some(&to)) => persisted.roster.push((
                    from,
                    to,
                    edge.kind,
                    edge.evidence_span.start_byte,
                    edge.evidence_span.end_byte,
                )),
                _ => persisted.whole.push(edge.clone()),
            }
        }
        Ok(persisted)
    }

    /// The sealed cross-file edges over the same roster encoding used.
    pub(super) fn expand(
        self,
        roster: &[&LineageSymbolRecordV1],
    ) -> Result<Vec<CanonicalRelationEdgeV1>, CodeIndexProductionErrorV1> {
        let occurrence = |position: u32| -> Result<SymbolOccurrenceId, CodeIndexProductionErrorV1> {
            usize::try_from(position)
                .ok()
                .and_then(|position| roster.get(position))
                .map(|symbol| symbol.occurrence.clone())
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed cross-file edge names a symbol outside the roster".to_owned(),
                    )
                })
        };
        let mut edges = Vec::with_capacity(self.roster.len() + self.whole.len());
        for (from, to, kind, start_byte, end_byte) in self.roster {
            edges.push(CanonicalRelationEdgeV1 {
                from_occurrence: occurrence(from)?,
                to_occurrence: occurrence(to)?,
                kind,
                authority: EdgeAuthorityV1::NameResolved,
                evidence_span: SourceSpan {
                    start_byte,
                    end_byte,
                },
            });
        }
        edges.extend(self.whole);
        Ok(edges)
    }
}
