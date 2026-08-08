//! The canonical Work HTTP surface.
//!
//! Every Work adapter — the daemon's application router, the dashboard's public
//! `/api/work` mount, the catalog registry, and the generated SDKs — is derived
//! from the single [`WorkOperation`] descriptor in this module. Adding an
//! operation is one enum variant plus one row in the `work_operations!` table,
//! which derives every key, id, segment, and path; there is no second route
//! table to keep in step, and no adapter that can drift from the catalog
//! without failing to compile.
//!
//! The owner supplies dispatch. This module owns only what HTTP owns: which
//! paths exist, which segment names them, whether the body was well-formed, and
//! that an unrecognised operation is refused the same way an unauthorised one
//! is.

use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde_json::Value;
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AdmitWorkPlacementCommand,
    AdmitWorkSynthesisCommand, ApplicationProblem, AttachRuntimeEvidenceCommand,
    CancelWorkAttemptCommand, CreateWorkCommand, ExecutionTopologyViewV1, GenerateProposalRequest,
    GeneratedWorkProposal, PauseWorkRunCommand, ReleaseWorkPlacementCommand,
    ReplanDependenciesCommand, RequestId, ResumeWorkAttemptsCommand, ResumeWorkRunCommand,
    RetryDirective, ReviewProposalRequestV1, StartWorkAttemptCommand,
    WorkArtifactHydrationRequestV1, WorkArtifactHydrationV1, WorkAttemptListRequestV1,
    WorkAttemptListV1, WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkPlacementPreflightRequestV1,
    WorkPlacementReadingV1, WorkPlacementStatusRequestV1, WorkProjectionDeltaRequestV1,
    WorkProjectionSnapshotRequestV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
    WorkSynthesisAttemptV1, WorkTopologyViewRequestV1,
};
use tracedecay_domain::{
    WorkAttemptV1, WorkPlacementPreflightV1, WorkPlacementV1, WorkProjection,
    WorkProjectionDeltaV1, WorkProjectionSnapshotV1, WorkRunControlV1,
};

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem,
    application_problem_response, invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

/// One canonical Work operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkOperation {
    Snapshot,
    Delta,
    GenerateProposal,
    Create,
    ReplanDependencies,
    ReviewProposal,
    AcceptProposal,
    AdmitExecution,
    AttachRuntimeEvidence,
    AcceptTask,
    StartAttempt,
    Synthesize,
    AttemptStatus,
    CancelAttempt,
    ResumeAttempts,
    ListAttempts,
    HydrateArtifacts,
    Views,
    Topology,
    PauseRun,
    ResumeRun,
    RunControl,
    PlacementPreflight,
    AdmitPlacement,
    PlacementStatus,
    ReleasePlacement,
}

/// Derive every Work operation projection from one `(variant, key, segment)`
/// table.
///
/// The catalog id, router path, catalog path, and dashboard path are all
/// mechanical compositions of the key and segment, so the table is the single
/// place an operation is described. A row cannot disagree with itself.
macro_rules! work_operations {
    ($($variant:ident: $key:literal, $segment:literal;)+) => {
        impl WorkOperation {
            /// The catalog operation key, as it appears in `operation.work.{key}`.
            pub const fn operation_key(self) -> &'static str {
                match self { $(Self::$variant => $key,)+ }
            }

            /// The catalog operation id, as a literal the route documents can hold.
            pub const fn operation_id_str(self) -> &'static str {
                match self { $(Self::$variant => concat!("operation.work.", $key),)+ }
            }

            /// The final path segment that names this operation on its router.
            pub const fn route_segment(self) -> &'static str {
                match self { $(Self::$variant => $segment,)+ }
            }

            /// The path this operation answers on the application router.
            pub const fn route_path(self) -> &'static str {
                match self { $(Self::$variant => concat!("/work/", $segment),)+ }
            }

            /// The path the catalog advertises, which the executable nests
            /// under its `/application` prefix.
            pub const fn application_route_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("/application/work/", $segment),)+
                }
            }

            /// The public dashboard path where the dashboard mounts this
            /// operation.
            pub const fn dashboard_route_path(self) -> &'static str {
                match self { $(Self::$variant => concat!("/api/work/", $segment),)+ }
            }
        }
    };
}

work_operations! {
    Snapshot: "snapshot", "snapshot";
    Delta: "delta", "delta";
    GenerateProposal: "generate_proposal", "generate-proposal";
    Create: "create", "create";
    ReplanDependencies: "replan_dependencies", "replan-dependencies";
    ReviewProposal: "review_proposal", "review-proposal";
    AcceptProposal: "accept_proposal", "accept-proposal";
    AdmitExecution: "admit_execution", "admit-execution";
    AttachRuntimeEvidence: "attach_runtime_evidence", "attach-runtime-evidence";
    AcceptTask: "accept_task", "accept-task";
    StartAttempt: "start_attempt", "start-attempt";
    Synthesize: "synthesize", "synthesize";
    AttemptStatus: "attempt_status", "attempt-status";
    CancelAttempt: "cancel_attempt", "cancel-attempt";
    ResumeAttempts: "resume_attempts", "resume-attempts";
    ListAttempts: "list_attempts", "list-attempts";
    HydrateArtifacts: "hydrate_artifacts", "hydrate-artifacts";
    Views: "views", "views";
    Topology: "topology", "topology";
    PauseRun: "pause_run", "pause-run";
    ResumeRun: "resume_run", "resume-run";
    RunControl: "run_control", "run-control";
    PlacementPreflight: "placement_preflight", "placement-preflight";
    AdmitPlacement: "admit_placement", "admit-placement";
    PlacementStatus: "placement_status", "placement-status";
    ReleasePlacement: "release_placement", "release-placement";
}

impl WorkOperation {
    /// Every mounted Work operation, in mounted order.
    pub const ALL: [Self; 26] = [
        Self::Snapshot,
        Self::Delta,
        Self::GenerateProposal,
        Self::Create,
        Self::ReplanDependencies,
        Self::ReviewProposal,
        Self::AcceptProposal,
        Self::AdmitExecution,
        Self::AttachRuntimeEvidence,
        Self::AcceptTask,
        Self::StartAttempt,
        Self::Synthesize,
        Self::AttemptStatus,
        Self::CancelAttempt,
        Self::ResumeAttempts,
        Self::ListAttempts,
        Self::HydrateArtifacts,
        Self::Views,
        Self::Topology,
        Self::PauseRun,
        Self::ResumeRun,
        Self::RunControl,
        Self::PlacementPreflight,
        Self::AdmitPlacement,
        Self::PlacementStatus,
        Self::ReleasePlacement,
    ];

    /// The catalog operation id.
    pub fn operation_id(self) -> String {
        self.operation_id_str().to_owned()
    }

    /// Whether the operation reads without producing a durable effect.
    pub const fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::Snapshot
                | Self::Delta
                | Self::GenerateProposal
                | Self::AttemptStatus
                | Self::ListAttempts
                | Self::HydrateArtifacts
                | Self::Views
                | Self::Topology
                | Self::RunControl
                | Self::PlacementPreflight
                | Self::PlacementStatus
        )
    }

    /// The generated name of the schema this operation's request satisfies.
    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::Snapshot => schema_name::<WorkProjectionSnapshotRequestV1>(),
            Self::Delta => schema_name::<WorkProjectionDeltaRequestV1>(),
            Self::GenerateProposal => schema_name::<GenerateProposalRequest>(),
            Self::Create => schema_name::<CreateWorkCommand>(),
            Self::ReplanDependencies => schema_name::<ReplanDependenciesCommand>(),
            Self::ReviewProposal => schema_name::<ReviewProposalRequestV1>(),
            Self::AcceptProposal => schema_name::<AcceptProposalCommand>(),
            Self::AdmitExecution => schema_name::<AdmitExecutionCommand>(),
            Self::AttachRuntimeEvidence => schema_name::<AttachRuntimeEvidenceCommand>(),
            Self::AcceptTask => schema_name::<AcceptTaskCommand>(),
            Self::StartAttempt => schema_name::<StartWorkAttemptCommand>(),
            Self::Synthesize => schema_name::<AdmitWorkSynthesisCommand>(),
            Self::AttemptStatus => schema_name::<WorkAttemptStatusRequestV1>(),
            Self::CancelAttempt => schema_name::<CancelWorkAttemptCommand>(),
            Self::ResumeAttempts => schema_name::<ResumeWorkAttemptsCommand>(),
            Self::ListAttempts => schema_name::<WorkAttemptListRequestV1>(),
            Self::HydrateArtifacts => schema_name::<WorkArtifactHydrationRequestV1>(),
            Self::Views => schema_name::<WorkGraphReadRequestV1>(),
            Self::Topology => schema_name::<WorkTopologyViewRequestV1>(),
            Self::PauseRun => schema_name::<PauseWorkRunCommand>(),
            Self::ResumeRun => schema_name::<ResumeWorkRunCommand>(),
            Self::RunControl => schema_name::<WorkRunControlRequestV1>(),
            Self::PlacementPreflight => schema_name::<WorkPlacementPreflightRequestV1>(),
            Self::AdmitPlacement => schema_name::<AdmitWorkPlacementCommand>(),
            Self::PlacementStatus => schema_name::<WorkPlacementStatusRequestV1>(),
            Self::ReleasePlacement => schema_name::<ReleaseWorkPlacementCommand>(),
        }
    }

    /// The generated name of the schema this operation answers with.
    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::Snapshot => schema_name::<WorkProjectionSnapshotV1>(),
            Self::Delta => schema_name::<WorkProjectionDeltaV1>(),
            Self::GenerateProposal => schema_name::<GeneratedWorkProposal>(),
            Self::Create
            | Self::ReplanDependencies
            | Self::ReviewProposal
            | Self::AcceptProposal
            | Self::AdmitExecution
            | Self::AttachRuntimeEvidence
            | Self::AcceptTask => schema_name::<WorkProjection>(),
            Self::StartAttempt | Self::AttemptStatus | Self::CancelAttempt => {
                schema_name::<WorkAttemptV1>()
            }
            Self::Synthesize => schema_name::<WorkSynthesisAttemptV1>(),
            Self::ResumeAttempts => schema_name::<WorkAttemptRecoveryReportV1>(),
            Self::ListAttempts => schema_name::<WorkAttemptListV1>(),
            Self::HydrateArtifacts => schema_name::<WorkArtifactHydrationV1>(),
            Self::Views => schema_name::<WorkGraphReadV1>(),
            Self::Topology => schema_name::<ExecutionTopologyViewV1>(),
            Self::PauseRun | Self::ResumeRun => schema_name::<WorkRunControlV1>(),
            Self::RunControl => schema_name::<WorkRunControlReadingV1>(),
            Self::PlacementPreflight => schema_name::<WorkPlacementPreflightV1>(),
            Self::AdmitPlacement | Self::ReleasePlacement => schema_name::<WorkPlacementV1>(),
            Self::PlacementStatus => schema_name::<WorkPlacementReadingV1>(),
        }
    }

    /// Resolve an operation from the final path segment that names it.
    ///
    /// The route segment is the one public name a Work operation has, so every
    /// adapter that accepts an operation by name — the router, the CLI, the
    /// catalog — resolves it here rather than keeping a second name table.
    pub fn from_route_segment(segment: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|operation| operation.route_segment() == segment)
    }

    fn parse(segment: &str) -> Option<Self> {
        Self::from_route_segment(segment)
    }
}

impl FromStr for WorkOperation {
    type Err = String;

    fn from_str(segment: &str) -> Result<Self, Self::Err> {
        Self::from_route_segment(segment).ok_or_else(|| {
            format!(
                "unknown Work operation route segment: {segment} (valid operations: {})",
                Self::ALL
                    .iter()
                    .map(|operation| operation.route_segment())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

/// One Work request, resolved to its canonical operation and ready to dispatch.
#[derive(Clone, Debug)]
pub struct WorkHttpRequest {
    pub operation: WorkOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type WorkInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

/// The application owner behind every Work route.
///
/// The owner decodes the body against the operation's request contract and
/// encodes its own result, because only the executable knows the outcome types.
/// This crate hands it a resolved operation and a well-formed JSON body.
pub trait WorkApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_work(&self, request: WorkHttpRequest) -> WorkInvocationFuture;
}

impl<F, Fut> WorkApplicationOwner for F
where
    F: Fn(WorkHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_work(&self, request: WorkHttpRequest) -> WorkInvocationFuture {
        Box::pin((self)(request))
    }
}

/// Build every mounted Work route.
pub fn work_application_router<O>(owner: O) -> Router
where
    O: WorkApplicationOwner,
{
    Router::new()
        .route("/work/{operation}", post(core_operation::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

/// Build the same Work routes relative to the mount point.
///
/// The dashboard nests this at `/api/work`.
pub fn work_core_router<O>(owner: O) -> Router
where
    O: WorkApplicationOwner,
{
    Router::new()
        .route("/{operation}", post(core_operation::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

async fn core_operation<O>(
    Path(segment): Path<String>,
    state: State<O>,
    request_id: Extension<RequestId>,
    controls: Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: WorkApplicationOwner,
{
    dispatch(segment, state, request_id, controls, body).await
}

async fn dispatch<O>(
    segment: String,
    State(owner): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: WorkApplicationOwner,
{
    let Some(operation) = WorkOperation::parse(&segment) else {
        // An operation this build does not mount is concealed the same way an
        // unauthorised one is, so probing a path cannot reveal what exists.
        return application_problem_response(adapter_problem(
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));
    };
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "work.invalid_body",
            "The Work request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_work(WorkHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

/// Refuse a body that does not satisfy the operation's request contract.
///
/// The owner decodes against the typed contract, so this is the refusal it
/// returns when that decode fails: the same canonical problem envelope every
/// other malformed application request produces.
pub fn work_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "work.invalid_request",
        "The Work application request is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::WorkOperation;

    #[test]
    fn every_operation_is_reachable_by_the_segment_its_path_ends_with() {
        for operation in WorkOperation::ALL {
            let path = operation.route_path();
            let segment = path.rsplit('/').next().expect("a non-empty final segment");
            assert_eq!(segment, operation.route_segment(), "{path}");
            assert_eq!(
                WorkOperation::parse(operation.route_segment()),
                Some(operation),
                "{path}"
            );
        }
    }

    #[test]
    fn the_descriptor_lists_each_operation_once() {
        assert_eq!(
            WorkOperation::ALL
                .into_iter()
                .collect::<BTreeSet<_>>()
                .len(),
            WorkOperation::ALL.len(),
        );
    }

    #[test]
    fn the_catalog_and_dashboard_paths_are_the_router_path_under_their_prefixes() {
        for operation in WorkOperation::ALL {
            assert_eq!(
                operation.application_route_path(),
                format!("/application{}", operation.route_path())
            );
            assert_eq!(
                operation.dashboard_route_path(),
                format!("/api{}", operation.route_path()),
                "{}",
                operation.operation_key()
            );
        }
    }

    #[test]
    fn the_operation_id_literal_is_the_key_under_the_canonical_prefix() {
        for operation in WorkOperation::ALL {
            assert_eq!(
                operation.operation_id_str(),
                format!("operation.work.{}", operation.operation_key())
            );
            assert_eq!(operation.operation_id(), operation.operation_id_str());
        }
    }

    #[test]
    fn only_the_projection_reads_and_proposal_generation_are_read_only() {
        let read_only = WorkOperation::ALL
            .into_iter()
            .filter(|operation| operation.is_read_only())
            .collect::<Vec<_>>();
        assert_eq!(
            read_only,
            vec![
                WorkOperation::Snapshot,
                WorkOperation::Delta,
                WorkOperation::GenerateProposal,
                WorkOperation::AttemptStatus,
                WorkOperation::ListAttempts,
                WorkOperation::HydrateArtifacts,
                WorkOperation::Views,
                WorkOperation::RunControl,
                WorkOperation::PlacementPreflight,
                WorkOperation::PlacementStatus,
            ]
        );
    }
}
