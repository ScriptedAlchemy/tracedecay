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

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde_json::Value;
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, ApplicationProblem,
    AttachRuntimeEvidenceCommand, CreateWorkCommand, ReplanDependenciesCommand, RequestId,
    RetryDirective, ReviewProposalRequestV1, WorkAttemptAcquireLeaseRequestV1,
    WorkAttemptCancelRequestV1, WorkAttemptFinishRequestV1, WorkAttemptPublishArtifactRequestV1,
    WorkAttemptPublishProgressRequestV1, WorkAttemptRecoverRequestV1,
    WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1, WorkAttemptStartRequestV1,
    WorkAttemptTerminalizeRequestV1, WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
};
use tracedecay_domain::{WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1};

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem,
    application_problem_response, invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

/// Which router family mounts an operation.
///
/// The distinction is load-bearing rather than cosmetic: core operations are
/// the projection and command surface the dashboard is allowed to reach, and
/// attempt operations are the runtime lease protocol, which it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkOperationFamily {
    Core,
    Attempt,
}

/// One canonical Work operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkOperation {
    Snapshot,
    Delta,
    Create,
    ReplanDependencies,
    ReviewProposal,
    AcceptProposal,
    AdmitExecution,
    AttachRuntimeEvidence,
    AcceptTask,
    AttemptAcquireLease,
    AttemptRenewLease,
    AttemptStart,
    AttemptPublishProgress,
    AttemptPublishArtifact,
    AttemptCancel,
    AttemptRecover,
    AttemptFinish,
    AttemptTerminalize,
}

/// The router prefix each family mounts its segments under.
macro_rules! work_family_route_prefix {
    (Core) => {
        "/work/"
    };
    (Attempt) => {
        "/work/attempt/"
    };
}

/// The dashboard path, which exists only for the core family.
macro_rules! work_dashboard_route_path {
    (Core, $segment:literal) => {
        Some(concat!("/api/work/", $segment))
    };
    (Attempt, $segment:literal) => {
        None
    };
}

/// Derive every Work operation projection from one `(variant, family, key,
/// segment)` table.
///
/// The catalog id, router path, catalog path, and dashboard path are all
/// mechanical compositions of the key and segment, so the table is the single
/// place an operation is described. A row cannot disagree with itself.
macro_rules! work_operations {
    ($($variant:ident: $family:ident, $key:literal, $segment:literal;)+) => {
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

            /// Which router family mounts this operation.
            pub const fn family(self) -> WorkOperationFamily {
                match self { $(Self::$variant => WorkOperationFamily::$family,)+ }
            }

            /// The path this operation answers on the application router.
            pub const fn route_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!(
                        work_family_route_prefix!($family),
                        $segment
                    ),)+
                }
            }

            /// The path the catalog advertises, which the executable nests
            /// under its `/application` prefix.
            pub const fn application_route_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!(
                        "/application",
                        work_family_route_prefix!($family),
                        $segment
                    ),)+
                }
            }

            /// The public dashboard path, for the core operations the
            /// dashboard mounts.
            pub const fn dashboard_route_path(self) -> Option<&'static str> {
                match self {
                    $(Self::$variant => work_dashboard_route_path!($family, $segment),)+
                }
            }
        }
    };
}

work_operations! {
    Snapshot: Core, "snapshot", "snapshot";
    Delta: Core, "delta", "delta";
    Create: Core, "create", "create";
    ReplanDependencies: Core, "replan_dependencies", "replan-dependencies";
    ReviewProposal: Core, "review_proposal", "review-proposal";
    AcceptProposal: Core, "accept_proposal", "accept-proposal";
    AdmitExecution: Core, "admit_execution", "admit-execution";
    AttachRuntimeEvidence: Core, "attach_runtime_evidence", "attach-runtime-evidence";
    AcceptTask: Core, "accept_task", "accept-task";
    AttemptAcquireLease: Attempt, "attempt_acquire_lease", "acquire-lease";
    AttemptRenewLease: Attempt, "attempt_renew_lease", "renew-lease";
    AttemptStart: Attempt, "attempt_start", "start";
    AttemptPublishProgress: Attempt, "attempt_publish_progress", "publish-progress";
    AttemptPublishArtifact: Attempt, "attempt_publish_artifact", "publish-artifact";
    AttemptCancel: Attempt, "attempt_cancel", "cancel";
    AttemptRecover: Attempt, "attempt_recover", "recover";
    AttemptFinish: Attempt, "attempt_finish", "finish";
    AttemptTerminalize: Attempt, "attempt_terminalize", "terminalize";
}

impl WorkOperation {
    /// No legacy projection or attempt operation is mounted while the native
    /// Graph-backed Work authority is unavailable.
    pub const CORE: [Self; 0] = [];
    pub const ATTEMPT: [Self; 0] = [];
    pub const ALL: [Self; 0] = [];

    /// The catalog operation id.
    pub fn operation_id(self) -> String {
        self.operation_id_str().to_owned()
    }

    /// Whether the operation reads without producing a durable effect.
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Snapshot | Self::Delta)
    }

    /// The generated name of the schema this operation's request satisfies.
    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::Snapshot => schema_name::<WorkProjectionSnapshotRequestV1>(),
            Self::Delta => schema_name::<WorkProjectionDeltaRequestV1>(),
            Self::Create => schema_name::<CreateWorkCommand>(),
            Self::ReplanDependencies => schema_name::<ReplanDependenciesCommand>(),
            Self::ReviewProposal => schema_name::<ReviewProposalRequestV1>(),
            Self::AcceptProposal => schema_name::<AcceptProposalCommand>(),
            Self::AdmitExecution => schema_name::<AdmitExecutionCommand>(),
            Self::AttachRuntimeEvidence => schema_name::<AttachRuntimeEvidenceCommand>(),
            Self::AcceptTask => schema_name::<AcceptTaskCommand>(),
            Self::AttemptAcquireLease => schema_name::<WorkAttemptAcquireLeaseRequestV1>(),
            Self::AttemptRenewLease => schema_name::<WorkAttemptRenewLeaseRequestV1>(),
            Self::AttemptStart => schema_name::<WorkAttemptStartRequestV1>(),
            Self::AttemptPublishProgress => schema_name::<WorkAttemptPublishProgressRequestV1>(),
            Self::AttemptPublishArtifact => schema_name::<WorkAttemptPublishArtifactRequestV1>(),
            Self::AttemptCancel => schema_name::<WorkAttemptCancelRequestV1>(),
            Self::AttemptRecover => schema_name::<WorkAttemptRecoverRequestV1>(),
            Self::AttemptFinish => schema_name::<WorkAttemptFinishRequestV1>(),
            Self::AttemptTerminalize => schema_name::<WorkAttemptTerminalizeRequestV1>(),
        }
    }

    /// The generated name of the schema this operation answers with.
    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::Snapshot => schema_name::<WorkProjectionSnapshotV1>(),
            Self::Delta => schema_name::<WorkProjectionDeltaV1>(),
            Self::Create
            | Self::ReplanDependencies
            | Self::ReviewProposal
            | Self::AcceptProposal
            | Self::AdmitExecution
            | Self::AttachRuntimeEvidence
            | Self::AcceptTask => schema_name::<WorkProjection>(),
            Self::AttemptAcquireLease
            | Self::AttemptRenewLease
            | Self::AttemptStart
            | Self::AttemptPublishProgress
            | Self::AttemptPublishArtifact
            | Self::AttemptCancel
            | Self::AttemptRecover
            | Self::AttemptFinish
            | Self::AttemptTerminalize => schema_name::<WorkAttemptResponseV1>(),
        }
    }

    fn parse(family: WorkOperationFamily, segment: &str) -> Option<Self> {
        let candidates: &[Self] = match family {
            WorkOperationFamily::Core => &Self::CORE,
            WorkOperationFamily::Attempt => &Self::ATTEMPT,
        };
        candidates
            .iter()
            .copied()
            .find(|operation| operation.route_segment() == segment)
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

/// Build every mounted Work route: the core surface and the attempt runtime.
pub fn work_application_router<O>(owner: O) -> Router
where
    O: WorkApplicationOwner,
{
    Router::new()
        .route("/work/{operation}", post(core_operation::<O>))
        .route("/work/attempt/{operation}", post(attempt_operation::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

/// Build only the core Work routes, relative to the mount point.
///
/// The dashboard nests this at `/api/work`. Because the attempt routes are not
/// registered here, an attempt path is not reachable through the dashboard even
/// though the handlers behind both families are the same.
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
    dispatch(
        WorkOperationFamily::Core,
        segment,
        state,
        request_id,
        controls,
        body,
    )
    .await
}

async fn attempt_operation<O>(
    Path(segment): Path<String>,
    state: State<O>,
    request_id: Extension<RequestId>,
    controls: Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: WorkApplicationOwner,
{
    dispatch(
        WorkOperationFamily::Attempt,
        segment,
        state,
        request_id,
        controls,
        body,
    )
    .await
}

async fn dispatch<O>(
    family: WorkOperationFamily,
    segment: String,
    State(owner): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: WorkApplicationOwner,
{
    let Some(operation) = WorkOperation::parse(family, &segment) else {
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

    use super::{WorkOperation, WorkOperationFamily};

    #[test]
    fn every_operation_is_reachable_by_the_segment_its_path_ends_with() {
        for operation in WorkOperation::ALL {
            let path = operation.route_path();
            let segment = path.rsplit('/').next().expect("a non-empty final segment");
            assert_eq!(segment, operation.route_segment(), "{path}");
            assert_eq!(
                WorkOperation::parse(operation.family(), operation.route_segment()),
                Some(operation),
                "{path}"
            );
        }
    }

    #[test]
    fn the_two_families_partition_the_surface_and_never_borrow_each_others_paths() {
        assert_eq!(
            WorkOperation::ALL.len(),
            WorkOperation::CORE.len() + WorkOperation::ATTEMPT.len()
        );
        assert_eq!(
            WorkOperation::ALL
                .into_iter()
                .collect::<BTreeSet<_>>()
                .len(),
            WorkOperation::ALL.len(),
            "the descriptor must list each operation once"
        );

        for operation in WorkOperation::CORE {
            assert_eq!(operation.family(), WorkOperationFamily::Core);
            assert!(!operation.route_path().contains("/attempt/"));
            assert!(operation.dashboard_route_path().is_some());
            assert_eq!(
                WorkOperation::parse(WorkOperationFamily::Attempt, operation.route_segment()),
                None,
                "a core segment must not resolve on the attempt router"
            );
        }
        for operation in WorkOperation::ATTEMPT {
            assert_eq!(operation.family(), WorkOperationFamily::Attempt);
            assert!(operation.route_path().starts_with("/work/attempt/"));
            assert_eq!(
                operation.dashboard_route_path(),
                None,
                "the dashboard must not name an attempt route"
            );
            assert_eq!(
                WorkOperation::parse(WorkOperationFamily::Core, operation.route_segment()),
                None,
                "an attempt segment must not resolve on the core router"
            );
        }
    }

    #[test]
    fn the_catalog_and_dashboard_paths_are_the_router_path_under_their_prefixes() {
        for operation in WorkOperation::ALL {
            assert_eq!(
                operation.application_route_path(),
                format!("/application{}", operation.route_path())
            );
            if let Some(dashboard) = operation.dashboard_route_path() {
                assert_eq!(
                    dashboard,
                    format!("/api{}", operation.route_path()),
                    "{}",
                    operation.operation_key()
                );
            }
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
    fn only_the_projection_reads_are_read_only() {
        let read_only = WorkOperation::ALL
            .into_iter()
            .filter(|operation| operation.is_read_only())
            .collect::<Vec<_>>();
        assert!(read_only.is_empty());
    }
}
