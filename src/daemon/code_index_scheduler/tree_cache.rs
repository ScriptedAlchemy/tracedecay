//! Bounded cache of prior Tree-sitter trees for incremental re-parse.
//!
//! The cache retains the previously parsed `Tree` for each file so a saved-file
//! edit can `InputEdit` the old tree and re-parse with it instead of parsing
//! from scratch. Tree reuse is a performance optimization only: canonical
//! content, descriptor, and chunk digests remain the product identity, so a
//! cache miss is always correct — it just costs a full parse.
//!
//! Structural cross-worktree isolation is enforced by construction: the cache
//! is stamped with the owning worktree's structural identity key and refuses to
//! serve entries committed under any other key. This makes the cross-worktree
//! reuse the contract forbids impossible, not merely checked.
//!
//! The cache is bounded by both entry count and total retained content bytes;
//! least-recently-used entries are evicted when either bound is exceeded.

use std::collections::HashMap;
use std::sync::Arc;

use tracedecay_domain::ContentDigest;
use tree_sitter::Tree;

pub(crate) const DEFAULT_MAX_ENTRIES: usize = 4_096;
pub(crate) const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// One committed (bytes, tree) pair to retain for a file.
pub(crate) struct SavedTreeInputV1 {
    pub path: String,
    pub bytes: Arc<[u8]>,
    pub digest: ContentDigest,
    pub tree: Tree,
}

/// A prior tree served for incremental re-parse.
pub(crate) struct RetainedTreeV1 {
    pub bytes: Arc<[u8]>,
    pub digest: ContentDigest,
    pub tree: Tree,
}

struct Entry {
    bytes: Arc<[u8]>,
    digest: ContentDigest,
    tree: Tree,
    tick: u64,
}

pub(crate) struct SavedTreeCacheV1 {
    identity_key: String,
    entries: HashMap<String, Entry>,
    max_entries: usize,
    max_bytes: usize,
    total_bytes: usize,
    tick: u64,
}

impl SavedTreeCacheV1 {
    pub(crate) fn new(identity_key: String) -> Self {
        Self::with_bounds(identity_key, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }

    pub(crate) fn with_bounds(identity_key: String, max_entries: usize, max_bytes: usize) -> Self {
        Self {
            identity_key,
            entries: HashMap::new(),
            max_entries: max_entries.max(1),
            max_bytes,
            total_bytes: 0,
            tick: 0,
        }
    }

    /// The prior retained tree for `path`, if any. Marks the entry as most
    /// recently used.
    pub(crate) fn get(&mut self, path: &str) -> Option<RetainedTreeV1> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(path)?;
        entry.tick = tick;
        Some(RetainedTreeV1 {
            bytes: Arc::clone(&entry.bytes),
            digest: entry.digest.clone(),
            tree: entry.tree.clone(),
        })
    }

    /// Replace the retained set with the trees admitted by a published
    /// generation.
    ///
    /// `identity_key` must equal the key this cache was stamped with; a mismatch
    /// clears the cache rather than mixing worktree identities. Only the paths
    /// present in `inputs` are retained afterward, so files that left the
    /// snapshot drop out immediately; LRU/byte eviction then enforces the bounds.
    pub(crate) fn commit_batch(&mut self, identity_key: &str, inputs: Vec<SavedTreeInputV1>) {
        if identity_key != self.identity_key {
            self.clear();
            return;
        }
        let retained: std::collections::HashSet<&str> =
            inputs.iter().map(|input| input.path.as_str()).collect();
        self.entries.retain(|path, entry| {
            let keep = retained.contains(path.as_str());
            if !keep {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes.len());
            }
            keep
        });
        for input in inputs {
            self.insert(input);
        }
        self.evict_to_bounds();
    }

    fn insert(&mut self, input: SavedTreeInputV1) {
        self.tick += 1;
        let size = input.bytes.len();
        if let Some(previous) = self.entries.insert(
            input.path,
            Entry {
                bytes: input.bytes,
                digest: input.digest,
                tree: input.tree,
                tick: self.tick,
            },
        ) {
            self.total_bytes = self.total_bytes.saturating_sub(previous.bytes.len());
        }
        self.total_bytes = self.total_bytes.saturating_add(size);
    }

    fn evict_to_bounds(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.tick)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&victim) {
                self.total_bytes = self.total_bytes.saturating_sub(entry.bytes.len());
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_index::chunks::content_digest;
    use tree_sitter::Parser;

    fn parse(source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::extraction::ts_provider::try_language("rust").expect("rust lang"))
            .expect("set language");
        parser.parse(source, None).expect("parse")
    }

    fn input(path: &str, source: &str) -> SavedTreeInputV1 {
        let bytes: Arc<[u8]> = Arc::from(source.as_bytes().to_vec());
        SavedTreeInputV1 {
            path: path.to_owned(),
            digest: content_digest(&bytes),
            tree: parse(source),
            bytes,
        }
    }

    #[test]
    fn entry_bound_evicts_least_recently_used() {
        let mut cache = SavedTreeCacheV1::with_bounds("sha256:key".to_owned(), 2, usize::MAX);
        cache.commit_batch(
            "sha256:key",
            vec![
                input("a.rs", "fn a() {}\n"),
                input("b.rs", "fn b() {}\n"),
                input("c.rs", "fn c() {}\n"),
            ],
        );
        assert_eq!(cache.len(), 2, "entry bound evicts down to capacity");
    }

    #[test]
    fn byte_bound_evicts_until_within_budget() {
        // Each source is well over 8 bytes, so a tiny byte budget forces the
        // cache down to a single retained entry.
        let mut cache = SavedTreeCacheV1::with_bounds("sha256:key".to_owned(), 100, 16);
        cache.commit_batch(
            "sha256:key",
            vec![
                input("a.rs", "fn alpha() {}\n"),
                input("b.rs", "fn beta() {}\n"),
            ],
        );
        assert!(cache.len() <= 1, "byte budget bounds the retained set");
        assert!(cache.total_bytes() <= 16);
    }

    #[test]
    fn foreign_identity_key_clears_the_cache() {
        let mut cache = SavedTreeCacheV1::new("sha256:worktree-a".to_owned());
        cache.commit_batch("sha256:worktree-a", vec![input("a.rs", "fn a() {}\n")]);
        assert_eq!(cache.len(), 1);
        // A commit stamped with a different worktree identity must never merge:
        // the cache clears rather than serve a cross-worktree tree.
        cache.commit_batch("sha256:worktree-b", vec![input("b.rs", "fn b() {}\n")]);
        assert_eq!(cache.len(), 0, "foreign identity clears, never mixes");
    }

    #[test]
    fn retained_tree_round_trips_for_input_edit() {
        let mut cache = SavedTreeCacheV1::new("sha256:key".to_owned());
        cache.commit_batch("sha256:key", vec![input("a.rs", "fn a() {}\n")]);
        let retained = cache.get("a.rs").expect("prior tree retained");
        assert_eq!(retained.bytes.as_ref(), b"fn a() {}\n");
        assert!(cache.get("missing.rs").is_none(), "a miss just full-parses");
    }
}
