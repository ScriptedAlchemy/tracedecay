//! Durable, source-owned managed-Test failure bindings for Work retries.
//!
//! Work mints one active opaque token for one exact terminal attempt. Its trusted
//! managed-Test launcher records the operation id in Work storage before the
//! process starts; it then seals the terminal source record before returning.
//! The seal atomically creates retry evidence for a failure and retires the
//! token for every terminal result. Retry admission therefore survives a
//! daemon restart and never infers lineage from root, commit, or caller input.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, UtcMicros, WorkAttemptIdentityV1, WorkAttemptV1, WorkAuthority,
};

use crate::work::work_authority;
use crate::work_attempt::{WorkAttemptStorageError, WorkAttemptStoragePort};
use crate::work_retry::{
    VerifiedWorkRetryFailureV1, WorkRetryCauseV1, WorkRetryEvidenceErrorV1,
    WorkRetryEvidencePortV1, WorkRetryFailureSelectorV1, WorkRetrySourceV1,
};
use crate::{
    ApplicationProblem, LegalAction, OperationReceipt, RequestAdmission, RequestContext,
    RetryDirective, SafeDiagnostic,
};

const TEST_TOKEN_PREFIX_V1: &str = "work-retry-test-v1-";
const TEST_TOKEN_HEX_BYTES_V1: usize = 32;
const MAX_SOURCE_OPERATION_ID_BYTES_V1: usize = 192;

/// Opaque, Work-minted token that a trusted managed-Test launch carries into
/// its terminal journal entry. The database, not syntax, proves ownership.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkRetryTestBindingTokenV1(String);

impl WorkRetryTestBindingTokenV1 {
    fn mint() -> Result<Self, WorkRetryEvidenceBindingErrorV1> {
        let mut random = [0_u8; TEST_TOKEN_HEX_BYTES_V1];
        getrandom::getrandom(&mut random)
            .map_err(|_| WorkRetryEvidenceBindingErrorV1::Unavailable)?;
        Self::new(format!("{TEST_TOKEN_PREFIX_V1}{}", hex::encode(random)))
    }

    pub fn new(value: impl Into<String>) -> Result<Self, WorkRetryEvidenceBindingErrorV1> {
        let value = value.into();
        let expected_len = TEST_TOKEN_PREFIX_V1.len() + TEST_TOKEN_HEX_BYTES_V1 * 2;
        if value.len() != expected_len
            || !value.starts_with(TEST_TOKEN_PREFIX_V1)
            || !value[TEST_TOKEN_PREFIX_V1.len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(WorkRetryEvidenceBindingErrorV1::Invalid);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WorkRetryTestBindingTokenV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Work's request to launch managed Tests for an exact prior attempt. It has
/// no caller-supplied test outcome, digest, scope, or timestamp.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryTestBindingTokenRequestV1 {
    pub original_attempt: WorkAttemptIdentityV1,
}

/// Public opaque capability returned by Work. The managed-Test tool accepts it
/// as `work_retry_test_binding_token`, then resolves its authority only from
/// the registered Work store.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryTestBindingTokenOutcomeV1 {
    pub token: WorkRetryTestBindingTokenV1,
}

/// A retained managed-Test operation reference. The source producer creates
/// it from the operation it owns; callers cannot select an original attempt.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryEvidenceBindingSourceV1 {
    operation_id: String,
}

impl WorkRetryEvidenceBindingSourceV1 {
    pub fn test(operation_id: String) -> Result<Self, WorkRetryEvidenceBindingErrorV1> {
        validate_operation_id(&operation_id)?;
        Ok(Self { operation_id })
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn selector(&self) -> WorkRetryFailureSelectorV1 {
        WorkRetryFailureSelectorV1 {
            source: WorkRetrySourceV1::Test,
            cause: WorkRetryCauseV1::TestFailure,
            evidence_ref: format!("test-failure:{}", self.operation_id),
        }
    }

    pub fn validate(&self) -> Result<(), WorkRetryEvidenceBindingErrorV1> {
        validate_operation_id(&self.operation_id)
    }
}

/// Immutable durable source-to-attempt relation created when the test source
/// terminal is sealed. It has no public command constructor.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryEvidenceBindingV1 {
    source: WorkRetryEvidenceBindingSourceV1,
    token: WorkRetryTestBindingTokenV1,
    original_attempt: WorkAttemptIdentityV1,
    selector: WorkRetryFailureSelectorV1,
    evidence_digest: ManifestDigest,
    observed_at: UtcMicros,
}

impl WorkRetryEvidenceBindingV1 {
    /// Storage adapters call this only while sealing a launched token in the
    /// same transaction that stores the immutable source terminal.
    pub fn from_sealed_test_terminal(
        source: WorkRetryEvidenceBindingSourceV1,
        token: WorkRetryTestBindingTokenV1,
        original_attempt: WorkAttemptIdentityV1,
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRetryEvidenceBindingErrorV1> {
        let binding = Self {
            selector: source.selector(),
            source,
            token,
            original_attempt,
            evidence_digest,
            observed_at,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn source(&self) -> &WorkRetryEvidenceBindingSourceV1 {
        &self.source
    }

    pub fn token(&self) -> &WorkRetryTestBindingTokenV1 {
        &self.token
    }

    pub fn original_attempt(&self) -> &WorkAttemptIdentityV1 {
        &self.original_attempt
    }

    pub fn selector(&self) -> &WorkRetryFailureSelectorV1 {
        &self.selector
    }

    pub fn evidence_digest(&self) -> &ManifestDigest {
        &self.evidence_digest
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }

    pub fn validate(&self) -> Result<(), WorkRetryEvidenceBindingErrorV1> {
        self.source.validate()?;
        WorkRetryTestBindingTokenV1::new(self.token.0.clone())?;
        if self.evidence_digest.validate().is_err()
            || self.selector != self.source.selector()
            || self.selector.evidence_ref.len() > 256
        {
            return Err(WorkRetryEvidenceBindingErrorV1::Invalid);
        }
        Ok(())
    }

    fn into_outcome(self) -> WorkRetryEvidenceBindingOutcomeV1 {
        WorkRetryEvidenceBindingOutcomeV1 {
            original_attempt: self.original_attempt,
            selector: self.selector,
            evidence_digest: self.evidence_digest,
            observed_at: self.observed_at,
        }
    }
}

/// Receipt from a sealed Test source with observed failing test names. Any
/// terminal without an affirmative failed-test observation still seals its
/// token, but creates no retry evidence and returns `None`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryEvidenceBindingOutcomeV1 {
    pub original_attempt: WorkAttemptIdentityV1,
    pub selector: WorkRetryFailureSelectorV1,
    pub evidence_digest: ManifestDigest,
    pub observed_at: UtcMicros,
}

/// Exact terminal evidence written by the trusted managed-Test runner before
/// it returns. This is a source record, never a retry command payload.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRetryTestFailureEvidenceV1 {
    pub operation_id: String,
    pub token: WorkRetryTestBindingTokenV1,
    pub terminal: OperationReceipt,
    pub failed_tests: Vec<String>,
}

impl WorkRetryTestFailureEvidenceV1 {
    pub fn observed_at(&self) -> UtcMicros {
        self.terminal.ended_at
    }

    pub fn is_failure(&self) -> bool {
        // A process error, cancellation, timeout, or truncated output is not
        // proof that a test failed. Retire the token for those terminals, but
        // never relabel operational failure as `TestFailure` retry evidence.
        self.terminal.termination == crate::OperationTermination::Completed
            && !self.failed_tests.is_empty()
    }

    pub fn validate(&self) -> Result<(), WorkRetryEvidenceBindingErrorV1> {
        validate_operation_id(&self.operation_id)?;
        WorkRetryTestBindingTokenV1::new(self.token.0.clone())?;
        if self.terminal.validate().is_err()
            || self.failed_tests.len() > 256
            || self.failed_tests.windows(2).any(|pair| pair[0] >= pair[1])
            || self.failed_tests.iter().any(|test| {
                test.is_empty()
                    || test.len() > 1_024
                    || test.trim() != test
                    || test.chars().any(char::is_control)
            })
        {
            return Err(WorkRetryEvidenceBindingErrorV1::Invalid);
        }
        Ok(())
    }
}

/// Canonical durable operations required by the Work-bound managed-Test
/// runner. Launch and seal are not public tool commands.
pub trait WorkRetryEvidenceBindingStoragePortV1: WorkAttemptStoragePort {
    /// Resolves the one registered Work authority that minted this bearer
    /// token. A source handler uses it only to construct the journal; it
    /// never receives caller-selected authority fields.
    fn resolve_test_retry_binding_authority(
        &self,
        token: &WorkRetryTestBindingTokenV1,
    ) -> Result<WorkAuthority, WorkAttemptStorageError>;

    fn mint_test_retry_binding_token(
        &self,
        authority: &WorkAuthority,
        original_attempt: &WorkAttemptIdentityV1,
        token: &WorkRetryTestBindingTokenV1,
        minted_at: UtcMicros,
    ) -> Result<WorkRetryTestBindingTokenV1, WorkAttemptStorageError>;

    fn launch_test_retry_binding(
        &self,
        authority: &WorkAuthority,
        source: &WorkRetryEvidenceBindingSourceV1,
        token: &WorkRetryTestBindingTokenV1,
    ) -> Result<(), WorkAttemptStorageError>;

    fn seal_test_retry_terminal(
        &self,
        authority: &WorkAuthority,
        evidence: &WorkRetryTestFailureEvidenceV1,
    ) -> Result<Option<WorkRetryEvidenceBindingV1>, WorkAttemptStorageError>;

    fn load_retry_evidence(
        &self,
        authority: &WorkAuthority,
        original_attempt: &WorkAttemptIdentityV1,
        selector: &WorkRetryFailureSelectorV1,
    ) -> Result<Option<WorkRetryEvidenceBindingV1>, WorkAttemptStorageError>;
}

/// Mints the active token for an exact terminal non-success attempt. A sealed
/// operational terminal without affirmative failed tests permits a fresh token;
/// a retained TestFailure does not.
pub struct WorkRetryTestBindingTokenServiceV1<S> {
    storage: S,
}

impl<S> WorkRetryTestBindingTokenServiceV1<S>
where
    S: WorkRetryEvidenceBindingStoragePortV1,
{
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }

    pub fn mint_for_attempt(
        &self,
        context: &RequestContext,
        bound_at: UtcMicros,
        request: WorkRetryTestBindingTokenRequestV1,
    ) -> Result<WorkRetryTestBindingTokenOutcomeV1, ApplicationProblem> {
        admit(context, bound_at)?;
        let authority = work_authority(context)?;
        let original = self
            .storage
            .load(&authority, &request.original_attempt)
            .map_err(storage_problem)?;
        require_terminal_non_success(&original)?;
        let candidate = WorkRetryTestBindingTokenV1::mint().map_err(invalid_problem)?;
        let token = self
            .storage
            .mint_test_retry_binding_token(
                &authority,
                &request.original_attempt,
                &candidate,
                bound_at,
            )
            .map_err(storage_problem)?;
        Ok(WorkRetryTestBindingTokenOutcomeV1 { token })
    }
}

/// Work-owned durable journal injected into the internal test runner. It
/// cannot accept an original attempt, result digest, or source timestamp from
/// a public caller.
#[derive(Clone)]
pub struct WorkRetryManagedTestJournalV1<S> {
    storage: S,
    authority: WorkAuthority,
    token: WorkRetryTestBindingTokenV1,
}

impl<S> WorkRetryManagedTestJournalV1<S> {
    pub const fn new(
        storage: S,
        authority: WorkAuthority,
        token: WorkRetryTestBindingTokenV1,
    ) -> Self {
        Self {
            storage,
            authority,
            token,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkRetryManagedTestJournalErrorV1 {
    #[error("managed Test retry journal source was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("managed Test retry journal source conflicts with its token")]
    Conflict,
    #[error("managed Test retry journal is unavailable")]
    Unavailable,
}

/// Object-safe runner port so the host handler can persist the exact terminal
/// before it produces a tool result.
pub trait WorkRetryManagedTestJournalPortV1: Send + Sync {
    fn token(&self) -> &WorkRetryTestBindingTokenV1;

    fn launch(&self, operation_id: &str) -> Result<(), WorkRetryManagedTestJournalErrorV1>;

    fn seal(
        &self,
        evidence: WorkRetryTestFailureEvidenceV1,
    ) -> Result<Option<WorkRetryEvidenceBindingOutcomeV1>, WorkRetryManagedTestJournalErrorV1>;
}

impl<S> WorkRetryManagedTestJournalPortV1 for WorkRetryManagedTestJournalV1<S>
where
    S: WorkRetryEvidenceBindingStoragePortV1,
{
    fn token(&self) -> &WorkRetryTestBindingTokenV1 {
        &self.token
    }

    fn launch(&self, operation_id: &str) -> Result<(), WorkRetryManagedTestJournalErrorV1> {
        let source = WorkRetryEvidenceBindingSourceV1::test(operation_id.to_owned())
            .map_err(journal_contract_error)?;
        self.storage
            .launch_test_retry_binding(&self.authority, &source, &self.token)
            .map_err(journal_storage_error)
    }

    fn seal(
        &self,
        evidence: WorkRetryTestFailureEvidenceV1,
    ) -> Result<Option<WorkRetryEvidenceBindingOutcomeV1>, WorkRetryManagedTestJournalErrorV1> {
        evidence.validate().map_err(journal_contract_error)?;
        if evidence.token != self.token {
            return Err(WorkRetryManagedTestJournalErrorV1::Conflict);
        }
        self.storage
            .seal_test_retry_terminal(&self.authority, &evidence)
            .map(|binding| binding.map(WorkRetryEvidenceBindingV1::into_outcome))
            .map_err(journal_storage_error)
    }
}

/// Durable Test evidence reader used by retry admission. Unsupported source
/// kinds have no selector or evidence implementation in this surface.
#[derive(Clone)]
pub struct StoredWorkRetryEvidenceV1<S> {
    storage: S,
}

impl<S> StoredWorkRetryEvidenceV1<S> {
    pub const fn new(storage: S) -> Self {
        Self { storage }
    }
}

impl<S> WorkRetryEvidencePortV1 for StoredWorkRetryEvidenceV1<S>
where
    S: WorkRetryEvidenceBindingStoragePortV1,
{
    fn resolve_failure(
        &self,
        authority: &WorkAuthority,
        original: &WorkAttemptV1,
        selector: &WorkRetryFailureSelectorV1,
    ) -> Result<VerifiedWorkRetryFailureV1, WorkRetryEvidenceErrorV1> {
        if selector.source != WorkRetrySourceV1::Test {
            return Err(WorkRetryEvidenceErrorV1::NotFoundOrNotAuthorized);
        }
        let binding = self
            .storage
            .load_retry_evidence(authority, original.identity(), selector)
            .map_err(storage_evidence_error)?
            .ok_or(WorkRetryEvidenceErrorV1::NotFoundOrNotAuthorized)?;
        binding
            .validate()
            .map_err(|_| WorkRetryEvidenceErrorV1::Conflict)?;
        if binding.original_attempt() != original.identity() || binding.selector() != selector {
            return Err(WorkRetryEvidenceErrorV1::Conflict);
        }
        Ok(VerifiedWorkRetryFailureV1 {
            selector: selector.clone(),
            evidence_digest: binding.evidence_digest().clone(),
            observed_at: binding.observed_at(),
        })
    }
}

/// One retry evidence port spanning runtime terminal evidence and durable Test
/// journal facts without relabelling either source.
#[derive(Clone)]
pub struct CompositeWorkRetryEvidenceV1<R, S> {
    runtime: R,
    bindings: StoredWorkRetryEvidenceV1<S>,
}

impl<R, S> CompositeWorkRetryEvidenceV1<R, S> {
    pub const fn new(runtime: R, storage: S) -> Self {
        Self {
            runtime,
            bindings: StoredWorkRetryEvidenceV1::new(storage),
        }
    }
}

impl<R, S> WorkRetryEvidencePortV1 for CompositeWorkRetryEvidenceV1<R, S>
where
    R: WorkRetryEvidencePortV1,
    S: WorkRetryEvidenceBindingStoragePortV1,
{
    fn resolve_failure(
        &self,
        authority: &WorkAuthority,
        original: &WorkAttemptV1,
        selector: &WorkRetryFailureSelectorV1,
    ) -> Result<VerifiedWorkRetryFailureV1, WorkRetryEvidenceErrorV1> {
        match selector.source {
            WorkRetrySourceV1::Runtime => {
                self.runtime.resolve_failure(authority, original, selector)
            }
            WorkRetrySourceV1::Test => self.bindings.resolve_failure(authority, original, selector),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkRetryEvidenceBindingErrorV1 {
    #[error("retry evidence binding is invalid")]
    Invalid,
    #[error("retry evidence binding conflicts with canonical source evidence")]
    Conflict,
    #[error("retry evidence binding token could not be minted")]
    Unavailable,
}

fn validate_operation_id(value: &str) -> Result<(), WorkRetryEvidenceBindingErrorV1> {
    if value.is_empty()
        || value.len() > MAX_SOURCE_OPERATION_ID_BYTES_V1
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
    {
        return Err(WorkRetryEvidenceBindingErrorV1::Invalid);
    }
    Ok(())
}

fn require_terminal_non_success(original: &WorkAttemptV1) -> Result<(), ApplicationProblem> {
    let eligible = matches!(
        original.terminal(),
        Some(
            tracedecay_domain::WorkTerminalEvidenceV1::Failed { .. }
                | tracedecay_domain::WorkTerminalEvidenceV1::TimedOut { .. }
                | tracedecay_domain::WorkTerminalEvidenceV1::Cancelled { .. }
        )
    );
    eligible.then_some(()).ok_or_else(conflict_problem)
}

fn admit(context: &RequestContext, bound_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(bound_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn journal_contract_error(
    error: WorkRetryEvidenceBindingErrorV1,
) -> WorkRetryManagedTestJournalErrorV1 {
    match error {
        WorkRetryEvidenceBindingErrorV1::Unavailable => {
            WorkRetryManagedTestJournalErrorV1::Unavailable
        }
        WorkRetryEvidenceBindingErrorV1::Invalid | WorkRetryEvidenceBindingErrorV1::Conflict => {
            WorkRetryManagedTestJournalErrorV1::Conflict
        }
    }
}

fn journal_storage_error(error: WorkAttemptStorageError) -> WorkRetryManagedTestJournalErrorV1 {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => {
            WorkRetryManagedTestJournalErrorV1::NotFoundOrNotAuthorized
        }
        WorkAttemptStorageError::Unavailable => WorkRetryManagedTestJournalErrorV1::Unavailable,
        WorkAttemptStorageError::AttemptConflict
        | WorkAttemptStorageError::RunAdmissionConflict
        | WorkAttemptStorageError::ReservationFenced
        | WorkAttemptStorageError::FenceConflict
        | WorkAttemptStorageError::CapacityExceeded => WorkRetryManagedTestJournalErrorV1::Conflict,
    }
}

fn storage_problem(error: WorkAttemptStorageError) -> ApplicationProblem {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkAttemptStorageError::AttemptConflict
        | WorkAttemptStorageError::RunAdmissionConflict
        | WorkAttemptStorageError::ReservationFenced
        | WorkAttemptStorageError::FenceConflict => conflict_problem(),
        WorkAttemptStorageError::CapacityExceeded => ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic {
                code: "application.work-retry-binding.saturated".to_owned(),
                message: "The Work retry evidence binding is temporarily saturated.".to_owned(),
            },
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
        WorkAttemptStorageError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-retry-binding.unavailable".to_owned(),
            message: "The Work retry evidence authority is unavailable.".to_owned(),
        }),
    }
}

fn storage_evidence_error(error: WorkAttemptStorageError) -> WorkRetryEvidenceErrorV1 {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => {
            WorkRetryEvidenceErrorV1::NotFoundOrNotAuthorized
        }
        WorkAttemptStorageError::Unavailable => WorkRetryEvidenceErrorV1::Unavailable,
        WorkAttemptStorageError::AttemptConflict
        | WorkAttemptStorageError::RunAdmissionConflict
        | WorkAttemptStorageError::ReservationFenced
        | WorkAttemptStorageError::FenceConflict
        | WorkAttemptStorageError::CapacityExceeded => WorkRetryEvidenceErrorV1::Conflict,
    }
}

fn invalid_problem(error: WorkRetryEvidenceBindingErrorV1) -> ApplicationProblem {
    match error {
        WorkRetryEvidenceBindingErrorV1::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-retry-binding.token-unavailable".to_owned(),
                message: "The Work retry evidence token authority is unavailable.".to_owned(),
            })
        }
        WorkRetryEvidenceBindingErrorV1::Invalid | WorkRetryEvidenceBindingErrorV1::Conflict => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "application.work-retry-binding.invalid".to_owned(),
                    message: "The Work retry evidence binding is invalid.".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
            }
        }
    }
}

fn conflict_problem() -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: "application.work-retry-binding.conflict".to_owned(),
            message: "The source failure does not bind the selected Work attempt.".to_owned(),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}
