use serde_json::Value;

/// Canonical taxonomy of `tracedecay_fact_store` wire actions.
///
/// This table is the single source of truth for action capabilities: the
/// handler gates cross-project writes and untracked read variants through it,
/// and the generic MCP dispatcher reaches it via [`needs_operation_context`]
/// instead of hardcoding action names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FactStoreAction {
    Add,
    Update,
    Remove,
    Search,
    Probe,
    Related,
    Reason,
    List,
    Contradict,
    Get,
}

impl FactStoreAction {
    pub(super) fn parse(action: &str) -> Option<Self> {
        Some(match action {
            "add" => Self::Add,
            "update" => Self::Update,
            "remove" => Self::Remove,
            "search" => Self::Search,
            "probe" => Self::Probe,
            "related" => Self::Related,
            "reason" => Self::Reason,
            "list" => Self::List,
            "contradict" => Self::Contradict,
            "get" => Self::Get,
            _ => return None,
        })
    }

    /// Actions that mutate canonical memory facts.
    pub(super) fn writes(self) -> bool {
        matches!(self, Self::Add | Self::Update | Self::Remove)
    }

    /// Actions with an untracked read variant for cross-project dispatch
    /// (retrieval accounting stays local to the owning project).
    fn has_untracked(self) -> bool {
        matches!(
            self,
            Self::Search | Self::Probe | Self::Related | Self::Reason | Self::List
        )
    }

    /// Mutations and locally-accounted retrievals both write canonical
    /// memory events, so both need a daemon-issued replay identity.
    fn needs_context(self) -> bool {
        self.writes() || self.has_untracked()
    }
}

/// Whether a memory tool call needs a daemon-issued operation context: a
/// trusted replay identity the MCP server derives from the JSON-RPC envelope
/// id. The generic dispatcher queries this capability so the action taxonomy
/// stays owned by this module.
pub(crate) fn needs_operation_context(tool_name: &str, arguments: &Value) -> bool {
    match tool_name {
        "tracedecay_fact_feedback" => true,
        "tracedecay_fact_store" => arguments
            .get("action")
            .and_then(Value::as_str)
            .and_then(FactStoreAction::parse)
            .is_some_and(FactStoreAction::needs_context),
        _ => false,
    }
}
