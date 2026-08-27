//! Triple sink trait and built-in implementations.
//!
//! A [`TripleSink`] receives triples emitted by a parser during streaming parse.
//! This decouples parsing from storage: the same parser can collect into a `Vec`,
//! stream into a store via batched inserts, or count triples for validation.
//!
//! # Built-in sinks
//!
//! - [`VecSink`]: collects all triples into a `Vec<Triple>` (default for `parse()`)
//! - [`BatchInsertSink`]: buffers triples and flushes to an [`RdfStore`] in chunks
//! - [`CountSink`]: counts triples without storing them (dry-run validation)

use super::store::RdfStore;
use super::triple::Triple;

/// A consumer of parsed RDF triples.
///
/// Parsers call [`emit`](TripleSink::emit) for each triple they produce.
/// The sink decides what to do: collect, store, count, forward, etc.
pub trait TripleSink {
    /// Called for each parsed triple. Return `Err` to abort parsing.
    ///
    /// # Errors
    ///
    /// Implementations may return an error to signal that parsing should stop
    /// (e.g., a storage failure or a resource limit).
    fn emit(&mut self, triple: Triple) -> Result<(), String>;

    /// Called when parsing is complete. Flushes any buffered triples.
    ///
    /// # Errors
    ///
    /// Implementations may return an error if the final flush fails.
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Collects all emitted triples into a `Vec`.
///
/// This is the default sink used by [`TurtleParser::parse`](super::turtle::TurtleParser::parse).
pub struct VecSink {
    triples: Vec<Triple>,
}

impl VecSink {
    /// Creates a new `VecSink`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            triples: Vec::new(),
        }
    }

    /// Returns the collected triples, consuming the sink.
    #[must_use]
    pub fn into_triples(self) -> Vec<Triple> {
        self.triples
    }
}

impl Default for VecSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TripleSink for VecSink {
    fn emit(&mut self, triple: Triple) -> Result<(), String> {
        self.triples.push(triple);
        Ok(())
    }
}

/// Buffers triples and flushes to an [`RdfStore`] via `batch_insert` every `batch_size` triples.
///
/// This keeps memory bounded: at most `batch_size` triples are held in memory at once,
/// regardless of how large the input document is.
pub struct BatchInsertSink<'a> {
    store: &'a RdfStore,
    buffer: Vec<Triple>,
    batch_size: usize,
    total_inserted: usize,
}

impl<'a> BatchInsertSink<'a> {
    /// Creates a new sink that flushes to `store` every `batch_size` triples.
    ///
    /// A batch size of 10,000 is a good default: small enough to bound memory,
    /// large enough to amortize lock overhead in `batch_insert`.
    #[must_use]
    pub fn new(store: &'a RdfStore, batch_size: usize) -> Self {
        debug_assert!(
            batch_size > 0,
            "batch_size must be > 0 to amortize flush overhead"
        );
        let batch_size = batch_size.max(1);
        Self {
            store,
            buffer: Vec::with_capacity(batch_size),
            batch_size,
            total_inserted: 0,
        }
    }

    /// Returns the total number of triples inserted (after deduplication).
    #[must_use]
    pub fn total_inserted(&self) -> usize {
        self.total_inserted
    }

    fn flush(&mut self) {
        if !self.buffer.is_empty() {
            let batch = std::mem::replace(&mut self.buffer, Vec::with_capacity(self.batch_size));
            self.total_inserted += self.store.batch_insert(batch);
        }
    }
}

impl TripleSink for BatchInsertSink<'_> {
    fn emit(&mut self, triple: Triple) -> Result<(), String> {
        self.buffer.push(triple);
        if self.buffer.len() >= self.batch_size {
            self.flush();
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.flush();
        Ok(())
    }
}

/// Counts emitted triples without storing them. Useful for dry-run validation.
pub struct CountSink {
    count: usize,
}

impl CountSink {
    /// Creates a new counting sink.
    #[must_use]
    pub fn new() -> Self {
        Self { count: 0 }
    }

    /// Returns the number of triples emitted.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }
}

impl Default for CountSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TripleSink for CountSink {
    fn emit(&mut self, _triple: Triple) -> Result<(), String> {
        self.count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::rdf::term::Term;

    fn sample_triple() -> Triple {
        Triple::new(
            Term::iri("http://example.org/s"),
            Term::iri("http://example.org/p"),
            Term::literal("o"),
        )
    }

    #[test]
    fn vec_sink_collects() {
        let mut sink = VecSink::new();
        sink.emit(sample_triple()).unwrap();
        sink.emit(sample_triple()).unwrap();
        assert_eq!(sink.into_triples().len(), 2);
    }

    #[test]
    fn count_sink_counts() {
        let mut sink = CountSink::new();
        sink.emit(sample_triple()).unwrap();
        sink.emit(sample_triple()).unwrap();
        sink.emit(sample_triple()).unwrap();
        assert_eq!(sink.count(), 3);
    }

    #[test]
    fn batch_insert_sink_flushes_on_finish() {
        let store = RdfStore::new();
        let mut sink = BatchInsertSink::new(&store, 100);
        sink.emit(sample_triple()).unwrap();
        // Not flushed yet (buffer < batch_size)
        assert_eq!(store.len(), 0);
        sink.finish().unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(sink.total_inserted(), 1);
    }

    #[test]
    fn batch_insert_sink_flushes_at_batch_size() {
        let store = RdfStore::new();
        let mut sink = BatchInsertSink::new(&store, 2);
        // Emit unique triples
        for i in 0..3 {
            sink.emit(Triple::new(
                Term::iri(format!("http://example.org/s{i}")),
                Term::iri("http://example.org/p"),
                Term::literal("o"),
            ))
            .unwrap();
        }
        // After 2 emits, first batch should have flushed (2 triples)
        // Third triple still in buffer
        assert_eq!(store.len(), 2);
        sink.finish().unwrap();
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn batch_insert_sink_deduplicates() {
        let store = RdfStore::new();
        let mut sink = BatchInsertSink::new(&store, 100);
        // Same triple twice
        sink.emit(sample_triple()).unwrap();
        sink.emit(sample_triple()).unwrap();
        sink.finish().unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(sink.total_inserted(), 1);
    }
}
