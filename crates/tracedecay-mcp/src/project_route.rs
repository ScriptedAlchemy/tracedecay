//! Typed project-route failures shared by scope resolution and the
//! composition root's route cache.

use tracedecay_domain::errors::TraceDecayError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectRouteFailureKind {
    NotFound,
    NotAuthorized,
    Ambiguous,
    Unavailable,
}

impl ProjectRouteFailureKind {
    #[hotpath::skip]
    pub const fn reason_code(self) -> &'static str {
        match self {
            Self::NotFound => "project_route_not_found",
            Self::NotAuthorized => "project_route_not_authorized",
            Self::Ambiguous => "project_route_ambiguous",
            Self::Unavailable => "project_route_unavailable",
        }
    }

    fn from_reason_code(reason_code: &str) -> Option<Self> {
        [
            Self::NotFound,
            Self::NotAuthorized,
            Self::Ambiguous,
            Self::Unavailable,
        ]
        .into_iter()
        .find(|kind| kind.reason_code() == reason_code)
    }

    #[hotpath::skip]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRouteFailure {
    pub kind: ProjectRouteFailureKind,
    pub detail: String,
}

impl ProjectRouteFailure {
    pub fn into_error(self) -> TraceDecayError {
        TraceDecayError::project_route(self.kind.reason_code(), self.kind.retryable(), self.detail)
    }

    /// The route failure a registry or project-open error reports. Only a
    /// typed route refusal names its kind; every other failure leaves the
    /// route unavailable.
    pub fn from_selection_error(error: &TraceDecayError) -> Self {
        let kind = error
            .project_route_context()
            .and_then(|(reason_code, _, _)| ProjectRouteFailureKind::from_reason_code(reason_code))
            .unwrap_or(ProjectRouteFailureKind::Unavailable);
        Self {
            kind,
            detail: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::errors::TraceDecayError;

    use super::{ProjectRouteFailure, ProjectRouteFailureKind};

    #[test]
    fn a_route_failure_kind_comes_from_its_reason_code_not_its_text() {
        let ambiguous = TraceDecayError::project_route(
            "project_route_ambiguous",
            false,
            "two registered projects claim this workspace",
        );
        assert_eq!(
            ProjectRouteFailure::from_selection_error(&ambiguous).kind,
            ProjectRouteFailureKind::Ambiguous
        );

        // Project open raises this when the Context Scout registrar holds no
        // registry for the project. The text names a registry, but nothing
        // about the route's authorization was decided.
        let mounting = TraceDecayError::Config {
            message: "project-open Context Scout address registry is unavailable".to_owned(),
        };
        let failure = ProjectRouteFailure::from_selection_error(&mounting);
        assert_eq!(failure.kind, ProjectRouteFailureKind::Unavailable);
        assert_eq!(
            failure.detail,
            "config error: project-open Context Scout address registry is unavailable"
        );
    }
}
