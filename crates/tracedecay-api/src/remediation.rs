//! Doctor remediation write descriptors, DTOs, and dashboard presentation.
//!
//! The executable retains the admitted remediation authority and translates its
//! owner-specific operation phases into the compact presentation inputs below.
//! This adapter owns the request wire shapes, route paths, and truthful
//! `DashboardEnvelope` construction; it never authorizes or dispatches an
//! effect.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::doctor::DoctorOwningOperationRefV1;
use tracedecay_application::{IdempotencyKey, PreviewId};

use crate::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardScopeV1,
};

/// Route for submitting a remediation preview.
pub const DOCTOR_REMEDIATION_PREVIEW_ROUTE_PATH: &str = "/api/doctor/remediations/preview";
/// Route for applying a confirmed remediation.
pub const DOCTOR_REMEDIATION_APPLY_ROUTE_PATH: &str = "/api/doctor/remediations/apply";
/// Route for reading one remediation operation.
pub const DOCTOR_REMEDIATION_STATUS_ROUTE_PATH: &str = "/api/doctor/remediations/{operation_id}";

/// One Doctor remediation route mounted by the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoctorRemediationRouteV1 {
    pub method: &'static str,
    pub path: &'static str,
    pub operation: &'static str,
}

const DOCTOR_REMEDIATION_ROUTES: [DoctorRemediationRouteV1; 3] = [
    DoctorRemediationRouteV1 {
        method: "POST",
        path: DOCTOR_REMEDIATION_PREVIEW_ROUTE_PATH,
        operation: "doctor_remediation_preview",
    },
    DoctorRemediationRouteV1 {
        method: "POST",
        path: DOCTOR_REMEDIATION_APPLY_ROUTE_PATH,
        operation: "doctor_remediation_apply",
    },
    DoctorRemediationRouteV1 {
        method: "GET",
        path: DOCTOR_REMEDIATION_STATUS_ROUTE_PATH,
        operation: "doctor_remediation_status",
    },
];

/// Every Doctor remediation route, in mount order.
#[must_use]
pub const fn doctor_remediation_routes() -> &'static [DoctorRemediationRouteV1] {
    &DOCTOR_REMEDIATION_ROUTES
}

/// Request DTO for a remediation preview. The executable supplies the
/// owner-specific target type while this crate owns the stable outer shape.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorRemediationPreviewRequestV1<Target> {
    pub operation: DoctorOwningOperationRefV1,
    pub target: Target,
}

/// Request DTO for a confirmed remediation apply. The executable supplies the
/// owner-specific target type while the preview and idempotency contracts stay
/// canonical application values.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorRemediationApplyRequestV1<Target> {
    pub operation: DoctorOwningOperationRefV1,
    pub target: Target,
    pub preview_id: Option<PreviewId>,
    pub idempotency_key: IdempotencyKey,
    pub confirmed: bool,
}

/// Wire payload for one remediation response.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DoctorRemediationPayloadV1<Operation, Error> {
    Operation { operation: Operation },
    Unavailable { reason: Error },
}

/// Owner-derived presentation facts for a successful remediation operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoctorRemediationOperationPresentationV1 {
    pub domain_state: DashboardDomainStateV1,
    pub complete: bool,
}

impl DoctorRemediationOperationPresentationV1 {
    #[must_use]
    pub const fn new(domain_state: DashboardDomainStateV1, complete: bool) -> Self {
        Self {
            domain_state,
            complete,
        }
    }
}

/// Owner-derived presentation facts for a rejected remediation operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoctorRemediationErrorPresentationV1 {
    pub domain_state: DashboardDomainStateV1,
    pub unsupported: bool,
}

impl DoctorRemediationErrorPresentationV1 {
    #[must_use]
    pub const fn new(domain_state: DashboardDomainStateV1, unsupported: bool) -> Self {
        Self {
            domain_state,
            unsupported,
        }
    }
}

/// Map an owner invocation result into the dashboard's established remediation
/// envelope. The root supplies only phase/error classification; this function
/// owns all coverage, freshness, and payload rendering.
#[must_use]
pub fn doctor_remediation_envelope<Operation, Error>(
    scope: DashboardScopeV1,
    result: Result<
        (Operation, DoctorRemediationOperationPresentationV1),
        (Error, DoctorRemediationErrorPresentationV1),
    >,
) -> DashboardEnvelopeV1<DoctorRemediationPayloadV1<Operation, Error>> {
    match result {
        Ok((operation, presentation)) => DashboardEnvelopeV1::new(
            scope,
            presentation.domain_state,
            if presentation.complete {
                DashboardCoverageV1::complete(1, "doctor_remediation_operation")
            } else {
                DashboardCoverageV1::unknown()
            },
            if presentation.complete {
                DashboardFreshnessV1::fresh_now()
            } else {
                DashboardFreshnessV1::unknown()
            },
            DoctorRemediationPayloadV1::Operation { operation },
        ),
        Err((reason, presentation)) => DashboardEnvelopeV1::new(
            scope,
            presentation.domain_state,
            if presentation.unsupported {
                DashboardCoverageV1::unsupported()
            } else {
                DashboardCoverageV1::unknown()
            },
            if presentation.unsupported {
                DashboardFreshnessV1::unsupported()
            } else {
                DashboardFreshnessV1::unknown()
            },
            DoctorRemediationPayloadV1::Unavailable { reason },
        ),
    }
}
