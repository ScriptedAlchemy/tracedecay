use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use fst::{IntoStreamer, Set, Streamer, automaton::Levenshtein};
use roaring::RoaringBitmap;

const NGRAM_KEY_ESTIMATED_BYTES: usize = 64;
const NGRAM_DOCUMENT_POSTING_ESTIMATED_BYTES: usize = 8;
pub(super) const LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED: &str =
    "lexical projection exceeded its build deadline";

#[derive(Clone, Debug, Default)]
pub(super) struct ByteNgramPostings {
    postings: BTreeMap<u32, RoaringBitmap>,
}

impl ByteNgramPostings {
    pub(super) fn from_documents<'a>(
        documents: impl IntoIterator<Item = &'a [u8]>,
        budget: &mut ByteNgramBudget,
        deadline: Option<Instant>,
    ) -> Result<Self, String> {
        let mut postings = BTreeMap::<u32, RoaringBitmap>::new();
        for (document, bytes) in documents.into_iter().enumerate() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED.to_owned());
            }
            let document = u32::try_from(document)
                .map_err(|_| "posting document id exceeds u32".to_owned())?;
            for width in 1..=bytes.len().min(3) {
                let unique = bytes
                    .windows(width)
                    .map(pack_byte_ngram)
                    .collect::<BTreeSet<_>>();
                for ngram in unique {
                    match postings.entry(ngram) {
                        Entry::Vacant(entry) => {
                            budget.charge(NGRAM_KEY_ESTIMATED_BYTES)?;
                            budget.charge(NGRAM_DOCUMENT_POSTING_ESTIMATED_BYTES)?;
                            let mut posting = RoaringBitmap::new();
                            posting.insert(document);
                            entry.insert(posting);
                        }
                        Entry::Occupied(mut entry) => {
                            if entry.get_mut().insert(document) {
                                budget.charge(NGRAM_DOCUMENT_POSTING_ESTIMATED_BYTES)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(Self { postings })
    }

    pub(super) fn candidate_documents(&self, needle: &[u8]) -> RoaringBitmap {
        let width = needle.len().min(3);
        if width == 0 {
            return RoaringBitmap::new();
        }
        let ngrams = needle
            .windows(width)
            .map(pack_byte_ngram)
            .collect::<BTreeSet<_>>();
        let mut ngrams = ngrams.into_iter();
        let Some(first) = ngrams.next() else {
            return RoaringBitmap::new();
        };
        let Some(mut documents) = self.postings.get(&first).cloned() else {
            return RoaringBitmap::new();
        };
        for ngram in ngrams {
            let Some(posting) = self.postings.get(&ngram) else {
                return RoaringBitmap::new();
            };
            documents &= posting;
            if documents.is_empty() {
                break;
            }
        }
        documents
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ByteNgramBudget {
    consumed_bytes: usize,
    maximum_bytes: usize,
}

impl ByteNgramBudget {
    pub(super) const fn new(maximum_bytes: usize) -> Self {
        Self {
            consumed_bytes: 0,
            maximum_bytes,
        }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), String> {
        let consumed = self
            .consumed_bytes
            .checked_add(bytes)
            .ok_or_else(|| self.exceeded())?;
        if consumed > self.maximum_bytes {
            return Err(self.exceeded());
        }
        self.consumed_bytes = consumed;
        Ok(())
    }

    fn exceeded(&self) -> String {
        format!(
            "lexical projection exceeds the {}-byte n-gram posting memory budget",
            self.maximum_bytes
        )
    }
}

fn pack_byte_ngram(bytes: &[u8]) -> u32 {
    debug_assert!((1..=3).contains(&bytes.len()));
    bytes
        .iter()
        .enumerate()
        .fold((bytes.len() as u32) << 24, |packed, (index, byte)| {
            packed | (u32::from(*byte) << (index * 8))
        })
}

#[derive(Clone, Debug)]
pub(super) struct FuzzyTermIndex {
    terms: Set<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FuzzySearchSlice {
    pub(super) terms: Vec<String>,
    #[cfg(test)]
    pub(super) examined: usize,
}

impl FuzzyTermIndex {
    pub(super) fn from_terms<I, S>(terms: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let terms = terms
            .into_iter()
            .map(|term| term.as_ref().to_owned())
            .collect::<BTreeSet<_>>();
        let terms = Set::from_iter(terms.into_iter().map(String::into_bytes))
            .map_err(|error| error.to_string())?;
        Ok(Self { terms })
    }

    pub(super) fn terms_at_distance(
        &self,
        query: &str,
        distance: usize,
        limit: usize,
        seen: &mut BTreeSet<String>,
    ) -> Result<FuzzySearchSlice, String> {
        let automaton =
            Levenshtein::new(query, distance as u32).map_err(|error| error.to_string())?;
        let mut stream = self.terms.search(automaton).into_stream();
        let mut terms = Vec::with_capacity(limit);
        #[cfg(test)]
        let mut examined = 0;
        while terms.len() < limit {
            let Some(term) = stream.next() else {
                break;
            };
            #[cfg(test)]
            {
                examined += 1;
            }
            let term = std::str::from_utf8(term).map_err(|error| error.to_string())?;
            if term != query && seen.insert(term.to_owned()) {
                terms.push(term.to_owned());
            }
        }
        Ok(FuzzySearchSlice {
            terms,
            #[cfg(test)]
            examined,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ngram_postings_prune_without_false_negatives() {
        let mut budget = ByteNgramBudget::new(1024 * 1024);
        let postings = ByteNgramPostings::from_documents(
            [
                b"alpha connection refused".as_slice(),
                b"beta connection restored".as_slice(),
                b"gamma request completed".as_slice(),
            ],
            &mut budget,
            None,
        )
        .expect("bounded postings");

        assert_eq!(
            postings
                .candidate_documents(b"connection refused")
                .iter()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            postings
                .candidate_documents(b"connection")
                .iter()
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            postings
                .candidate_documents(b"missing diagnostic")
                .is_empty()
        );
    }

    #[test]
    fn ngram_postings_reject_over_budget_generations() {
        let mut budget = ByteNgramBudget::new(80);
        let error = ByteNgramPostings::from_documents([b"abcdef".as_slice()], &mut budget, None)
            .expect_err("posting memory must be bounded");
        assert!(error.contains("n-gram posting memory budget"));
    }

    #[test]
    fn lexical_projection_deadline_is_plan20_overridable() {
        assert_eq!(
            super::super::lexical_projection_build_deadline_micros(None),
            super::super::LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1
        );
        assert_eq!(
            super::super::lexical_projection_build_deadline_micros(Some(12)),
            12
        );
    }

    #[test]
    fn ngram_postings_reject_an_already_expired_build_deadline() {
        let mut budget = ByteNgramBudget::new(1024 * 1024);
        let error = ByteNgramPostings::from_documents(
            [b"abcdef".as_slice()],
            &mut budget,
            Some(Instant::now()),
        )
        .expect_err("expired deadline must fail closed");
        assert_eq!(error, LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED);
    }

    #[test]
    fn fuzzy_enumeration_stops_at_the_remaining_budget() {
        let terms = ('!'..='~').map(|character| format!("aaaa{character}aaaaa"));
        let index = FuzzyTermIndex::from_terms(terms).expect("valid FST");
        let mut seen = BTreeSet::new();
        let slice = index
            .terms_at_distance("aaaaaaaaaa", 1, 3, &mut seen)
            .expect("bounded fuzzy search");

        assert_eq!(slice.terms.len(), 3);
        assert!(slice.examined <= 4);
    }
}
