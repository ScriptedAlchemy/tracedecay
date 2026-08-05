use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
    canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::error::ApplicationContractError;
use crate::identity::application_identifier;

const RESOLVED_SCOPE_DIGEST_DOMAIN: &str = "tracedecay.application.scope.v1";

application_identifier!(
    RequestId => ("request id", 512),
    CapabilityGrantId => ("capability grant id", 512),
    CancellationTokenId => ("cancellation token id", 512),
);

/// The resolved PR11 scope is one exact project/repository/worktree root.
///
/// Paths, CWDs, labels, and mutable branch spellings are deliberately absent.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedScope {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub reference: Option<RefId>,
    pub scope_digest: ManifestDigest,
}

impl ResolvedScope {
    pub fn new(
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        reference: Option<RefId>,
    ) -> Result<Self, ApplicationContractError> {
        project_id.validate()?;
        repository_id.validate()?;
        worktree_id.validate()?;
        if let Some(reference) = &reference {
            reference.validate()?;
        }
        let mut scope = Self {
            project_id,
            repository_id,
            worktree_id,
            reference,
            scope_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))?,
        };
        scope.scope_digest = scope.compute_digest()?;
        Ok(scope)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        Ok(canonical_sha256(&(
            RESOLVED_SCOPE_DIGEST_DOMAIN,
            &self.project_id,
            &self.repository_id,
            &self.worktree_id,
            &self.reference,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        if let Some(reference) = &self.reference {
            reference.validate()?;
        }
        self.scope_digest.validate()?;
        if self.scope_digest != self.compute_digest()? {
            return Err(ApplicationContractError::Inconsistent {
                field: "resolved scope digest",
            });
        }
        Ok(())
    }
}

/// Disclosure ceiling carried by an immutable grant and revalidated at sinks.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureClass {
    Metadata,
    Evidence,
    Sensitive,
}

/// Immutable, pre-resolved grant input. The application may narrow or reject
/// it, but cannot issue, renew, or widen it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrantSnapshot {
    pub grant_id: CapabilityGrantId,
    pub revision: u64,
    pub digest: ManifestDigest,
    pub issuer: ActorId,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub scope: ResolvedScope,
    pub allowed_capabilities: BTreeSet<CapabilityId>,
    pub allowed_use_cases: BTreeSet<UseCaseId>,
    pub disclosure: DisclosureClass,
}

impl CapabilityGrantSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grant_id: CapabilityGrantId,
        revision: u64,
        digest: ManifestDigest,
        issuer: ActorId,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
        scope: ResolvedScope,
        allowed_capabilities: BTreeSet<CapabilityId>,
        allowed_use_cases: BTreeSet<UseCaseId>,
        disclosure: DisclosureClass,
    ) -> Result<Self, ApplicationContractError> {
        let grant = Self {
            grant_id,
            revision,
            digest,
            issuer,
            issued_at,
            expires_at,
            scope,
            allowed_capabilities,
            allowed_use_cases,
            disclosure,
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "capability grant revision",
            });
        }
        self.digest.validate()?;
        self.issuer.validate()?;
        self.scope.validate()?;
        if self.expires_at <= self.issued_at {
            return Err(ApplicationContractError::InvalidRange {
                field: "capability grant validity",
            });
        }
        if self.allowed_capabilities.is_empty() || self.allowed_use_cases.is_empty() {
            return Err(ApplicationContractError::Inconsistent {
                field: "capability grant operation set",
            });
        }
        Ok(())
    }

    pub fn is_expired_at(&self, observed_at: UtcMicros) -> bool {
        observed_at >= self.expires_at
    }
}

/// One immutable deadline supplied by the caller or upstream admission layer.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Deadline {
    pub expires_at: UtcMicros,
}

impl Deadline {
    pub fn new(expires_at: UtcMicros) -> Result<Self, ApplicationContractError> {
        Ok(Self { expires_at })
    }

    pub fn is_elapsed_at(&self, observed_at: UtcMicros) -> bool {
        observed_at >= self.expires_at
    }
}

/// Immutable cancellation observation. Runtime cancellation execution belongs
/// to the caller or owning runtime, never to this application crate.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum CancellationState {
    Active,
    Cancelled { requested_at: UtcMicros },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CancellationContext {
    pub token_id: CancellationTokenId,
    pub state: CancellationState,
}

impl CancellationContext {
    pub fn active(token_id: impl Into<String>) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            token_id: CancellationTokenId::new(token_id)?,
            state: CancellationState::Active,
        })
    }

    pub fn cancelled(
        token_id: impl Into<String>,
        requested_at: UtcMicros,
    ) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            token_id: CancellationTokenId::new(token_id)?,
            state: CancellationState::Cancelled { requested_at },
        })
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(self.state, CancellationState::Cancelled { .. })
    }
}

const CANCELLATION_ACTIVE: u8 = 0;
const CANCELLATION_REQUESTING: u8 = 1;
const CANCELLATION_CANCELLED: u8 = 2;
const CANCELLATION_COMMIT_STARTED: u8 = 3;

#[derive(Debug)]
struct CancellationSignalState {
    phase: AtomicU8,
    requested_at: AtomicI64,
}

/// One live transport cancellation identity shared by adapter clones.
///
/// Serialization uses [`Self::context`] at the daemon boundary; the live
/// signal itself remains process-local so disconnect and protocol-cancel
/// observers update the same token rather than manufacturing replacement
/// contexts.
#[derive(Clone, Debug)]
pub struct CancellationSignal {
    token_id: CancellationTokenId,
    state: Arc<CancellationSignalState>,
}

impl CancellationSignal {
    pub fn active(token_id: impl Into<String>) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            token_id: CancellationTokenId::new(token_id)?,
            state: Arc::new(CancellationSignalState {
                phase: AtomicU8::new(CANCELLATION_ACTIVE),
                requested_at: AtomicI64::new(0),
            }),
        })
    }

    pub fn cancel(&self, requested_at: UtcMicros) -> bool {
        if self
            .state
            .phase
            .compare_exchange(
                CANCELLATION_ACTIVE,
                CANCELLATION_REQUESTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        self.state
            .requested_at
            .store(requested_at.0, Ordering::Release);
        self.state
            .phase
            .store(CANCELLATION_CANCELLED, Ordering::Release);
        true
    }

    pub fn try_begin_commit(&self) -> bool {
        self.state
            .phase
            .compare_exchange(
                CANCELLATION_ACTIVE,
                CANCELLATION_COMMIT_STARTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn commit_started(&self) -> bool {
        self.phase() == CANCELLATION_COMMIT_STARTED
    }

    pub fn context(&self) -> CancellationContext {
        let phase = self.phase();
        CancellationContext {
            token_id: self.token_id.clone(),
            state: if phase == CANCELLATION_CANCELLED {
                CancellationState::Cancelled {
                    requested_at: UtcMicros(self.state.requested_at.load(Ordering::Acquire)),
                }
            } else {
                CancellationState::Active
            },
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.phase() == CANCELLATION_CANCELLED
    }

    pub fn cancelled_at(&self) -> Option<UtcMicros> {
        (self.phase() == CANCELLATION_CANCELLED)
            .then(|| UtcMicros(self.state.requested_at.load(Ordering::Acquire)))
    }

    fn phase(&self) -> u8 {
        loop {
            let phase = self.state.phase.load(Ordering::Acquire);
            if phase != CANCELLATION_REQUESTING {
                return phase;
            }
            std::hint::spin_loop();
        }
    }
}

/// Admission state observed at a caller-supplied time. No wall clock is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestAdmission {
    Admitted,
    Cancelled,
    TimedOut,
}

/// Transport-neutral request context required by every application use case.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequestContext {
    actor: ActorId,
    scope: ResolvedScope,
    grant: CapabilityGrantSnapshot,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationContext,
}

impl RequestContext {
    pub fn new(
        actor: ActorId,
        scope: ResolvedScope,
        grant: CapabilityGrantSnapshot,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationContext,
    ) -> Result<Self, ApplicationContractError> {
        let context = Self {
            actor,
            scope,
            grant,
            request_id,
            deadline,
            cancellation,
        };
        context.validate()?;
        Ok(context)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.actor.validate()?;
        self.scope.validate()?;
        self.grant.validate()?;
        if self.scope != self.grant.scope {
            return Err(ApplicationContractError::Inconsistent {
                field: "request context grant scope",
            });
        }
        Ok(())
    }

    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn grant(&self) -> &CapabilityGrantSnapshot {
        &self.grant
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    pub fn cancellation(&self) -> &CancellationContext {
        &self.cancellation
    }

    pub fn with_deadline(mut self, deadline: Deadline) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn with_cancellation(mut self, cancellation: CancellationContext) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn admission_at(&self, observed_at: UtcMicros) -> RequestAdmission {
        if self.cancellation.is_cancelled() {
            RequestAdmission::Cancelled
        } else if self.deadline.is_elapsed_at(observed_at) || self.grant.is_expired_at(observed_at)
        {
            RequestAdmission::TimedOut
        } else {
            RequestAdmission::Admitted
        }
    }

    pub fn allows(&self, capability_id: &CapabilityId, use_case_id: &UseCaseId) -> bool {
        self.grant.scope == self.scope
            && self.grant.allowed_capabilities.contains(capability_id)
            && self.grant.allowed_use_cases.contains(use_case_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{CancellationSignal, CancellationState};
    use tracedecay_domain::UtcMicros;

    #[test]
    fn cancellation_signal_clones_share_one_runtime_token() {
        let signal = CancellationSignal::active("cancel.transport.fixture").unwrap();
        let waiter = signal.clone();

        signal.cancel(UtcMicros(41));
        assert_eq!(waiter.cancelled_at(), Some(UtcMicros(41)));
        assert!(matches!(
            waiter.context().state,
            CancellationState::Cancelled {
                requested_at: UtcMicros(41)
            }
        ));
    }

    #[test]
    fn cancellation_wins_commit_arbitration() {
        let signal = CancellationSignal::active("cancel.before-commit.fixture").unwrap();

        assert!(signal.cancel(UtcMicros(41)));
        assert!(!signal.try_begin_commit());
        assert!(!signal.commit_started());
        assert_eq!(signal.cancelled_at(), Some(UtcMicros(41)));
    }

    #[test]
    fn commit_claim_wins_cancellation_arbitration() {
        let signal = CancellationSignal::active("commit.before-cancel.fixture").unwrap();

        assert!(signal.try_begin_commit());
        assert!(signal.commit_started());
        assert!(!signal.cancel(UtcMicros(41)));
        assert!(!signal.is_cancelled());
        assert_eq!(signal.cancelled_at(), None);
        assert!(matches!(signal.context().state, CancellationState::Active));
    }
}
