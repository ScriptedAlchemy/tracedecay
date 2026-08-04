use serde::{Deserialize, Serialize};

/// One bounded project activity observation. Paths, source, messages, and
/// external identifiers are never retained in this payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivityObservedV1 {
    pub family: String,
    pub units: u64,
    pub detail: Option<String>,
}

impl ActivityObservedV1 {
    /// Activity detail is a finite vocabulary. Live source owners must never
    /// turn hook names, tool names, provider identifiers, or user content into
    /// a retained telemetry label.
    pub fn is_valid(&self) -> bool {
        self.units > 0
            && matches!(
                self.family.as_str(),
                "hook" | "session_ingest" | "code_index" | "tool_call" | "task"
            )
            && self.detail.as_deref().is_none_or(|detail| {
                crate::canonical_text::is_canonical_text_within(detail, 64)
                    && Self::allows_detail(&self.family, detail)
            })
    }

    /// Retain a detail only when the producer supplied one member of the
    /// family-specific vocabulary.
    #[must_use]
    pub fn bounded_detail(family: &str, detail: Option<&str>) -> Option<String> {
        detail
            .filter(|detail| {
                crate::canonical_text::is_canonical_text_within(detail, 64)
                    && Self::allows_detail(family, detail)
            })
            .map(str::to_owned)
    }

    fn allows_detail(family: &str, detail: &str) -> bool {
        match family {
            "hook" => matches!(
                detail,
                "session_boundary"
                    | "prompt_boundary"
                    | "tool_lifecycle"
                    | "saved_edit"
                    | "test_lifecycle"
                    | "opencode_lsp_updated"
            ),
            "session_ingest" => matches!(detail, "claude" | "codex" | "cursor" | "opencode"),
            "code_index" => matches!(detail, "hook_admitted" | "scheduler_reconciled"),
            "tool_call" => matches!(detail, "tracedecay"),
            "task" => matches!(
                detail,
                "leased"
                    | "running"
                    | "progress"
                    | "artifact"
                    | "cancellation_requested"
                    | "cancellation_acknowledged"
                    | "cancellation_escalated"
                    | "recovery_required"
                    | "succeeded"
                    | "failed"
                    | "timed_out"
                    | "cancelled"
            ),
            _ => false,
        }
    }
}
