//! Single-use, destination-bound handoff opening over daemon-owned authority.
//!
//! These tokens open an already-owned investigation or Work surface. They are
//! deliberately separate from workflow actor-to-actor handoff grants: opening
//! never transfers execution context, renews a lease, mutates Work, or returns
//! an investigation/task body.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::feedback::FeedbackFindingId;
use tracedecay_domain::{ManifestDigest, TaskId, UtcMicros, WorkVersion, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::context::{RequestAdmission, RequestContext, RequestId};
use crate::error::ApplicationContractError;
use crate::feedback::FeedbackFindingReadV1;
use crate::identity::application_identifier;

pub const MAX_HANDOFF_OPEN_LIFETIME_MICROS: i64 = 60_000_000;

pub const HANDOFF_ISSUE_CAPABILITY_ID_V1: &str = "capability.handoff.issue";
pub const HANDOFF_ISSUE_USE_CASE_ID_V1: &str = "use-case.handoff.issue";
pub const HANDOFF_ISSUE_OPERATION_ID_V1: (&str, &str, &str) = (
    "issue",
    HANDOFF_ISSUE_CAPABILITY_ID_V1,
    HANDOFF_ISSUE_USE_CASE_ID_V1,
);
pub const OPEN_INVESTIGATION_HANDOFF_CAPABILITY_ID_V1: &str =
    "capability.handoff.open_investigation_handoff";
pub const OPEN_INVESTIGATION_HANDOFF_USE_CASE_ID_V1: &str =
    "use-case.handoff.open_investigation_handoff";
pub const OPEN_TASK_HANDOFF_CAPABILITY_ID_V1: &str = "capability.handoff.open_task_handoff";
pub const OPEN_TASK_HANDOFF_USE_CASE_ID_V1: &str = "use-case.handoff.open_task_handoff";

application_identifier!(
    HandoffSessionId => ("handoff session id", 512),
);

/// Bearer material is accepted only at the daemon boundary and never
/// serialized into a grant, receipt, diagnostic, or result.
pub struct HandoffOpenToken {
    secret: String,
}

impl HandoffOpenToken {
    pub fn new(secret: String) -> Result<Self, HandoffOpenError> {
        let byte_len = secret.len();
        if !(32..=512).contains(&byte_len)
            || secret.trim() != secret
            || secret.chars().any(char::is_control)
        {
            return Err(HandoffOpenError::InvalidToken);
        }
        Ok(Self { secret })
    }

    fn digest(&self) -> Result<ManifestDigest, HandoffOpenError> {
        canonical_sha256(&("tracedecay.application.handoff-open.v1", &self.secret))
            .map_err(|_| HandoffOpenError::InvalidToken)
    }
}

impl fmt::Debug for HandoffOpenToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HandoffOpenToken([REDACTED])")
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum HandoffOpenKindV1 {
    Investigation,
    Task,
}

/// Current daemon policy and mutable-authority identity bound to issuance.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffAuthoritySnapshotV1 {
    authority_digest: ManifestDigest,
    policy_digest: ManifestDigest,
}

impl HandoffAuthoritySnapshotV1 {
    pub fn new(
        authority_digest: ManifestDigest,
        policy_digest: ManifestDigest,
    ) -> Result<Self, ApplicationContractError> {
        authority_digest.validate()?;
        policy_digest.validate()?;
        Ok(Self {
            authority_digest,
            policy_digest,
        })
    }

    pub fn authority_digest(&self) -> &ManifestDigest {
        &self.authority_digest
    }

    pub fn policy_digest(&self) -> &ManifestDigest {
        &self.policy_digest
    }
}

/// Context fields that must still match when the bearer is consumed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffOpenContextV1 {
    kind: HandoffOpenKindV1,
    session_id: HandoffSessionId,
    scope_digest: ManifestDigest,
    grant_id: crate::context::CapabilityGrantId,
    grant_revision: u64,
    grant_digest: ManifestDigest,
    authority: HandoffAuthoritySnapshotV1,
}

impl HandoffOpenContextV1 {
    fn from_request(
        request: &RequestContext,
        kind: HandoffOpenKindV1,
        session_id: HandoffSessionId,
        authority: HandoffAuthoritySnapshotV1,
    ) -> Result<Self, HandoffOpenError> {
        request
            .validate()
            .map_err(|_| HandoffOpenError::NotFoundOrNotAuthorized)?;
        Ok(Self {
            kind,
            session_id,
            scope_digest: request.scope().scope_digest.clone(),
            grant_id: request.grant().grant_id.clone(),
            grant_revision: request.grant().revision,
            grant_digest: request.grant().digest.clone(),
            authority,
        })
    }

    pub const fn kind(&self) -> HandoffOpenKindV1 {
        self.kind
    }

    pub fn session_id(&self) -> &HandoffSessionId {
        &self.session_id
    }

    pub fn scope_digest(&self) -> &ManifestDigest {
        &self.scope_digest
    }

    pub fn authority(&self) -> &HandoffAuthoritySnapshotV1 {
        &self.authority
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HandoffOpenTargetV1 {
    Investigation {
        finding_id: FeedbackFindingId,
        owner_version_digest: ManifestDigest,
    },
    Task {
        task_id: TaskId,
        version: WorkVersion,
        owner_version_digest: ManifestDigest,
    },
}

impl HandoffOpenTargetV1 {
    pub const fn kind(&self) -> HandoffOpenKindV1 {
        match self {
            Self::Investigation { .. } => HandoffOpenKindV1::Investigation,
            Self::Task { .. } => HandoffOpenKindV1::Task,
        }
    }

    pub fn owner_version_digest(&self) -> &ManifestDigest {
        match self {
            Self::Investigation {
                owner_version_digest,
                ..
            }
            | Self::Task {
                owner_version_digest,
                ..
            } => owner_version_digest,
        }
    }
}

/// Complete secret-free binding persisted by the daemon authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffOpenBindingV1 {
    context: HandoffOpenContextV1,
    target: HandoffOpenTargetV1,
}

impl HandoffOpenBindingV1 {
    pub fn investigation(
        request: &RequestContext,
        session_id: HandoffSessionId,
        finding_id: FeedbackFindingId,
        owner_version_digest: ManifestDigest,
        authority: HandoffAuthoritySnapshotV1,
    ) -> Result<Self, HandoffOpenError> {
        finding_id
            .validate()
            .map_err(|_| HandoffOpenError::InvalidBinding)?;
        owner_version_digest
            .validate()
            .map_err(|_| HandoffOpenError::InvalidBinding)?;
        Ok(Self {
            context: HandoffOpenContextV1::from_request(
                request,
                HandoffOpenKindV1::Investigation,
                session_id,
                authority,
            )?,
            target: HandoffOpenTargetV1::Investigation {
                finding_id,
                owner_version_digest,
            },
        })
    }

    pub fn task(
        request: &RequestContext,
        session_id: HandoffSessionId,
        task_id: TaskId,
        version: WorkVersion,
        authority: HandoffAuthoritySnapshotV1,
    ) -> Result<Self, HandoffOpenError> {
        task_id
            .validate()
            .map_err(|_| HandoffOpenError::InvalidBinding)?;
        let owner_version_digest = canonical_sha256(&(
            "tracedecay.application.handoff-open.task-version.v1",
            &task_id,
            version,
        ))
        .map_err(|_| HandoffOpenError::InvalidBinding)?;
        Ok(Self {
            context: HandoffOpenContextV1::from_request(
                request,
                HandoffOpenKindV1::Task,
                session_id,
                authority,
            )?,
            target: HandoffOpenTargetV1::Task {
                task_id,
                version,
                owner_version_digest,
            },
        })
    }

    pub fn context(&self) -> &HandoffOpenContextV1 {
        &self.context
    }

    pub fn target(&self) -> &HandoffOpenTargetV1 {
        &self.target
    }
}

pub fn investigation_owner_version_digest(
    finding: &FeedbackFindingReadV1,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.application.handoff-open.investigation-version.v1",
        &finding.result_id,
        &finding.cycle_id,
        &finding.scope,
        &finding.finding,
    ))
    .map_err(Into::into)
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffOpenGrantV1 {
    binding: HandoffOpenBindingV1,
    token_digest: ManifestDigest,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
}

impl HandoffOpenGrantV1 {
    pub fn new(
        binding: HandoffOpenBindingV1,
        token_digest: ManifestDigest,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<Self, HandoffOpenError> {
        if issued_at >= expires_at
            || expires_at
                .0
                .checked_sub(issued_at.0)
                .is_none_or(|lifetime| lifetime > MAX_HANDOFF_OPEN_LIFETIME_MICROS)
        {
            return Err(HandoffOpenError::InvalidExpiry);
        }
        token_digest
            .validate()
            .map_err(|_| HandoffOpenError::InvalidToken)?;
        Ok(Self {
            binding,
            token_digest,
            issued_at,
            expires_at,
        })
    }

    pub fn binding(&self) -> &HandoffOpenBindingV1 {
        &self.binding
    }

    pub fn context(&self) -> &HandoffOpenContextV1 {
        self.binding.context()
    }

    pub fn target(&self) -> &HandoffOpenTargetV1 {
        self.binding.target()
    }

    pub fn token_digest(&self) -> &ManifestDigest {
        &self.token_digest
    }

    pub const fn issued_at(&self) -> &UtcMicros {
        &self.issued_at
    }

    pub const fn expires_at(&self) -> &UtcMicros {
        &self.expires_at
    }

    pub fn consume(
        &self,
        request_id: RequestId,
        input_digest: ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<HandoffOpenConsumptionV1, ApplicationContractError> {
        HandoffOpenConsumptionV1::new(
            self.binding.clone(),
            self.token_digest.clone(),
            request_id,
            input_digest,
            consumed_at,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffOpenConsumptionV1 {
    binding: HandoffOpenBindingV1,
    token_digest: ManifestDigest,
    request_id: RequestId,
    input_digest: ManifestDigest,
    consumed_at: UtcMicros,
    receipt_digest: ManifestDigest,
}

impl HandoffOpenConsumptionV1 {
    fn new(
        binding: HandoffOpenBindingV1,
        token_digest: ManifestDigest,
        request_id: RequestId,
        input_digest: ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<Self, ApplicationContractError> {
        let receipt_digest = canonical_sha256(&(
            "tracedecay.application.handoff-open.consumption.v1",
            &binding,
            &token_digest,
            &request_id,
            &input_digest,
            consumed_at,
        ))?;
        Ok(Self {
            binding,
            token_digest,
            request_id,
            input_digest,
            consumed_at,
            receipt_digest,
        })
    }

    pub fn binding(&self) -> &HandoffOpenBindingV1 {
        &self.binding
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    fn receipt(&self) -> HandoffOpenReceiptV1 {
        HandoffOpenReceiptV1 {
            token_digest: self.token_digest.clone(),
            request_id: self.request_id.clone(),
            input_digest: self.input_digest.clone(),
            consumed_at: self.consumed_at,
            receipt_digest: self.receipt_digest.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffOpenConsumeOutcomeV1 {
    Consumed(HandoffOpenConsumptionV1),
    Concealed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffOpenAuthorityError {
    Conflict,
    IdempotencyConflict,
    Unavailable,
}

pub trait HandoffOpenAuthorityPort: Send + Sync {
    fn issue(&self, grant: &HandoffOpenGrantV1) -> Result<(), HandoffOpenAuthorityError>;

    fn resolve(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenContextV1,
        observed_at: UtcMicros,
    ) -> Result<Option<HandoffOpenGrantV1>, HandoffOpenAuthorityError>;

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenContextV1,
        request_id: &RequestId,
        input_digest: &ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<HandoffOpenConsumeOutcomeV1, HandoffOpenAuthorityError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffOpenTargetError {
    Unavailable,
}

pub type HandoffOpenTargetFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, HandoffOpenTargetError>> + Send + 'a>>;

pub trait HandoffOpenTargetPort: Send + Sync {
    fn is_current<'a>(
        &'a self,
        context: &'a RequestContext,
        binding: &'a HandoffOpenBindingV1,
    ) -> HandoffOpenTargetFuture<'a>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffOpenError {
    InvalidToken,
    InvalidBinding,
    InvalidExpiry,
    Cancelled,
    TimedOut,
    NotFoundOrNotAuthorized,
    Conflict,
    AuthorityUnavailable,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenInvestigationHandoffRequestV1 {
    pub token: String,
    pub session_id: HandoffSessionId,
}

impl fmt::Debug for OpenInvestigationHandoffRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenInvestigationHandoffRequestV1")
            .field("token", &"[REDACTED]")
            .field("session_id", &self.session_id)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenTaskHandoffRequestV1 {
    pub token: String,
    pub session_id: HandoffSessionId,
}

impl fmt::Debug for OpenTaskHandoffRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenTaskHandoffRequestV1")
            .field("token", &"[REDACTED]")
            .field("session_id", &self.session_id)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InvestigationHandoffSurfaceV1 {
    pub finding_id: FeedbackFindingId,
    pub owner_version_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffSurfaceV1 {
    pub task_id: TaskId,
    pub version: WorkVersion,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffOpenReceiptV1 {
    pub token_digest: ManifestDigest,
    pub request_id: RequestId,
    pub input_digest: ManifestDigest,
    pub consumed_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenInvestigationHandoffResultV1 {
    pub surface: InvestigationHandoffSurfaceV1,
    pub receipt: HandoffOpenReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpenTaskHandoffResultV1 {
    pub surface: TaskHandoffSurfaceV1,
    pub receipt: HandoffOpenReceiptV1,
}

pub struct HandoffOpenService<A, T> {
    authority: A,
    targets: T,
}

impl<A, T> HandoffOpenService<A, T>
where
    A: HandoffOpenAuthorityPort,
    T: HandoffOpenTargetPort,
{
    pub const fn new(authority: A, targets: T) -> Self {
        Self { authority, targets }
    }

    pub async fn issue(
        &self,
        context: &RequestContext,
        binding: HandoffOpenBindingV1,
        token: &HandoffOpenToken,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<HandoffOpenGrantV1, HandoffOpenError> {
        admit(
            context,
            HANDOFF_ISSUE_CAPABILITY_ID_V1,
            HANDOFF_ISSUE_USE_CASE_ID_V1,
            issued_at,
        )?;
        if binding.context.scope_digest != context.scope().scope_digest
            || binding.context.grant_id != context.grant().grant_id
            || binding.context.grant_revision != context.grant().revision
            || binding.context.grant_digest != context.grant().digest
        {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        }
        if !self
            .targets
            .is_current(context, &binding)
            .await
            .map_err(|_| HandoffOpenError::AuthorityUnavailable)?
        {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        }
        let grant = HandoffOpenGrantV1::new(binding, token.digest()?, issued_at, expires_at)?;
        self.authority.issue(&grant).map_err(authority_error)?;
        Ok(grant)
    }

    pub async fn open_investigation(
        &self,
        context: &RequestContext,
        request: OpenInvestigationHandoffRequestV1,
        authority: HandoffAuthoritySnapshotV1,
        observed_at: UtcMicros,
    ) -> Result<OpenInvestigationHandoffResultV1, HandoffOpenError> {
        let consumption = self
            .open(
                context,
                request.token,
                request.session_id,
                authority,
                HandoffOpenKindV1::Investigation,
                OPEN_INVESTIGATION_HANDOFF_CAPABILITY_ID_V1,
                OPEN_INVESTIGATION_HANDOFF_USE_CASE_ID_V1,
                observed_at,
            )
            .await?;
        let HandoffOpenTargetV1::Investigation {
            finding_id,
            owner_version_digest,
        } = consumption.binding().target().clone()
        else {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        };
        Ok(OpenInvestigationHandoffResultV1 {
            surface: InvestigationHandoffSurfaceV1 {
                finding_id,
                owner_version_digest,
            },
            receipt: consumption.receipt(),
        })
    }

    pub async fn open_task(
        &self,
        context: &RequestContext,
        request: OpenTaskHandoffRequestV1,
        authority: HandoffAuthoritySnapshotV1,
        observed_at: UtcMicros,
    ) -> Result<OpenTaskHandoffResultV1, HandoffOpenError> {
        let consumption = self
            .open(
                context,
                request.token,
                request.session_id,
                authority,
                HandoffOpenKindV1::Task,
                OPEN_TASK_HANDOFF_CAPABILITY_ID_V1,
                OPEN_TASK_HANDOFF_USE_CASE_ID_V1,
                observed_at,
            )
            .await?;
        let HandoffOpenTargetV1::Task {
            task_id, version, ..
        } = consumption.binding().target().clone()
        else {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        };
        Ok(OpenTaskHandoffResultV1 {
            surface: TaskHandoffSurfaceV1 { task_id, version },
            receipt: consumption.receipt(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn open(
        &self,
        context: &RequestContext,
        token: String,
        session_id: HandoffSessionId,
        authority: HandoffAuthoritySnapshotV1,
        kind: HandoffOpenKindV1,
        capability: &str,
        use_case: &str,
        observed_at: UtcMicros,
    ) -> Result<HandoffOpenConsumptionV1, HandoffOpenError> {
        admit(context, capability, use_case, observed_at)?;
        let token = HandoffOpenToken::new(token)?;
        let token_digest = token.digest()?;
        let expected = HandoffOpenContextV1::from_request(context, kind, session_id, authority)?;
        let Some(grant) = self
            .authority
            .resolve(&token_digest, &expected, observed_at)
            .map_err(authority_error)?
        else {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        };
        if grant.target().kind() != kind
            || !self
                .targets
                .is_current(context, grant.binding())
                .await
                .map_err(|_| HandoffOpenError::AuthorityUnavailable)?
        {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        }
        let input_digest = canonical_sha256(&(
            "tracedecay.application.handoff-open.request.v1",
            kind,
            &expected,
            &token_digest,
        ))
        .map_err(|_| HandoffOpenError::InvalidBinding)?;
        let consumption = match self
            .authority
            .consume(
                &token_digest,
                &expected,
                context.request_id(),
                &input_digest,
                observed_at,
            )
            .map_err(authority_error)?
        {
            HandoffOpenConsumeOutcomeV1::Consumed(consumption) => consumption,
            HandoffOpenConsumeOutcomeV1::Concealed => {
                return Err(HandoffOpenError::NotFoundOrNotAuthorized);
            }
        };
        // The single-use commit is authoritative. Rechecking again prevents a
        // version change racing the pre-effect read from opening stale state.
        if !self
            .targets
            .is_current(context, consumption.binding())
            .await
            .map_err(|_| HandoffOpenError::AuthorityUnavailable)?
        {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        }
        Ok(consumption)
    }
}

fn admit(
    context: &RequestContext,
    capability: &str,
    use_case: &str,
    observed_at: UtcMicros,
) -> Result<(), HandoffOpenError> {
    match context.admission_at(observed_at) {
        RequestAdmission::Cancelled => return Err(HandoffOpenError::Cancelled),
        RequestAdmission::TimedOut => return Err(HandoffOpenError::TimedOut),
        RequestAdmission::Admitted => {}
    }
    let capability =
        CapabilityId::new(capability).map_err(|_| HandoffOpenError::AuthorityUnavailable)?;
    let use_case = UseCaseId::new(use_case).map_err(|_| HandoffOpenError::AuthorityUnavailable)?;
    if !context.allows(&capability, &use_case) {
        return Err(HandoffOpenError::NotFoundOrNotAuthorized);
    }
    Ok(())
}

fn authority_error(error: HandoffOpenAuthorityError) -> HandoffOpenError {
    match error {
        HandoffOpenAuthorityError::Conflict => HandoffOpenError::Conflict,
        HandoffOpenAuthorityError::IdempotencyConflict => HandoffOpenError::Conflict,
        HandoffOpenAuthorityError::Unavailable => HandoffOpenError::AuthorityUnavailable,
    }
}
