//! Compatibility surface for Cursor compaction admission.
//!
//! Compaction itself is replayed by the daemon after the host event is durable;
//! this module never runs transcript capture, LCM, or `cursor-agent` inline.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CursorPreCompactOutcome {
    pub status: String,
    pub reason: String,
    pub summary_nodes_created: usize,
    pub summary_node_ids: Vec<String>,
}

pub async fn cursor_pre_compact_via_daemon(event_json: &str) -> CursorPreCompactOutcome {
    match super::cursor::submit_cursor_event("preCompact", event_json).await {
        super::cursor::CursorAdmissionReceipt::Deferred => CursorPreCompactOutcome {
            status: "deferred".to_owned(),
            reason: "durably queued for daemon compaction".to_owned(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        },
        super::cursor::CursorAdmissionReceipt::Unavailable => CursorPreCompactOutcome {
            status: "unavailable".to_owned(),
            reason: "daemon admission is unavailable".to_owned(),
            summary_nodes_created: 0,
            summary_node_ids: Vec::new(),
        },
    }
}
