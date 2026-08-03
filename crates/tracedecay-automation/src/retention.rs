use serde::{Deserialize, Serialize};

/// Default retention window for `analytics_events`, in days. Analytics rows
/// are a derived signal, so a generous six-month window loses nothing that
/// cannot be recomputed from the source transcripts.
pub const DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS: u32 = 180;

/// Per-table retention windows. A `None` window disables pruning for that
/// table (unlimited retention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Retention window for `analytics_events`. Defaults to
    /// [`DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS`].
    #[serde(default = "default_analytics_events_days")]
    pub analytics_events_days: Option<u32>,
    /// Retention window for `session_messages`. Defaults to `None`
    /// (unlimited): this is part of the lossless session record.
    #[serde(default)]
    pub session_messages_days: Option<u32>,
    /// Retention window for `lcm_raw_messages`. Defaults to `None`
    /// (unlimited): this is part of the lossless session record.
    #[serde(default)]
    pub lcm_raw_messages_days: Option<u32>,
}

fn default_analytics_events_days() -> Option<u32> {
    Some(DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS)
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            analytics_events_days: default_analytics_events_days(),
            session_messages_days: None,
            lcm_raw_messages_days: None,
        }
    }
}

impl RetentionConfig {
    /// Window configured for `table`, in days (`None` = unlimited).
    pub fn window_days(&self, table: RetentionTable) -> Option<u32> {
        match table {
            RetentionTable::AnalyticsEvents => self.analytics_events_days,
            RetentionTable::SessionMessages => self.session_messages_days,
            RetentionTable::LcmRawMessages => self.lcm_raw_messages_days,
        }
    }
}

/// A prunable telemetry table. The variants map to a fixed table/column pair,
/// so the SQL never interpolates untrusted identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTable {
    /// `analytics_events` (global DB), pruned by `timestamp`.
    AnalyticsEvents,
    /// `session_messages` (global DB), pruned by `timestamp`.
    SessionMessages,
    /// `lcm_raw_messages` (per-store LCM DB), pruned by `timestamp`.
    LcmRawMessages,
}

impl RetentionTable {
    /// The three tables that live in the global database.
    pub const GLOBAL_TABLES: [RetentionTable; 3] = [
        Self::AnalyticsEvents,
        Self::SessionMessages,
        Self::LcmRawMessages,
    ];

    pub fn table_name(self) -> &'static str {
        match self {
            Self::AnalyticsEvents => "analytics_events",
            Self::SessionMessages => "session_messages",
            Self::LcmRawMessages => "lcm_raw_messages",
        }
    }
}
