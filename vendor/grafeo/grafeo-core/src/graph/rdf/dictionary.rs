//! Term dictionary for dictionary-encoded triple scans.
//!
//! Maps RDF `Term` values to compact `u32` IDs for efficient join processing.
//! During query execution, triple scans emit integer term IDs instead of strings,
//! and a `DictResolve` step at the result boundary converts them back.

use super::term::Term;
use hashbrown::HashMap;

/// Bidirectional mapping between RDF terms and compact integer IDs.
///
/// Built lazily on first query (or during `collect_statistics`). Invalidated
/// by any store mutation so the planner falls back to string columns.
#[derive(Debug, Clone)]
pub struct TermDictionary {
    term_to_id: HashMap<Term, u32>,
    id_to_term: Vec<Term>,
}

impl TermDictionary {
    /// Creates an empty dictionary.
    #[must_use]
    pub fn new() -> Self {
        Self {
            term_to_id: HashMap::new(),
            id_to_term: Vec::new(),
        }
    }

    /// Creates a dictionary pre-sized for the given capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            term_to_id: HashMap::with_capacity(capacity),
            id_to_term: Vec::with_capacity(capacity),
        }
    }

    /// Inserts a term and returns its ID. If the term already exists, returns
    /// the existing ID.
    ///
    /// # Panics
    ///
    /// Panics if the dictionary exceeds `u32::MAX` entries.
    pub fn get_or_insert(&mut self, term: &Term) -> u32 {
        if let Some(&id) = self.term_to_id.get(term) {
            return id;
        }
        let id: u32 = self
            .id_to_term
            .len()
            .try_into()
            .expect("TermDictionary exceeded u32::MAX entries");
        self.id_to_term.push(term.clone());
        self.term_to_id.insert(term.clone(), id);
        id
    }

    /// Looks up the ID for a term, returning `None` if unknown.
    #[must_use]
    pub fn get_id(&self, term: &Term) -> Option<u32> {
        self.term_to_id.get(term).copied()
    }

    /// Resolves a term ID back to the original term.
    #[must_use]
    pub fn get_term(&self, id: u32) -> Option<&Term> {
        self.id_to_term.get(id as usize)
    }

    /// Returns the number of distinct terms in the dictionary.
    #[must_use]
    pub fn len(&self) -> usize {
        self.id_to_term.len()
    }

    /// Returns true if the dictionary is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.id_to_term.is_empty()
    }
}

impl Default for TermDictionary {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut dict = TermDictionary::new();
        let t1 = Term::iri("http://example.org/alix");
        let t2 = Term::literal("Alix");
        let t3 = Term::iri("http://example.org/gus");

        let id1 = dict.get_or_insert(&t1);
        let id2 = dict.get_or_insert(&t2);
        let id3 = dict.get_or_insert(&t3);

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_eq!(dict.get_or_insert(&t1), id1, "same term returns same ID");

        assert_eq!(dict.get_term(id1), Some(&t1));
        assert_eq!(dict.get_term(id2), Some(&t2));
        assert_eq!(dict.get_term(id3), Some(&t3));
        assert_eq!(dict.get_id(&t1), Some(id1));
        assert_eq!(dict.len(), 3);
    }

    #[test]
    fn unknown_term_returns_none() {
        let dict = TermDictionary::new();
        assert_eq!(dict.get_id(&Term::iri("http://unknown")), None);
        assert_eq!(dict.get_term(999), None);
    }

    #[test]
    fn with_capacity_works() {
        let dict = TermDictionary::with_capacity(100);
        assert!(dict.is_empty());
        assert_eq!(dict.len(), 0);
    }

    #[test]
    fn default_creates_empty() {
        let dict = TermDictionary::default();
        assert!(dict.is_empty());
    }

    #[test]
    fn stable_ids_across_inserts() {
        let mut dict = TermDictionary::new();
        let t1 = Term::iri("http://example.org/a");
        let t2 = Term::literal("value");
        let id1 = dict.get_or_insert(&t1);
        let id2 = dict.get_or_insert(&t2);
        // IDs should be sequential starting from 0
        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        // Re-insert should return same IDs
        assert_eq!(dict.get_or_insert(&t1), 0);
        assert_eq!(dict.get_or_insert(&t2), 1);
        assert_eq!(dict.len(), 2);
    }

    #[test]
    fn mixed_term_types() {
        let mut dict = TermDictionary::new();
        let iri = Term::iri("http://example.org/x");
        let lit = Term::literal("hello");
        let blank = Term::blank("b0");
        let lang = Term::lang_literal("bonjour".to_string(), "fr".to_string());
        dict.get_or_insert(&iri);
        dict.get_or_insert(&lit);
        dict.get_or_insert(&blank);
        dict.get_or_insert(&lang);
        assert_eq!(dict.len(), 4);
        assert!(dict.get_id(&iri).is_some());
        assert!(dict.get_id(&blank).is_some());
        assert!(dict.get_id(&lang).is_some());
    }
}
