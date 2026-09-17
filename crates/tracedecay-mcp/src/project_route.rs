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

    pub fn from_selection_error(error: &TraceDecayError) -> Self {
        let detail = error.to_string();
        let kind = match error {
            TraceDecayError::ProjectRoute { reason_code, .. } => match reason_code.as_str() {
                "project_route_not_found" => ProjectRouteFailureKind::NotFound,
                "project_route_not_authorized" => ProjectRouteFailureKind::NotAuthorized,
                "project_route_ambiguous" => ProjectRouteFailureKind::Ambiguous,
                _ => ProjectRouteFailureKind::Unavailable,
            },
            TraceDecayError::Config { message } if message.contains("not found for selector") => {
                ProjectRouteFailureKind::NotFound
            }
            TraceDecayError::Config { message }
                if message.contains("ambiguous") || message.contains("multiple stores") =>
            {
                ProjectRouteFailureKind::Ambiguous
            }
            TraceDecayError::Config { message }
                if message.contains("registry is unavailable")
                    || message.contains("profile identity") =>
            {
                ProjectRouteFailureKind::NotAuthorized
            }
            _ => ProjectRouteFailureKind::Unavailable,
        };
        Self { kind, detail }
    }
}
