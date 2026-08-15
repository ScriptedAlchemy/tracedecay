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
use tracedecay_domain::{
    ActorId, ManifestDigest, TaskId, UtcMicros, WorkVersion, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::context::{RequestAdmission, RequestContext, RequestId, ResolvedScope};
use crate::error::ApplicationContractError;
use crate::feedback::FeedbackFindingReadV1;
use crate::identity::application_identifier;

pub const MAX_HANDOFF_OPEN_LIFETIME_MICROS: i64 = 60_000_000;

pub const HANDOFF_ISSUE_CAPABILITY_ID_V1: &str = "capability.handoff.issue_task_handoff";
pub const HANDOFF_ISSUE_USE_CASE_ID_V1: &str = "use-case.handoff.issue_task_handoff";
pub const OPEN_INVESTIGATION_HANDOFF_CAPABILITY_ID_V1: &str =
    "capability.handoff.open_investigation_handoff";
pub const OPEN_INVESTIGATION_HANDOFF_USE_CASE_ID_V1: &str =
    "use-case.handoff.open_investigation_handoff";
pub const OPEN_TASK_HANDOFF_CAPABILITY_ID_V1: &str = "capability.handoff.open_task_handoff";
pub const OPEN_TASK_HANDOFF_USE_CASE_ID_V1: &str = "use-case.handoff.open_task_handoff";
pub const LIST_TASK_HANDOFFS_CAPABILITY_ID_V1: &str = "capability.handoff.list_task_handoffs";
pub const LIST_TASK_HANDOFFS_USE_CASE_ID_V1: &str = "use-case.handoff.list_task_handoffs";

/// Ceiling on grants returned by one enumeration.
///
/// A frontier is read, not paged, and an unbounded answer would be neither
/// readable nor affordable. Reaching the ceiling is reported on the result
/// rather than hidden, so a truncated frontier can never be mistaken for a
/// complete one.
pub const MAX_HANDOFF_LIST_RESULTS_V1: u32 = 200;

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

    pub fn digest(&self) -> Result<ManifestDigest, HandoffOpenError> {
        canonical_sha256(&("tracedecay.application.handoff-open.v1", &self.secret))
            .map_err(|_| HandoffOpenError::InvalidToken)
    }
}

pub fn issue_task_handoff_input_digest(
    request: &IssueTaskHandoffRequestV1,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.handoff.application-input.v1",
        "issue_task_handoff",
        request,
    ))
    .map_err(Into::into)
}

pub fn open_task_handoff_input_digest(
    request: &OpenTaskHandoffRequestV1,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.handoff.application-input.v1",
        "open_task_handoff",
        request,
    ))
    .map_err(Into::into)
}

pub fn list_task_handoffs_input_digest(
    request: &ListTaskHandoffsRequestV1,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.handoff.application-input.v1",
        "list_task_handoffs",
        request,
    ))
    .map_err(Into::into)
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
    issuer_actor_id: ActorId,
    recipient_actor_id: ActorId,
    grant_id: crate::context::CapabilityGrantId,
    grant_revision: u64,
    grant_digest: ManifestDigest,
    authority: HandoffAuthoritySnapshotV1,
}

impl HandoffOpenContextV1 {
    pub fn from_request(
        request: &RequestContext,
        kind: HandoffOpenKindV1,
        session_id: HandoffSessionId,
        recipient_actor_id: ActorId,
        authority: HandoffAuthoritySnapshotV1,
    ) -> Result<Self, HandoffOpenError> {
        request
            .validate()
            .map_err(|_| HandoffOpenError::NotFoundOrNotAuthorized)?;
        Ok(Self {
            kind,
            session_id,
            scope_digest: request.scope().scope_digest.clone(),
            issuer_actor_id: request.actor().clone(),
            recipient_actor_id,
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

    pub fn issuer_actor_id(&self) -> &ActorId {
        &self.issuer_actor_id
    }

    pub fn recipient_actor_id(&self) -> &ActorId {
        &self.recipient_actor_id
    }

    pub fn authority(&self) -> &HandoffAuthoritySnapshotV1 {
        &self.authority
    }
}

/// The recipient-owned fields that may be known before resolving an opaque
/// token. Issuer grant identity stays concealed inside the persisted binding;
/// an independently authenticated recipient is never required to reproduce
/// the issuer's grant.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffOpenExpectationV1 {
    kind: HandoffOpenKindV1,
    session_id: HandoffSessionId,
    scope_digest: ManifestDigest,
    recipient_actor_id: ActorId,
}

impl HandoffOpenExpectationV1 {
    pub fn from_request(
        request: &RequestContext,
        kind: HandoffOpenKindV1,
        session_id: HandoffSessionId,
    ) -> Result<Self, HandoffOpenError> {
        request
            .validate()
            .map_err(|_| HandoffOpenError::NotFoundOrNotAuthorized)?;
        Ok(Self {
            kind,
            session_id,
            scope_digest: request.scope().scope_digest.clone(),
            recipient_actor_id: request.actor().clone(),
        })
    }

    pub fn matches(&self, context: &HandoffOpenContextV1) -> bool {
        self.kind == context.kind
            && self.session_id == context.session_id
            && self.scope_digest == context.scope_digest
            && self.recipient_actor_id == context.recipient_actor_id
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
                request.actor().clone(),
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
        recipient_actor_id: ActorId,
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
                recipient_actor_id,
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
    issued_request_id: RequestId,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
}

impl HandoffOpenGrantV1 {
    pub fn new(
        binding: HandoffOpenBindingV1,
        token_digest: ManifestDigest,
        issued_request_id: RequestId,
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
            issued_request_id,
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

    pub fn issued_request_id(&self) -> &RequestId {
        &self.issued_request_id
    }

    pub fn same_issue_identity(&self, other: &Self) -> bool {
        self.binding == other.binding
            && self.token_digest == other.token_digest
            && self.issued_request_id == other.issued_request_id
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
    binding_digest: ManifestDigest,
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
        let binding_digest =
            canonical_sha256(&("tracedecay.application.handoff-open.binding.v1", &binding))?;
        let receipt_digest = handoff_open_receipt_digest(
            &binding_digest,
            &token_digest,
            &request_id,
            &input_digest,
            consumed_at,
        )?;
        Ok(Self {
            binding,
            binding_digest,
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

    /// When the single use was spent. Read by enumeration to tell a redeemed
    /// token apart from one that merely lapsed.
    pub const fn consumed_at(&self) -> &UtcMicros {
        &self.consumed_at
    }

    fn receipt(&self) -> HandoffOpenReceiptV1 {
        HandoffOpenReceiptV1 {
            binding_digest: self.binding_digest.clone(),
            token_digest: self.token_digest.clone(),
            request_id: self.request_id.clone(),
            input_digest: self.input_digest.clone(),
            consumed_at: self.consumed_at,
            receipt_digest: self.receipt_digest.clone(),
        }
    }
}

pub fn handoff_open_consumption_input_digest(
    kind: HandoffOpenKindV1,
    session_id: &HandoffSessionId,
    scope: &ResolvedScope,
    recipient_actor_id: &ActorId,
    token_digest: &ManifestDigest,
) -> Result<ManifestDigest, ApplicationContractError> {
    let expectation = HandoffOpenExpectationV1 {
        kind,
        session_id: session_id.clone(),
        scope_digest: scope.scope_digest.clone(),
        recipient_actor_id: recipient_actor_id.clone(),
    };
    canonical_sha256(&(
        "tracedecay.application.handoff-open.request.v1",
        kind,
        &expectation,
        token_digest,
    ))
    .map_err(Into::into)
}

pub fn handoff_open_receipt_digest(
    binding_digest: &ManifestDigest,
    token_digest: &ManifestDigest,
    request_id: &RequestId,
    input_digest: &ManifestDigest,
    consumed_at: UtcMicros,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.application.handoff-open.consumption-receipt.v1",
        binding_digest,
        token_digest,
        request_id,
        input_digest,
        consumed_at,
    ))
    .map_err(Into::into)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffOpenConsumeOutcomeV1 {
    Consumed(Box<HandoffOpenConsumptionV1>),
    Concealed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffOpenAuthorityError {
    Conflict,
    IdempotencyConflict,
    Unavailable,
}

/// The recipient-owned scoping for an enumeration.
///
/// Deliberately the same three fields [`HandoffOpenExpectationV1`] matches on,
/// minus the kind: a caller may enumerate EXACTLY the grants it could redeem,
/// across both kinds. That equivalence is the whole safety argument for the
/// operation — listing hands out no authority the caller did not already hold,
/// because every row returned is a token it could already have consumed had it
/// held the bearer. It is not an issuer view: an issuer that could enumerate by
/// its own identity would be reading tokens addressed to other principals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffOpenListFilterV1 {
    session_id: HandoffSessionId,
    scope_digest: ManifestDigest,
    recipient_actor_id: ActorId,
}

impl HandoffOpenListFilterV1 {
    pub fn from_request(
        request: &RequestContext,
        session_id: HandoffSessionId,
    ) -> Result<Self, HandoffOpenError> {
        request
            .validate()
            .map_err(|_| HandoffOpenError::NotFoundOrNotAuthorized)?;
        Ok(Self {
            session_id,
            scope_digest: request.scope().scope_digest.clone(),
            recipient_actor_id: request.actor().clone(),
        })
    }

    pub fn session_id(&self) -> &HandoffSessionId {
        &self.session_id
    }

    pub fn scope_digest(&self) -> &ManifestDigest {
        &self.scope_digest
    }

    pub fn recipient_actor_id(&self) -> &ActorId {
        &self.recipient_actor_id
    }

    /// True when this grant's context is one the filtering caller may see.
    pub fn matches(&self, context: &HandoffOpenContextV1) -> bool {
        self.session_id == context.session_id
            && self.scope_digest == context.scope_digest
            && self.recipient_actor_id == context.recipient_actor_id
    }
}

/// One enumerated grant, with the consumption instant when it has one.
///
/// The grant itself never holds a bearer secret, and consumption is kept beside
/// it rather than folded into a boolean: a token that was redeemed and a token
/// that expired unredeemed are different outcomes, and a frontier that showed
/// them alike would hide every dropped handoff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffOpenListingV1 {
    pub grant: HandoffOpenGrantV1,
    pub consumed_at: Option<UtcMicros>,
}

pub trait HandoffOpenAuthorityPort: Send + Sync {
    /// Commits a grant or returns the byte-authoritative grant already
    /// committed for the same request identity.
    fn issue(
        &self,
        grant: &HandoffOpenGrantV1,
    ) -> Result<HandoffOpenGrantV1, HandoffOpenAuthorityError>;

    /// Enumerates the grants matching `filter`, newest issuance first, capped
    /// at `limit`.
    ///
    /// Unlike [`Self::resolve`], expired grants are RETAINED and reported: an
    /// expiry that vanished from the frontier would read as a handoff that was
    /// completed rather than one that lapsed. Callers get at most `limit` rows
    /// and are told separately whether more existed.
    fn list(
        &self,
        filter: &HandoffOpenListFilterV1,
        limit: u32,
    ) -> Result<Vec<HandoffOpenListingV1>, HandoffOpenAuthorityError>;

    fn resolve(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenExpectationV1,
        observed_at: UtcMicros,
    ) -> Result<Option<HandoffOpenGrantV1>, HandoffOpenAuthorityError>;

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenExpectationV1,
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

/// Issues a short-lived, version-bound task-opening token.
///
/// Identity, scope, authority revisions, issuance time, and expiry are all
/// supplied by the admitted daemon request. The caller supplies only the
/// bearer, destination session, exact task version, and enrolled recipient
/// principal it intends to authorize.
#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IssueTaskHandoffRequestV1 {
    pub token: String,
    pub session_id: HandoffSessionId,
    pub task_id: TaskId,
    pub version: WorkVersion,
    pub recipient_actor_id: ActorId,
}

/// Enumerates the handoff tokens this caller could redeem in one session.
///
/// The two `open_*` operations redeem a bearer the caller already holds; they
/// cannot answer "what has been handed to me and not yet taken up". This one
/// can, and it does so without any bearer: the request carries no token, and
/// the result carries only digests.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListTaskHandoffsRequestV1 {
    pub session_id: HandoffSessionId,
}

/// Where one enumerated token stands at the observed instant.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskHandoffTokenStateV1 {
    /// Not yet redeemed, and not yet expired — the live frontier.
    Open,
    /// Redeemed. Single-use, so it can never be redeemed again.
    Consumed,
    /// Its window closed with no redemption. A dropped handoff, which is
    /// exactly the fact a frontier exists to surface.
    Expired,
}

/// One handoff token, projected for public reading.
///
/// Mirrors [`IssueTaskHandoffResultV1`]'s doctrine: the complete binding stays
/// inside the daemon authority, and only these identifiers cross the wire. No
/// bearer secret exists to leak here — the authority never stored one — and the
/// issuer's grant identity and policy digests stay concealed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListedTaskHandoffV1 {
    pub token_digest: ManifestDigest,
    pub issued_request_id: RequestId,
    pub session_id: HandoffSessionId,
    pub kind: HandoffOpenKindV1,
    pub target: HandoffOpenTargetV1,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub state: TaskHandoffTokenStateV1,
    /// Set only when `state` is `consumed`.
    pub consumed_at: Option<UtcMicros>,
}

impl ListedTaskHandoffV1 {
    pub fn from_listing(listing: &HandoffOpenListingV1, observed_at: UtcMicros) -> Self {
        let grant = &listing.grant;
        // Consumption is checked before expiry: a token redeemed inside its
        // window and then read after it lapsed was taken up, not dropped.
        let state = match listing.consumed_at {
            Some(_) => TaskHandoffTokenStateV1::Consumed,
            None if observed_at >= *grant.expires_at() => TaskHandoffTokenStateV1::Expired,
            None => TaskHandoffTokenStateV1::Open,
        };
        Self {
            token_digest: grant.token_digest().clone(),
            issued_request_id: grant.issued_request_id().clone(),
            session_id: grant.context().session_id().clone(),
            kind: grant.context().kind(),
            target: grant.target().clone(),
            issued_at: *grant.issued_at(),
            expires_at: *grant.expires_at(),
            state,
            consumed_at: listing.consumed_at,
        }
    }
}

/// The handoff-token frontier for one session.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListTaskHandoffsResultV1 {
    /// The instant the states below were decided at. Without it, `open` and
    /// `expired` are claims with no clock behind them.
    pub observed_at: UtcMicros,
    pub handoffs: Vec<ListedTaskHandoffV1>,
    pub open_count: u32,
    pub consumed_count: u32,
    pub expired_count: u32,
    /// True when the enumeration ceiling was reached, so the counts describe a
    /// prefix of the frontier rather than all of it.
    pub truncated: bool,
}

impl ListTaskHandoffsResultV1 {
    pub fn from_listings(listings: &[HandoffOpenListingV1], observed_at: UtcMicros) -> Self {
        let handoffs: Vec<ListedTaskHandoffV1> = listings
            .iter()
            .map(|listing| ListedTaskHandoffV1::from_listing(listing, observed_at))
            .collect();
        let count = |wanted: TaskHandoffTokenStateV1| {
            u32::try_from(
                handoffs
                    .iter()
                    .filter(|handoff| handoff.state == wanted)
                    .count(),
            )
            .unwrap_or(u32::MAX)
        };
        Self {
            open_count: count(TaskHandoffTokenStateV1::Open),
            consumed_count: count(TaskHandoffTokenStateV1::Consumed),
            expired_count: count(TaskHandoffTokenStateV1::Expired),
            truncated: handoffs.len() as u32 >= MAX_HANDOFF_LIST_RESULTS_V1,
            handoffs,
            observed_at,
        }
    }
}

/// Flat public receipt for a committed task-handoff issue.
///
/// The complete binding remains inside the daemon authority. Publishing only
/// these exact identifiers avoids turning mutable grant and policy internals
/// into a second public wire authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IssueTaskHandoffResultV1 {
    pub token_digest: ManifestDigest,
    pub issued_request_id: RequestId,
    pub session_id: HandoffSessionId,
    pub task_id: TaskId,
    pub version: WorkVersion,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
}

impl IssueTaskHandoffResultV1 {
    pub fn from_grant(grant: &HandoffOpenGrantV1) -> Result<Self, HandoffOpenError> {
        let HandoffOpenTargetV1::Task {
            task_id, version, ..
        } = grant.target()
        else {
            return Err(HandoffOpenError::InvalidBinding);
        };
        Ok(Self {
            token_digest: grant.token_digest().clone(),
            issued_request_id: grant.issued_request_id().clone(),
            session_id: grant.context().session_id().clone(),
            task_id: task_id.clone(),
            version: *version,
            issued_at: *grant.issued_at(),
            expires_at: *grant.expires_at(),
        })
    }
}

impl fmt::Debug for IssueTaskHandoffRequestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssueTaskHandoffRequestV1")
            .field("token", &"[REDACTED]")
            .field("session_id", &self.session_id)
            .field("task_id", &self.task_id)
            .field("version", &self.version)
            .field("recipient_actor_id", &self.recipient_actor_id)
            .finish()
    }
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

impl crate::remote::protocol::RemoteProtocolBodyV1 for IssueTaskHandoffRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        HandoffOpenToken::new(self.token.clone()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "remote task handoff issue token",
            }
        })?;
        self.task_id.validate()?;
        self.recipient_actor_id.validate()?;
        Ok(())
    }
}

impl crate::remote::protocol::RemoteProtocolBodyV1 for OpenTaskHandoffRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        _sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        HandoffOpenToken::new(self.token.clone()).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "remote task handoff open token",
            }
        })?;
        Ok(())
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
    pub binding_digest: ManifestDigest,
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

    pub async fn issue_task(
        &self,
        context: &RequestContext,
        request: IssueTaskHandoffRequestV1,
        authority: HandoffAuthoritySnapshotV1,
        observed_at: UtcMicros,
    ) -> Result<HandoffOpenGrantV1, HandoffOpenError> {
        let expires_at = UtcMicros(
            observed_at
                .0
                .checked_add(MAX_HANDOFF_OPEN_LIFETIME_MICROS)
                .ok_or(HandoffOpenError::InvalidExpiry)?,
        );
        let token = HandoffOpenToken::new(request.token)?;
        let binding = HandoffOpenBindingV1::task(
            context,
            request.session_id,
            request.task_id,
            request.version,
            request.recipient_actor_id,
            authority,
        )?;
        self.issue(context, binding, &token, observed_at, expires_at)
            .await
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
        let grant = HandoffOpenGrantV1::new(
            binding,
            token.digest()?,
            context.request_id().clone(),
            issued_at,
            expires_at,
        )?;
        self.authority.issue(&grant).map_err(authority_error)
    }

    /// Enumerates the handoff tokens this caller could redeem.
    ///
    /// A pure read: nothing is issued, consumed, or otherwise mutated, so it
    /// mints no effect and no token is spent by looking. The authority applies
    /// the same recipient/session/scope match that redemption applies, so a
    /// caller can see exactly the set it could act on and nothing else.
    pub async fn list_task(
        &self,
        context: &RequestContext,
        request: ListTaskHandoffsRequestV1,
        observed_at: UtcMicros,
    ) -> Result<ListTaskHandoffsResultV1, HandoffOpenError> {
        admit(
            context,
            LIST_TASK_HANDOFFS_CAPABILITY_ID_V1,
            LIST_TASK_HANDOFFS_USE_CASE_ID_V1,
            observed_at,
        )?;
        let filter = HandoffOpenListFilterV1::from_request(context, request.session_id)?;
        let listings = self
            .authority
            .list(&filter, MAX_HANDOFF_LIST_RESULTS_V1)
            .map_err(authority_error)?;
        Ok(ListTaskHandoffsResultV1::from_listings(
            &listings,
            observed_at,
        ))
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
        let expected = HandoffOpenExpectationV1::from_request(context, kind, session_id.clone())?;
        let Some(grant) = self
            .authority
            .resolve(&token_digest, &expected, observed_at)
            .map_err(authority_error)?
        else {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        };
        if grant.target().kind() != kind
            || !expected.matches(grant.context())
            || grant.context().authority() != &authority
            || !self
                .targets
                .is_current(context, grant.binding())
                .await
                .map_err(|_| HandoffOpenError::AuthorityUnavailable)?
        {
            return Err(HandoffOpenError::NotFoundOrNotAuthorized);
        }
        let input_digest = handoff_open_consumption_input_digest(
            kind,
            &session_id,
            context.scope(),
            context.actor(),
            &token_digest,
        )
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
            HandoffOpenConsumeOutcomeV1::Consumed(consumption) => *consumption,
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
