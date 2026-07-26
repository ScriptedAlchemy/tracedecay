/// Canonical taxonomy of `tracedecay_fact_store` wire actions.
///
/// This table is the single source of truth for action capabilities: the
/// handler gates cross-project writes and untracked read variants through it.
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
}
