use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    UtcMicros, WorkProductGraphV1, WorkProductProjectionBundleV1, WorkRuntimeProjectionV1,
};

use crate::{OpaqueCursor, RequestAdmission, RequestContext};

use super::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkProductApplicationErrorV1,
    WorkProductBindingV1, WorkProductOwnerAuthorizationErrorV1,
    WorkProductOwnerAuthorizationPortV1, WorkProductPortContextV1, WorkProductSelectionScopeV1,
};

pub const MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WorkGraphReadModeV1 {
    Current,
    AsOf {
        valid_at: UtcMicros,
    },
    Evolution {
        from_valid_at: UtcMicros,
        through_valid_at: UtcMicros,
    },
    Forensic {
        from_observed_at: UtcMicros,
        through_observed_at: UtcMicros,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkGraphReadRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub mode: WorkGraphReadModeV1,
    #[schemars(with = "Option<String>")]
    pub continuation: Option<OpaqueCursor>,
    pub observed_at: UtcMicros,
}

impl WorkGraphReadRequestV1 {
    pub const fn current(selection: WorkProductSelectionScopeV1, observed_at: UtcMicros) -> Self {
        Self {
            selection,
            mode: WorkGraphReadModeV1::Current,
            continuation: None,
            observed_at,
        }
    }

    pub fn as_of(
        selection: WorkProductSelectionScopeV1,
        valid_at: UtcMicros,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        let request = Self {
            selection,
            mode: WorkGraphReadModeV1::AsOf { valid_at },
            continuation: None,
            observed_at,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn evolution(
        selection: WorkProductSelectionScopeV1,
        from_valid_at: UtcMicros,
        through_valid_at: UtcMicros,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        let request = Self {
            selection,
            mode: WorkGraphReadModeV1::Evolution {
                from_valid_at,
                through_valid_at,
            },
            continuation: None,
            observed_at,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn forensic(
        selection: WorkProductSelectionScopeV1,
        from_observed_at: UtcMicros,
        through_observed_at: UtcMicros,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        let request = Self {
            selection,
            mode: WorkGraphReadModeV1::Forensic {
                from_observed_at,
                through_observed_at,
            },
            continuation: None,
            observed_at,
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), WorkProductApplicationErrorV1> {
        self.selection.validate()?;
        let valid = match self.mode {
            WorkGraphReadModeV1::Current => self.continuation.is_none(),
            WorkGraphReadModeV1::AsOf { valid_at } => {
                self.continuation.is_none() && valid_at <= self.observed_at
            }
            WorkGraphReadModeV1::Evolution {
                from_valid_at,
                through_valid_at,
            } => from_valid_at <= through_valid_at && through_valid_at <= self.observed_at,
            WorkGraphReadModeV1::Forensic {
                from_observed_at,
                through_observed_at,
            } => from_observed_at <= through_observed_at && through_observed_at <= self.observed_at,
        };
        if valid {
            Ok(())
        } else {
            Err(WorkProductApplicationErrorV1::InvalidRequest)
        }
    }
}

/// One immutable graph version and every Work projection derived from that
/// same version at the caller's explicit observation time.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkGraphVersionEntryV1 {
    valid_at: UtcMicros,
    observed_at: UtcMicros,
    projected_at: UtcMicros,
    verified_version: VerifiedWorkGraphVersionV1,
    graph: WorkProductGraphV1,
    runtime: WorkRuntimeProjectionV1,
    projections: WorkProductProjectionBundleV1,
}

impl WorkGraphVersionEntryV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        valid_at: UtcMicros,
        observed_at: UtcMicros,
        projected_at: UtcMicros,
        verified_version: VerifiedWorkGraphVersionV1,
        graph: WorkProductGraphV1,
        runtime: WorkRuntimeProjectionV1,
        projections: WorkProductProjectionBundleV1,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        if observed_at < valid_at
            || verified_version.graph_version() != graph.version()
            || runtime.graph_version() != graph.version()
            || runtime.observed_at() != projected_at
            || projections.graph_version() != graph.version()
        {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        graph
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        runtime
            .validate(&graph, projected_at)
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let expected_projections =
            WorkProductProjectionBundleV1::from_graph(&graph, &runtime, projected_at)
                .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        if projections != expected_projections {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        Ok(Self {
            valid_at,
            observed_at,
            projected_at,
            verified_version,
            graph,
            runtime,
            projections,
        })
    }

    pub const fn valid_at(&self) -> UtcMicros {
        self.valid_at
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }

    pub const fn projected_at(&self) -> UtcMicros {
        self.projected_at
    }

    pub const fn verified_version(&self) -> &VerifiedWorkGraphVersionV1 {
        &self.verified_version
    }

    pub const fn graph(&self) -> &WorkProductGraphV1 {
        &self.graph
    }

    pub const fn runtime(&self) -> &WorkRuntimeProjectionV1 {
        &self.runtime
    }

    pub const fn projections(&self) -> &WorkProductProjectionBundleV1 {
        &self.projections
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum WorkGraphTimelineCoverageV1 {
    Complete {
        returned: u32,
    },
    Partial {
        returned: u32,
        #[schemars(with = "String")]
        continuation: OpaqueCursor,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkGraphTimelineV1 {
    entries: Vec<WorkGraphVersionEntryV1>,
    coverage: WorkGraphTimelineCoverageV1,
}

impl WorkGraphTimelineV1 {
    pub fn complete(
        entries: Vec<WorkGraphVersionEntryV1>,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        if entries.len() > MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1
            || entries.windows(2).any(|pair| {
                (
                    pair[0].valid_at(),
                    pair[0].observed_at(),
                    pair[0].verified_version().graph_version(),
                ) >= (
                    pair[1].valid_at(),
                    pair[1].observed_at(),
                    pair[1].verified_version().graph_version(),
                )
            })
        {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        let returned = u32::try_from(entries.len())
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        Ok(Self {
            entries,
            coverage: WorkGraphTimelineCoverageV1::Complete { returned },
        })
    }

    pub fn partial(
        entries: Vec<WorkGraphVersionEntryV1>,
        continuation: OpaqueCursor,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        let mut timeline = Self::complete(entries)?;
        let returned = u32::try_from(timeline.entries.len())
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        timeline.coverage = WorkGraphTimelineCoverageV1::Partial {
            returned,
            continuation,
        };
        Ok(timeline)
    }

    pub fn entries(&self) -> &[WorkGraphVersionEntryV1] {
        &self.entries
    }

    pub const fn coverage(&self) -> &WorkGraphTimelineCoverageV1 {
        &self.coverage
    }

    pub const fn continuation(&self) -> Option<&OpaqueCursor> {
        match &self.coverage {
            WorkGraphTimelineCoverageV1::Complete { .. } => None,
            WorkGraphTimelineCoverageV1::Partial { continuation, .. } => Some(continuation),
        }
    }

    fn validate(&self) -> Result<(), WorkProductApplicationErrorV1> {
        let returned = match &self.coverage {
            WorkGraphTimelineCoverageV1::Complete { returned }
            | WorkGraphTimelineCoverageV1::Partial { returned, .. } => *returned,
        };
        if usize::try_from(returned).ok() != Some(self.entries.len())
            || self.entries.len() > MAX_WORK_GRAPH_TEMPORAL_ENTRIES_V1
            || self.entries.windows(2).any(|pair| {
                (
                    pair[0].valid_at(),
                    pair[0].observed_at(),
                    pair[0].verified_version().graph_version(),
                ) >= (
                    pair[1].valid_at(),
                    pair[1].observed_at(),
                    pair[1].verified_version().graph_version(),
                )
            })
        {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WorkGraphReadV1 {
    Current {
        authorized_scope: AuthorizedWorkProductScopeV1,
        snapshot: WorkGraphVersionEntryV1,
    },
    AsOf {
        authorized_scope: AuthorizedWorkProductScopeV1,
        snapshot: WorkGraphVersionEntryV1,
    },
    Evolution {
        authorized_scope: AuthorizedWorkProductScopeV1,
        timeline: WorkGraphTimelineV1,
    },
    Forensic {
        authorized_scope: AuthorizedWorkProductScopeV1,
        timeline: WorkGraphTimelineV1,
    },
}

impl WorkGraphReadV1 {
    pub const fn authorized_scope(&self) -> &AuthorizedWorkProductScopeV1 {
        match self {
            Self::Current {
                authorized_scope, ..
            }
            | Self::AsOf {
                authorized_scope, ..
            }
            | Self::Evolution {
                authorized_scope, ..
            }
            | Self::Forensic {
                authorized_scope, ..
            } => authorized_scope,
        }
    }

    pub fn entries(&self) -> &[WorkGraphVersionEntryV1] {
        match self {
            Self::Current { snapshot, .. } | Self::AsOf { snapshot, .. } => {
                std::slice::from_ref(snapshot)
            }
            Self::Evolution { timeline, .. } | Self::Forensic { timeline, .. } => {
                timeline.entries()
            }
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkGraphReadPortErrorV1 {
    #[error("Work graph was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work graph selection is stale")]
    Stale,
    #[error("Verified Work graph is unavailable")]
    Unavailable,
    #[error("Work graph read was cancelled")]
    Cancelled,
    #[error("Work graph read timed out")]
    TimedOut,
}

impl From<WorkGraphReadPortErrorV1> for WorkProductApplicationErrorV1 {
    fn from(error: WorkGraphReadPortErrorV1) -> Self {
        match error {
            WorkGraphReadPortErrorV1::NotFoundOrNotAuthorized => Self::NotFoundOrNotAuthorized,
            WorkGraphReadPortErrorV1::Stale => Self::VersionConflict,
            WorkGraphReadPortErrorV1::Unavailable => Self::GraphAuthorityUnavailable,
            WorkGraphReadPortErrorV1::Cancelled => Self::Cancelled,
            WorkGraphReadPortErrorV1::TimedOut => Self::TimedOut,
        }
    }
}

pub trait WorkGraphReadPortV1: Send + Sync {
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, WorkGraphReadPortErrorV1>;
}

impl<P> WorkGraphReadPortV1 for &P
where
    P: WorkGraphReadPortV1 + ?Sized,
{
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, WorkGraphReadPortErrorV1> {
        (**self).read_graph(context, request)
    }
}

pub struct WorkProductReadServiceV1<G, A> {
    graph: G,
    owner_authority: A,
    binding: WorkProductBindingV1,
}

impl<G, A> WorkProductReadServiceV1<G, A>
where
    G: WorkGraphReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
{
    pub const fn new(graph: G, owner_authority: A, binding: WorkProductBindingV1) -> Self {
        Self {
            graph,
            owner_authority,
            binding,
        }
    }

    pub fn read_graph(
        &self,
        context: &RequestContext,
        request: WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, WorkProductApplicationErrorV1> {
        if !context.allows(self.binding.capability_id(), self.binding.use_case_id()) {
            return Err(WorkProductApplicationErrorV1::NotAuthorized);
        }
        match context.admission_at(request.observed_at) {
            RequestAdmission::Admitted => {}
            RequestAdmission::Cancelled => {
                return Err(WorkProductApplicationErrorV1::Cancelled);
            }
            RequestAdmission::TimedOut => return Err(WorkProductApplicationErrorV1::TimedOut),
        }
        request.validate()?;
        let authorized_scope = self
            .owner_authority
            .authorize_scope(context, &request.selection, request.observed_at)
            .map_err(|error| match error {
                WorkProductOwnerAuthorizationErrorV1::NotAuthorized => {
                    WorkProductApplicationErrorV1::NotAuthorized
                }
                WorkProductOwnerAuthorizationErrorV1::Unavailable => {
                    WorkProductApplicationErrorV1::GraphAuthorityUnavailable
                }
            })?;
        if authorized_scope.selection() != &request.selection {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let port_context = WorkProductPortContextV1::from_request(
            context,
            authorized_scope.clone(),
            request.observed_at,
        );
        let result = self.graph.read_graph(&port_context, &request)?;
        validate_result(&request, &authorized_scope, &result)?;
        Ok(result)
    }
}

pub(crate) fn validate_result(
    request: &WorkGraphReadRequestV1,
    authorized_scope: &AuthorizedWorkProductScopeV1,
    result: &WorkGraphReadV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if result.authorized_scope() != authorized_scope {
        return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
    }
    let mode_matches = matches!(
        (&request.mode, result),
        (
            WorkGraphReadModeV1::Current,
            WorkGraphReadV1::Current { .. }
        ) | (
            WorkGraphReadModeV1::AsOf { .. },
            WorkGraphReadV1::AsOf { .. }
        ) | (
            WorkGraphReadModeV1::Evolution { .. },
            WorkGraphReadV1::Evolution { .. }
        ) | (
            WorkGraphReadModeV1::Forensic { .. },
            WorkGraphReadV1::Forensic { .. }
        )
    );
    if !mode_matches {
        return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
    }
    if let WorkGraphReadV1::Evolution { timeline, .. }
    | WorkGraphReadV1::Forensic { timeline, .. } = result
    {
        if timeline.validate().is_err() {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
    }
    for entry in result.entries() {
        if entry.projected_at() != request.observed_at
            || entry.observed_at() < entry.valid_at()
            || entry.observed_at() > request.observed_at
            || entry.verified_version().graph_version() != entry.graph().version()
            || entry.runtime().graph_version() != entry.graph().version()
            || entry.runtime().observed_at() != entry.projected_at()
            || entry
                .runtime()
                .validate(entry.graph(), entry.projected_at())
                .is_err()
            || entry.projections().graph_version() != entry.graph().version()
            || entry.graph().validate().is_err()
            || !WorkProductProjectionBundleV1::from_graph(
                entry.graph(),
                entry.runtime(),
                entry.projected_at(),
            )
            .is_ok_and(|expected| expected == *entry.projections())
        {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let within_selection = match request.mode {
            WorkGraphReadModeV1::Current => true,
            WorkGraphReadModeV1::AsOf { valid_at } => entry.valid_at() <= valid_at,
            WorkGraphReadModeV1::Evolution {
                from_valid_at,
                through_valid_at,
            } => (from_valid_at..=through_valid_at).contains(&entry.valid_at()),
            WorkGraphReadModeV1::Forensic {
                from_observed_at,
                through_observed_at,
            } => (from_observed_at..=through_observed_at).contains(&entry.observed_at()),
        };
        if !within_selection {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
    }
    Ok(())
}
