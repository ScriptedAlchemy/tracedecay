//! Graph search is bound to the dashboard's launch project.
//!
//! A listener started from one enrolled repo cannot silently search another.
//! Wrong-project and empty-scope requests fail closed with a typed omission
//! reason instead of a successful empty hit list.

/// Coverage omission code for a search that named a project this listener
/// cannot serve.
pub const WRONG_PROJECT_REASON: &str = "wrong_project";

/// Coverage omission code when the listener has no project to search.
pub const EMPTY_SCOPE_REASON: &str = "empty_scope";

/// Why graph search must not run against this request's project selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphSearchScopeRefusal {
    WrongProject { requested: String, bound: String },
    EmptyScope,
}

impl GraphSearchScopeRefusal {
    /// Typed omission reason the envelope carries. The code is the first
    /// token so clients can switch on it; the rest is the operator sentence.
    #[must_use]
    pub fn omission_reason(&self) -> String {
        match self {
            Self::WrongProject { requested, bound } => format!(
                "{WRONG_PROJECT_REASON}: this dashboard is bound to {bound}; \
                 it does not search {requested}. Rebound with \
                 `tracedecay dashboard --path <repo>` to serve that project"
            ),
            Self::EmptyScope => format!(
                "{EMPTY_SCOPE_REASON}: this dashboard has no bound project to search; \
                 start it from an enrolled repo or pass --path <repo>"
            ),
        }
    }
}

/// Refuse a search that is not aimed at the launch project, or that has no
/// project to search.
///
/// `bound_project_id` is the project this dashboard listener was started for.
/// `requested_project_id` is an explicit selector (`?project_id=` or the
/// `/api/projects/{id}/…` gateway). `None` means "search the bound project".
#[must_use]
pub fn refuse_graph_search_scope(
    bound_project_id: Option<&str>,
    requested_project_id: Option<&str>,
) -> Option<GraphSearchScopeRefusal> {
    let Some(bound) = bound_project_id.filter(|id| !id.is_empty()) else {
        return Some(GraphSearchScopeRefusal::EmptyScope);
    };
    match requested_project_id.filter(|id| !id.is_empty()) {
        Some(requested) if requested != bound => Some(GraphSearchScopeRefusal::WrongProject {
            requested: requested.to_owned(),
            bound: bound.to_owned(),
        }),
        Some(_) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EMPTY_SCOPE_REASON, GraphSearchScopeRefusal, WRONG_PROJECT_REASON,
        refuse_graph_search_scope,
    };

    #[test]
    fn bound_project_search_without_selector_is_admitted() {
        assert_eq!(refuse_graph_search_scope(Some("proj_federati"), None), None);
    }

    #[test]
    fn matching_selector_is_admitted() {
        assert_eq!(
            refuse_graph_search_scope(Some("proj_federati"), Some("proj_federati")),
            None
        );
    }

    #[test]
    fn other_enrolled_repo_is_wrong_project_not_empty_hits() {
        let refusal = refuse_graph_search_scope(Some("proj_federati"), Some("proj_grok_bot_cli"))
            .expect("other project must fail closed");
        assert_eq!(
            refusal,
            GraphSearchScopeRefusal::WrongProject {
                requested: "proj_grok_bot_cli".to_owned(),
                bound: "proj_federati".to_owned(),
            }
        );
        let reason = refusal.omission_reason();
        assert!(reason.starts_with(WRONG_PROJECT_REASON), "{reason}");
        assert!(reason.contains("--path"), "{reason}");
        assert!(reason.contains("proj_grok_bot_cli"), "{reason}");
    }

    #[test]
    fn missing_bound_project_is_empty_scope() {
        let refusal =
            refuse_graph_search_scope(None, None).expect("unbound dashboard must fail closed");
        assert_eq!(refusal, GraphSearchScopeRefusal::EmptyScope);
        let reason = refusal.omission_reason();
        assert!(reason.starts_with(EMPTY_SCOPE_REASON), "{reason}");
        assert!(reason.contains("--path"), "{reason}");
    }

    #[test]
    fn selector_on_an_unbound_dashboard_is_still_empty_scope() {
        assert_eq!(
            refuse_graph_search_scope(None, Some("proj_federati")),
            Some(GraphSearchScopeRefusal::EmptyScope)
        );
    }
}
