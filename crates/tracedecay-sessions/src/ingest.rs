/// Counters returned by an ingestion pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
}

impl TranscriptIngestStats {
    /// Accumulates another pass's counters without wrapping.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            sessions_upserted: self
                .sessions_upserted
                .saturating_add(other.sessions_upserted),
            messages_upserted: self
                .messages_upserted
                .saturating_add(other.messages_upserted),
        }
    }
}

/// Incremental position persisted between provider ingestion runs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StoredCursor {
    pub position: u64,
    pub mtime: u64,
    pub file_id: u64,
}

/// Rows read past a stored cursor and the resulting advanced cursor.
pub struct NewRows<T> {
    pub items: Vec<T>,
    pub new_cursor: StoredCursor,
}

#[cfg(test)]
mod tests {
    use super::TranscriptIngestStats;

    #[test]
    fn ingest_stats_merge_saturates() {
        let merged = TranscriptIngestStats {
            sessions_upserted: u64::MAX,
            messages_upserted: 2,
        }
        .merge(TranscriptIngestStats {
            sessions_upserted: 1,
            messages_upserted: 3,
        });

        assert_eq!(merged.sessions_upserted, u64::MAX);
        assert_eq!(merged.messages_upserted, 5);
    }
}
