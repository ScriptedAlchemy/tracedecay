use std::collections::BTreeSet;
use std::time::Instant;

use fst::{IntoStreamer, Set, Streamer, automaton::Levenshtein};
use roaring::RoaringBitmap;

const NGRAM_PAGE_BACKING_BYTES: usize = 1024 * 1024;
const NGRAM_PAGE_ENTRY_CAPACITY: usize = NGRAM_PAGE_BACKING_BYTES / std::mem::size_of::<u64>();
const NGRAM_MINIMUM_PAGE_ENTRY_CAPACITY: usize = 1024;
const NGRAM_MINIMUM_PAGE_BYTES: usize =
    NGRAM_MINIMUM_PAGE_ENTRY_CAPACITY * std::mem::size_of::<u64>();
const NGRAM_MAXIMUM_PAGE_COUNT: usize = 512;
pub(super) const LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED: &str =
    "lexical projection exceeded its build deadline";
pub(super) const LEXICAL_PROJECTION_NGRAM_MEMORY_BUDGET_EXCEEDED: &str =
    "lexical projection n-gram posting memory budget exceeded";

#[derive(Debug, Default)]
pub(super) struct ByteNgramPostings {
    pages: Vec<PackedNgramPage>,
    entry_count: usize,
}

#[derive(Debug)]
struct PackedNgramPage {
    entries: Vec<u64>,
    // Full pages are immutable sorted runs. Only the single trailing page is
    // scanned; this avoids a second O(generation) merge allocation at finish.
    sorted: bool,
}

impl PackedNgramPage {
    fn try_new(capacity: usize) -> Result<Self, String> {
        debug_assert!((1..=NGRAM_PAGE_ENTRY_CAPACITY).contains(&capacity));
        let mut entries = Vec::new();
        entries.try_reserve_exact(capacity).map_err(|error| {
            format!("lexical projection could not allocate a bounded n-gram page: {error}")
        })?;
        debug_assert_eq!(entries.capacity(), capacity);
        Ok(Self {
            entries,
            sorted: false,
        })
    }

    fn push(&mut self, entry: u64) {
        debug_assert!(self.entries.len() < self.entries.capacity());
        self.entries.push(entry);
        if self.entries.len() == self.entries.capacity() {
            self.entries.sort_unstable();
            self.sorted = true;
        }
    }

    fn remaining_capacity(&self) -> usize {
        self.entries.capacity().saturating_sub(self.entries.len())
    }

    fn add_documents(&self, ngram: u32, documents: &mut RoaringBitmap) {
        if self.sorted {
            let lower = self
                .entries
                .partition_point(|entry| unpack_ngram(*entry) < ngram);
            let upper = self.entries[lower..]
                .partition_point(|entry| unpack_ngram(*entry) == ngram)
                .saturating_add(lower);
            for entry in &self.entries[lower..upper] {
                documents.insert(unpack_document(*entry));
            }
        } else {
            for entry in self
                .entries
                .iter()
                .filter(|entry| unpack_ngram(**entry) == ngram)
            {
                documents.insert(unpack_document(*entry));
            }
        }
    }
}

impl ByteNgramPostings {
    pub(super) fn insert_document(
        &mut self,
        document: u32,
        bytes: &[u8],
        budget: &mut ByteNgramBudget,
    ) -> Result<(), String> {
        crate::hotpath_metrics::measure_frequent("query.artifact.ngram.insert_document", || {
            self.insert_document_inner(document, bytes, budget)
        })
    }

    fn insert_document_inner(
        &mut self,
        document: u32,
        bytes: &[u8],
        budget: &mut ByteNgramBudget,
    ) -> Result<(), String> {
        let scratch_entries = (1..=bytes.len().min(3)).try_fold(0usize, |entries, width| {
            entries
                .checked_add(bytes.len().saturating_sub(width).saturating_add(1))
                .ok_or_else(|| budget.exceeded())
        })?;
        let scratch_bytes = scratch_entries
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or_else(|| budget.exceeded())?;
        budget.ensure_peak(scratch_bytes, 0)?;

        let mut unique = Vec::new();
        unique.try_reserve_exact(scratch_entries).map_err(|error| {
            format!("lexical projection could not allocate bounded n-gram scratch: {error}")
        })?;
        debug_assert_eq!(unique.capacity(), scratch_entries);
        for width in 1..=bytes.len().min(3) {
            unique.extend(bytes.windows(width).map(pack_byte_ngram));
        }
        unique.sort_unstable();
        unique.dedup();
        if unique.is_empty() {
            return Ok(());
        }

        let available_entries = self
            .pages
            .last()
            .map(PackedNgramPage::remaining_capacity)
            .unwrap_or_default();
        let mut entries_to_allocate = unique.len().saturating_sub(available_entries);
        let mut next_page_capacity =
            self.pages
                .last()
                .map_or(NGRAM_MINIMUM_PAGE_ENTRY_CAPACITY, |page| {
                    page.entries
                        .capacity()
                        .saturating_mul(2)
                        .min(NGRAM_PAGE_ENTRY_CAPACITY)
                });
        let mut required_pages = 0usize;
        let mut page_bytes = 0usize;
        while entries_to_allocate > 0 {
            let capacity = entries_to_allocate
                .max(next_page_capacity)
                .min(NGRAM_PAGE_ENTRY_CAPACITY);
            page_bytes = page_bytes
                .checked_add(
                    capacity
                        .checked_mul(std::mem::size_of::<u64>())
                        .ok_or_else(|| budget.exceeded())?,
                )
                .ok_or_else(|| budget.exceeded())?;
            required_pages = required_pages
                .checked_add(1)
                .ok_or_else(|| budget.exceeded())?;
            entries_to_allocate = entries_to_allocate.saturating_sub(capacity);
            next_page_capacity = capacity.saturating_mul(2).min(NGRAM_PAGE_ENTRY_CAPACITY);
        }
        let descriptor_capacity = if self.pages.capacity() == 0 {
            budget.page_descriptor_capacity()
        } else {
            0
        };
        if self.pages.len().saturating_add(required_pages)
            > self.pages.capacity().max(descriptor_capacity)
        {
            return Err(budget.exceeded());
        }
        let descriptor_bytes = descriptor_capacity
            .checked_mul(std::mem::size_of::<PackedNgramPage>())
            .ok_or_else(|| budget.exceeded())?;
        let retained_bytes = descriptor_bytes
            .checked_add(page_bytes)
            .ok_or_else(|| budget.exceeded())?;
        budget.reserve_retained_at_peak(scratch_bytes, retained_bytes)?;

        if descriptor_capacity > 0
            && let Err(error) = self.pages.try_reserve_exact(descriptor_capacity)
        {
            budget.release_retained(retained_bytes);
            return Err(format!(
                "lexical projection could not allocate bounded n-gram page metadata: {error}"
            ));
        }
        debug_assert!(descriptor_capacity == 0 || self.pages.capacity() == descriptor_capacity);
        let mut entries_to_allocate = unique.len().saturating_sub(available_entries);
        let mut next_page_capacity =
            self.pages
                .last()
                .map_or(NGRAM_MINIMUM_PAGE_ENTRY_CAPACITY, |page| {
                    page.entries
                        .capacity()
                        .saturating_mul(2)
                        .min(NGRAM_PAGE_ENTRY_CAPACITY)
                });
        let mut allocated_page_bytes = 0usize;
        while entries_to_allocate > 0 {
            let capacity = entries_to_allocate
                .max(next_page_capacity)
                .min(NGRAM_PAGE_ENTRY_CAPACITY);
            let allocated_bytes = capacity.saturating_mul(std::mem::size_of::<u64>());
            match PackedNgramPage::try_new(capacity) {
                Ok(page) => self.pages.push(page),
                Err(error) => {
                    budget.release_retained(page_bytes.saturating_sub(allocated_page_bytes));
                    return Err(error);
                }
            }
            allocated_page_bytes = allocated_page_bytes.saturating_add(allocated_bytes);
            entries_to_allocate = entries_to_allocate.saturating_sub(capacity);
            next_page_capacity = capacity.saturating_mul(2).min(NGRAM_PAGE_ENTRY_CAPACITY);
        }

        let mut page_index = self
            .pages
            .iter()
            .position(|page| page.remaining_capacity() > 0)
            .ok_or_else(|| "lexical n-gram page reservation was incomplete".to_owned())?;
        for ngram in unique {
            loop {
                let page = self
                    .pages
                    .get_mut(page_index)
                    .ok_or_else(|| "lexical n-gram page reservation was incomplete".to_owned())?;
                if page.remaining_capacity() > 0 {
                    page.push(pack_posting(ngram, document));
                    break;
                }
                page_index = page_index
                    .checked_add(1)
                    .ok_or_else(|| "lexical n-gram page index overflowed".to_owned())?;
            }
            self.entry_count = self.entry_count.saturating_add(1);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn from_documents<'a>(
        documents: impl IntoIterator<Item = &'a [u8]>,
        budget: &mut ByteNgramBudget,
        deadline: Option<Instant>,
    ) -> Result<Self, String> {
        let mut postings = Self::default();
        for (document, bytes) in documents.into_iter().enumerate() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED.to_owned());
            }
            let document = u32::try_from(document)
                .map_err(|_| "posting document id exceeds u32".to_owned())?;
            postings.insert_document(document, bytes, budget)?;
        }
        Ok(postings)
    }

    #[hotpath::measure(label = "query.artifact.ngram.candidate_documents")]
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
        let mut documents = self.documents_for_ngram(first);
        for ngram in ngrams {
            documents &= self.documents_for_ngram(ngram);
            if documents.is_empty() {
                break;
            }
        }
        documents
    }

    pub(super) fn retained_owned_bytes(&self) -> usize {
        self.pages.iter().fold(
            self.pages
                .capacity()
                .saturating_mul(std::mem::size_of::<PackedNgramPage>()),
            |bytes, page| {
                bytes.saturating_add(
                    page.entries
                        .capacity()
                        .saturating_mul(std::mem::size_of::<u64>()),
                )
            },
        )
    }

    fn documents_for_ngram(&self, ngram: u32) -> RoaringBitmap {
        let mut documents = RoaringBitmap::new();
        for page in &self.pages {
            page.add_documents(ngram, &mut documents);
        }
        documents
    }

    #[cfg(test)]
    fn page_count(&self) -> usize {
        self.pages.len()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ByteNgramBudget {
    consumed_bytes: usize,
    maximum_bytes: usize,
}

impl ByteNgramBudget {
    #[hotpath::skip]
    pub(super) const fn new(maximum_bytes: usize) -> Self {
        Self {
            consumed_bytes: 0,
            maximum_bytes,
        }
    }

    fn ensure_peak(&self, temporary_bytes: usize, retained_bytes: usize) -> Result<(), String> {
        let consumed = self
            .consumed_bytes
            .checked_add(temporary_bytes)
            .and_then(|bytes| bytes.checked_add(retained_bytes))
            .ok_or_else(|| self.exceeded())?;
        if consumed > self.maximum_bytes {
            return Err(self.exceeded());
        }
        Ok(())
    }

    fn reserve_retained_at_peak(
        &mut self,
        temporary_bytes: usize,
        retained_bytes: usize,
    ) -> Result<(), String> {
        self.ensure_peak(temporary_bytes, retained_bytes)?;
        self.consumed_bytes = self
            .consumed_bytes
            .checked_add(retained_bytes)
            .ok_or_else(|| self.exceeded())?;
        Ok(())
    }

    fn release_retained(&mut self, bytes: usize) {
        self.consumed_bytes = self.consumed_bytes.saturating_sub(bytes);
    }

    fn page_descriptor_capacity(&self) -> usize {
        self.maximum_bytes
            .checked_div(NGRAM_MINIMUM_PAGE_BYTES)
            .unwrap_or_default()
            .clamp(1, NGRAM_MAXIMUM_PAGE_COUNT)
    }

    #[cfg(test)]
    #[hotpath::skip]
    const fn consumed_bytes(&self) -> usize {
        self.consumed_bytes
    }

    fn exceeded(&self) -> String {
        format!(
            "{}: maximum {} bytes",
            LEXICAL_PROJECTION_NGRAM_MEMORY_BUDGET_EXCEEDED, self.maximum_bytes
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

fn pack_posting(ngram: u32, document: u32) -> u64 {
    (u64::from(ngram) << 32) | u64::from(document)
}

fn unpack_ngram(posting: u64) -> u32 {
    (posting >> 32) as u32
}

fn unpack_document(posting: u64) -> u32 {
    posting as u32
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
    #[hotpath::measure(label = "query.lane.fuzzy.build_index")]
    pub(super) fn from_terms<I, S>(terms: I, deadline: Option<Instant>) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut canonical_terms = BTreeSet::new();
        for term in terms {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED.to_owned());
            }
            canonical_terms.insert(term.as_ref().to_owned());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED.to_owned());
        }
        let terms = Set::from_iter(canonical_terms.into_iter().map(String::into_bytes))
            .map_err(|error| error.to_string())?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED.to_owned());
        }
        Ok(Self { terms })
    }

    #[hotpath::measure(label = "query.lane.fuzzy.terms_at_distance")]
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

    pub(super) fn retained_owned_bytes(&self) -> usize {
        self.terms.as_fst().size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ngram_postings_prune_without_false_negatives() {
        let mut budget = ByteNgramBudget::new(2 * 1024 * 1024);
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
    fn ngram_postings_charge_flat_page_capacity_and_preserve_candidates() {
        let documents = (0..70_000_u32)
            .map(|document| {
                [
                    (document & 0xff) as u8,
                    ((document >> 8) & 0xff) as u8,
                    ((document >> 16) & 0xff) as u8,
                ]
            })
            .collect::<Vec<_>>();
        let mut budget = ByteNgramBudget::new(8 * 1024 * 1024);
        let postings = ByteNgramPostings::from_documents(
            documents.iter().map(|document| document.as_slice()),
            &mut budget,
            None,
        )
        .expect("flat pages fit the resident budget");

        assert!(postings.page_count() > 1, "fixture must cross a page");
        assert_eq!(postings.retained_owned_bytes(), budget.consumed_bytes());
        for document in [0_u32, 65_535, 69_999] {
            assert_eq!(
                postings
                    .candidate_documents(&documents[document as usize])
                    .iter()
                    .collect::<Vec<_>>(),
                vec![document]
            );
        }
    }

    #[test]
    fn ngram_postings_refuse_before_allocating_a_page_or_unique_scratch() {
        let bytes = vec![b'a'; 256 * 1024];
        let mut postings = ByteNgramPostings::default();
        let mut budget = ByteNgramBudget::new(NGRAM_PAGE_BACKING_BYTES);

        let error = postings
            .insert_document(0, &bytes, &mut budget)
            .expect_err("page plus document scratch must exceed the budget");

        assert!(error.contains("n-gram posting memory budget"));
        assert_eq!(postings.retained_owned_bytes(), 0);
        assert_eq!(budget.consumed_bytes(), 0);
    }

    #[test]
    fn request_deadline_overrides_crate_lexical_fallback() {
        assert_eq!(
            super::super::lexical_projection_build_deadline_micros(None),
            super::super::LEXICAL_PROJECTION_BUILD_DEADLINE_MICROS_V1
        );
        assert_eq!(
            super::super::lexical_projection_build_deadline_micros(Some(12)),
            12
        );
        assert_eq!(
            super::super::lexical_projection_build_deadline_micros(Some(0)),
            0
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
    fn fuzzy_index_rejects_an_already_expired_build_deadline() {
        let error = FuzzyTermIndex::from_terms(["alpha"], Some(Instant::now()))
            .expect_err("expired deadline must cover fuzzy index construction");
        assert_eq!(error, LEXICAL_PROJECTION_BUILD_DEADLINE_EXCEEDED);
    }

    #[test]
    fn fuzzy_enumeration_stops_at_the_remaining_budget() {
        let terms = ('!'..='~').map(|character| format!("aaaa{character}aaaaa"));
        let index = FuzzyTermIndex::from_terms(terms, None).expect("valid FST");
        let mut seen = BTreeSet::new();
        let slice = index
            .terms_at_distance("aaaaaaaaaa", 1, 3, &mut seen)
            .expect("bounded fuzzy search");

        assert_eq!(slice.terms.len(), 3);
        assert!(slice.examined <= 4);
    }
}
