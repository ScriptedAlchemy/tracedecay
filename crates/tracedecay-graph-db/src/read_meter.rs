//! Per-request read accounting on a verified snapshot lease.
//!
//! A request attaches one meter to the snapshot it reads through
//! ([`crate::VerifiedGraphSnapshot::metered`]); every point read and fan-out
//! that lease serves is counted at the same boundary its Hotpath span
//! measures, so the receipt and a profile agree on what a call did.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{GraphProperty, GraphPropertyName};

/// Which store served a lease read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphReadStore {
    /// A sealed generation's compacted per-generation store.
    Sealed,
    /// The shared staging database.
    Staging,
}

/// Counters one request accumulates across every read its lease serves.
#[derive(Debug, Default)]
pub struct GraphReadMeter {
    sealed_point_reads: AtomicU64,
    staging_point_reads: AtomicU64,
    adjacency_queries: AtomicU64,
    adjacency_rows: AtomicU64,
    bytes_hydrated: AtomicU64,
}

/// A reading of a [`GraphReadMeter`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphReadCost {
    pub sealed_point_reads: u64,
    pub staging_point_reads: u64,
    pub adjacency_queries: u64,
    pub adjacency_rows: u64,
    pub bytes_hydrated: u64,
}

impl GraphReadMeter {
    #[must_use]
    pub fn cost(&self) -> GraphReadCost {
        GraphReadCost {
            sealed_point_reads: self.sealed_point_reads.load(Ordering::Relaxed),
            staging_point_reads: self.staging_point_reads.load(Ordering::Relaxed),
            adjacency_queries: self.adjacency_queries.load(Ordering::Relaxed),
            adjacency_rows: self.adjacency_rows.load(Ordering::Relaxed),
            bytes_hydrated: self.bytes_hydrated.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_point_read(&self, store: GraphReadStore, bytes: u64) {
        match store {
            GraphReadStore::Sealed => &self.sealed_point_reads,
            GraphReadStore::Staging => &self.staging_point_reads,
        }
        .fetch_add(1, Ordering::Relaxed);
        self.bytes_hydrated.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn record_fanout(&self, rows: u64, bytes: u64) {
        self.adjacency_queries.fetch_add(1, Ordering::Relaxed);
        self.adjacency_rows.fetch_add(rows, Ordering::Relaxed);
        self.bytes_hydrated.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Payload bytes of decoded properties: scalar widths plus string and byte
/// lengths.
pub(crate) fn property_bytes(properties: &BTreeMap<GraphPropertyName, GraphProperty>) -> u64 {
    properties
        .values()
        .map(|property| match property {
            GraphProperty::Bool(_) => 1,
            GraphProperty::I64(_) | GraphProperty::F64(_) => 8,
            GraphProperty::String(value) => value.len() as u64,
            GraphProperty::Bytes(value) => value.len() as u64,
        })
        .sum()
}
