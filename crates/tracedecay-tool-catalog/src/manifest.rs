use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{BindingId, CapabilityId, FeatureId, ProfileId, SchemaId, UseCaseId};
use crate::validation::CatalogValidationError;

/// A reviewed reference to a typed request or result schema.
///
/// The catalog never carries schema bodies. Identity and revision bind each
/// capability to its reviewed request and result contracts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SchemaRef {
    schema_id: SchemaId,
    revision: u32,
}

impl SchemaRef {
    pub fn new(schema_id: SchemaId, revision: u32) -> Result<Self, CatalogValidationError> {
        if revision == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "schema revision",
                reason: "must be greater than zero",
            });
        }
        Ok(Self {
            schema_id,
            revision,
        })
    }

    pub fn schema_id(&self) -> &SchemaId {
        &self.schema_id
    }

    pub const fn revision(&self) -> u32 {
        self.revision
    }
}

/// Scope dimensions an application use case requires before it is admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDimension {
    /// One exact typed configuration layer. Its project, profile, or
    /// collection identity is revalidated by the configuration authority.
    ConfigurationLayer,
    Project,
    Repository,
    Worktree,
    Branch,
    Session,
    Resource,
}

/// Immutable scope requirements for a capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScopeRequirement {
    dimensions: Vec<ScopeDimension>,
}

impl ScopeRequirement {
    pub fn none() -> Self {
        Self {
            dimensions: Vec::new(),
        }
    }

    pub fn new(mut dimensions: Vec<ScopeDimension>) -> Result<Self, CatalogValidationError> {
        canonicalize_set(&mut dimensions, "scope dimensions")?;
        Ok(Self { dimensions })
    }

    pub fn dimensions(&self) -> &[ScopeDimension] {
        &self.dimensions
    }

    pub fn requires(&self, dimension: ScopeDimension) -> bool {
        self.dimensions.binary_search(&dimension).is_ok()
    }

    pub fn is_empty(&self) -> bool {
        self.dimensions.is_empty()
    }
}

/// How application authorization is established and refreshed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityRequirement {
    None,
    CapabilityGrant,
    CapabilityGrantWithRevalidation,
}

/// Public behavior when direct resource access is denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedDisclosurePolicy {
    Indistinguishable,
    Explicit,
}

/// Privacy classification used by discovery and authorization policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    PublicMetadata,
    ScopedMetadata,
    Sensitive,
    Administrative,
}

/// State retained by the caller or protocol while an operation is in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleClass {
    Stateless,
    ConnectionStateful,
    SessionStateful,
    Resumable,
}

/// The stable effect classification of one application operation.
///
/// Git index writes remain separate classes so policy cannot accidentally
/// substitute one index mutation for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    Read,
    Preview,
    SourceEdit,
    GitIndexStage,
    GitIndexUnstage,
    GitIndexCommit,
    ConfigurationWrite,
    Administrative,
}

impl EffectClass {
    pub const fn is_effect(self) -> bool {
        !matches!(self, Self::Read | Self::Preview)
    }

    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Read | Self::Preview)
    }
}

/// Whether a streaming result can be resumed after a bounded interruption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamResumeContract {
    NotResumable,
    Resumable,
}

/// Bounded streaming metadata. It describes events only; it is not a transport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum StreamingContract {
    Unsupported,
    Bounded {
        maximum_events: u32,
        maximum_bytes: u32,
        resume: StreamResumeContract,
    },
}

impl StreamingContract {
    pub fn bounded(
        maximum_events: u32,
        maximum_bytes: u32,
        resume: StreamResumeContract,
    ) -> Result<Self, CatalogValidationError> {
        if maximum_events == 0 || maximum_bytes == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "stream budget",
                reason: "maximum events and bytes must be greater than zero",
            });
        }
        Ok(Self::Bounded {
            maximum_events,
            maximum_bytes,
            resume,
        })
    }

    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Bounded { .. })
    }
}

/// A stage at which cancellation is observed and recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationPoint {
    BeforeAdmission,
    BeforeRead,
    DuringRead,
    BeforeEffect,
    EffectInFlight,
    Reconciling,
    AfterCommit,
}

/// Cancellation semantics declared by an application operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum CancellationContract {
    NotCancellable,
    Cooperative { points: Vec<CancellationPoint> },
}

impl CancellationContract {
    pub fn cooperative(mut points: Vec<CancellationPoint>) -> Result<Self, CatalogValidationError> {
        if points.is_empty() {
            return Err(CatalogValidationError::MissingValue {
                field: "cancellation points",
            });
        }
        canonicalize_set(&mut points, "cancellation points")?;
        Ok(Self::Cooperative { points })
    }

    pub fn points(&self) -> &[CancellationPoint] {
        match self {
            Self::NotCancellable => &[],
            Self::Cooperative { points } => points,
        }
    }

    pub fn observes(&self, point: CancellationPoint) -> bool {
        self.points().binary_search(&point).is_ok()
    }
}

/// Result behavior after an authorized deadline expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadlineBehavior {
    RejectBeforeAdmission,
    ReturnOperationReceipt,
    ReturnEffectReceipt,
}

/// Maximum permitted deadline and terminal behavior for an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeadlineContract {
    maximum_millis: u64,
    behavior: DeadlineBehavior,
}

impl DeadlineContract {
    pub fn new(
        maximum_millis: u64,
        behavior: DeadlineBehavior,
    ) -> Result<Self, CatalogValidationError> {
        if maximum_millis == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "maximum deadline",
                reason: "must be greater than zero",
            });
        }
        Ok(Self {
            maximum_millis,
            behavior,
        })
    }

    pub const fn maximum_millis(&self) -> u64 {
        self.maximum_millis
    }

    pub const fn behavior(&self) -> DeadlineBehavior {
        self.behavior
    }
}

/// Cursor behavior for a bounded, paginated operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaginationContract {
    default_page_size: u32,
    maximum_page_size: u32,
    cursor_ttl_millis: u64,
}

impl PaginationContract {
    pub fn new(
        default_page_size: u32,
        maximum_page_size: u32,
        cursor_ttl_millis: u64,
    ) -> Result<Self, CatalogValidationError> {
        if default_page_size == 0
            || maximum_page_size == 0
            || default_page_size > maximum_page_size
            || cursor_ttl_millis == 0
        {
            return Err(CatalogValidationError::InvalidValue {
                field: "pagination contract",
                reason: "page sizes and cursor TTL must be bounded and non-zero",
            });
        }
        Ok(Self {
            default_page_size,
            maximum_page_size,
            cursor_ttl_millis,
        })
    }

    pub const fn default_page_size(&self) -> u32 {
        self.default_page_size
    }

    pub const fn maximum_page_size(&self) -> u32 {
        self.maximum_page_size
    }

    pub const fn cursor_ttl_millis(&self) -> u64 {
        self.cursor_ttl_millis
    }
}

/// Whether an operation requires an application-level idempotency key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyContract {
    NotRequired,
    Required,
}

/// Whether an effect has a shipped, catalog-addressable inverse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum InverseContract {
    NotApplicable,
    Unavailable { reason: InverseUnavailableReason },
    Capability { capability_id: CapabilityId },
}

/// Why an effect cannot advertise a callable inverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InverseUnavailableReason {
    NoShippedInverse,
    ExternalAuthority,
}

/// An authority or state boundary that is rechecked immediately before work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevalidationPoint {
    Authority,
    Scope,
    Policy,
    Configuration,
    ExpectedState,
}

/// Revalidation requirements for the application handler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum RevalidationContract {
    NotRequired,
    Required { checks: Vec<RevalidationPoint> },
}

impl RevalidationContract {
    pub fn required(mut checks: Vec<RevalidationPoint>) -> Result<Self, CatalogValidationError> {
        if checks.is_empty() {
            return Err(CatalogValidationError::MissingValue {
                field: "revalidation checks",
            });
        }
        canonicalize_set(&mut checks, "revalidation checks")?;
        Ok(Self::Required { checks })
    }

    pub fn checks(&self) -> &[RevalidationPoint] {
        match self {
            Self::NotRequired => &[],
            Self::Required { checks } => checks,
        }
    }
}

/// Whether an admitted effect must publish a reconciliation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationContract {
    NotRequired,
    Required,
}

/// Receipt strength required for a capability result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptContract {
    Operation,
    DurableEffect,
}

/// Stable terminal state surfaced after an operation is admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    EffectUnknown,
    Partial,
}

/// The exhaustive terminal-state set a manifest promises to preserve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalStateContract {
    states: Vec<TerminalState>,
}

impl TerminalStateContract {
    pub fn new(mut states: Vec<TerminalState>) -> Result<Self, CatalogValidationError> {
        if states.is_empty() {
            return Err(CatalogValidationError::MissingValue {
                field: "terminal states",
            });
        }
        canonicalize_set(&mut states, "terminal states")?;
        Ok(Self { states })
    }

    pub fn states(&self) -> &[TerminalState] {
        &self.states
    }

    pub fn contains(&self, state: TerminalState) -> bool {
        self.states.binary_search(&state).is_ok()
    }
}

/// Availability metadata used for policy and profile filtering.
///
/// The current wire contract distinguishes callable capabilities from
/// capabilities that have not been implemented.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum AvailabilityContract {
    Available,
    Unavailable { reason: UnavailabilityReason },
}

impl AvailabilityContract {
    pub const fn is_callable(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Safe reason an inert catalog entry is intentionally unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailabilityReason {
    /// The capability has no shipped implementation behind it.
    NotImplemented,
    /// The capability is implemented and reachable, but only through another
    /// callable capability that owns its transport surface. The entry is
    /// retained so a direct route resolves to a typed unavailable decision
    /// instead of an unknown-capability rejection.
    ReachedThroughAnotherCapability,
}

/// Versioned agent-routing metadata. This is description data only and never
/// selects, invokes, or substitutes an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RoutingContractV1 {
    revision: u32,
    name: String,
    description: String,
    examples: Vec<String>,
}

impl RoutingContractV1 {
    pub fn new(
        revision: u32,
        name: impl Into<String>,
        description: impl Into<String>,
        examples: Vec<String>,
    ) -> Result<Self, CatalogValidationError> {
        let name = name.into();
        let description = description.into();
        validate_routing_text(&name, "routing name")?;
        validate_routing_text(&description, "routing description")?;
        if revision == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "routing revision",
                reason: "must be greater than zero",
            });
        }
        if examples.len() > 8 {
            return Err(CatalogValidationError::InvalidValue {
                field: "routing examples",
                reason: "must contain at most eight examples",
            });
        }
        for example in &examples {
            validate_routing_text(example, "routing example")?;
        }

        Ok(Self {
            revision,
            name,
            description,
            examples,
        })
    }

    pub const fn revision(&self) -> u32 {
        self.revision
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn examples(&self) -> &[String] {
        &self.examples
    }

    pub fn estimated_routing_tokens(&self) -> u32 {
        let total_bytes = self.name.len()
            + self.description.len()
            + self.examples.iter().map(String::len).sum::<usize>();
        total_bytes.div_ceil(4) as u32
    }
}

/// Input used to create an immutable [`CapabilityManifestV1`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityManifestInputV1 {
    pub capability_id: CapabilityId,
    pub use_case_id: UseCaseId,
    pub routing: RoutingContractV1,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
    pub effect: EffectClass,
    pub scope: ScopeRequirement,
    pub authority: AuthorityRequirement,
    pub denied_disclosure: DeniedDisclosurePolicy,
    pub privacy: PrivacyClass,
    pub lifecycle: LifecycleClass,
    pub streaming: StreamingContract,
    pub cancellation: CancellationContract,
    pub deadline: DeadlineContract,
    pub pagination: Option<PaginationContract>,
    pub idempotency: IdempotencyContract,
    pub inverse: InverseContract,
    pub authority_revalidation: RevalidationContract,
    pub reconciliation: ReconciliationContract,
    pub receipt: ReceiptContract,
    pub terminal_states: TerminalStateContract,
    pub availability: AvailabilityContract,
    pub binding_ids: Vec<BindingId>,
    pub profile_eligibility: Vec<ProfileId>,
    pub required_features: Vec<FeatureId>,
}

/// Immutable capability metadata consumed by policy, composition, and future
/// adapters. It has no handler, dispatch, transport, or persistence behavior.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CapabilityManifestV1 {
    capability_id: CapabilityId,
    use_case_id: UseCaseId,
    routing: RoutingContractV1,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
    effect: EffectClass,
    scope: ScopeRequirement,
    authority: AuthorityRequirement,
    denied_disclosure: DeniedDisclosurePolicy,
    privacy: PrivacyClass,
    lifecycle: LifecycleClass,
    streaming: StreamingContract,
    cancellation: CancellationContract,
    deadline: DeadlineContract,
    pagination: Option<PaginationContract>,
    idempotency: IdempotencyContract,
    inverse: InverseContract,
    authority_revalidation: RevalidationContract,
    reconciliation: ReconciliationContract,
    receipt: ReceiptContract,
    terminal_states: TerminalStateContract,
    availability: AvailabilityContract,
    binding_ids: Vec<BindingId>,
    profile_eligibility: Vec<ProfileId>,
    required_features: Vec<FeatureId>,
}

impl CapabilityManifestV1 {
    pub fn new(input: CapabilityManifestInputV1) -> Result<Self, CatalogValidationError> {
        let mut binding_ids = input.binding_ids;
        let mut profile_eligibility = input.profile_eligibility;
        let mut required_features = input.required_features;
        canonicalize_set(&mut binding_ids, "manifest binding IDs")?;
        canonicalize_set(&mut profile_eligibility, "manifest profile eligibility")?;
        canonicalize_set(&mut required_features, "manifest required features")?;

        let manifest = Self {
            capability_id: input.capability_id,
            use_case_id: input.use_case_id,
            routing: input.routing,
            request_schema: input.request_schema,
            result_schema: input.result_schema,
            effect: input.effect,
            scope: input.scope,
            authority: input.authority,
            denied_disclosure: input.denied_disclosure,
            privacy: input.privacy,
            lifecycle: input.lifecycle,
            streaming: input.streaming,
            cancellation: input.cancellation,
            deadline: input.deadline,
            pagination: input.pagination,
            idempotency: input.idempotency,
            inverse: input.inverse,
            authority_revalidation: input.authority_revalidation,
            reconciliation: input.reconciliation,
            receipt: input.receipt,
            terminal_states: input.terminal_states,
            availability: input.availability,
            binding_ids,
            profile_eligibility,
            required_features,
        };
        manifest.validate_intrinsic()?;
        Ok(manifest)
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub fn use_case_id(&self) -> &UseCaseId {
        &self.use_case_id
    }

    pub fn routing(&self) -> &RoutingContractV1 {
        &self.routing
    }

    pub fn request_schema(&self) -> &SchemaRef {
        &self.request_schema
    }

    pub fn result_schema(&self) -> &SchemaRef {
        &self.result_schema
    }

    pub const fn effect(&self) -> EffectClass {
        self.effect
    }

    pub fn scope(&self) -> &ScopeRequirement {
        &self.scope
    }

    pub const fn authority(&self) -> AuthorityRequirement {
        self.authority
    }

    pub const fn denied_disclosure(&self) -> DeniedDisclosurePolicy {
        self.denied_disclosure
    }

    pub const fn privacy(&self) -> PrivacyClass {
        self.privacy
    }

    pub const fn lifecycle(&self) -> LifecycleClass {
        self.lifecycle
    }

    pub fn streaming(&self) -> &StreamingContract {
        &self.streaming
    }

    pub fn cancellation(&self) -> &CancellationContract {
        &self.cancellation
    }

    pub fn deadline(&self) -> &DeadlineContract {
        &self.deadline
    }

    pub fn pagination(&self) -> Option<&PaginationContract> {
        self.pagination.as_ref()
    }

    pub const fn idempotency(&self) -> IdempotencyContract {
        self.idempotency
    }

    pub fn inverse(&self) -> &InverseContract {
        &self.inverse
    }

    pub fn authority_revalidation(&self) -> &RevalidationContract {
        &self.authority_revalidation
    }

    pub const fn reconciliation(&self) -> ReconciliationContract {
        self.reconciliation
    }

    pub const fn receipt(&self) -> ReceiptContract {
        self.receipt
    }

    pub fn terminal_states(&self) -> &TerminalStateContract {
        &self.terminal_states
    }

    pub fn availability(&self) -> &AvailabilityContract {
        &self.availability
    }

    pub fn binding_ids(&self) -> &[BindingId] {
        &self.binding_ids
    }

    pub fn profile_eligibility(&self) -> &[ProfileId] {
        &self.profile_eligibility
    }

    pub fn required_features(&self) -> &[FeatureId] {
        &self.required_features
    }

    pub fn schema_refs(&self) -> [&SchemaRef; 2] {
        [&self.request_schema, &self.result_schema]
    }

    pub(crate) fn validate_intrinsic(&self) -> Result<(), CatalogValidationError> {
        if self.scope.requires(ScopeDimension::Resource)
            && self.denied_disclosure != DeniedDisclosurePolicy::Indistinguishable
        {
            return Err(
                self.invalid("resource-addressed capabilities require indistinguishable denial")
            );
        }
        if self.privacy == PrivacyClass::Administrative
            && self.authority != AuthorityRequirement::CapabilityGrantWithRevalidation
        {
            return Err(
                self.invalid("administrative capabilities require revalidating grant authority")
            );
        }
        if self.effect.is_effect()
            && (self.scope.is_empty()
                || self.authority != AuthorityRequirement::CapabilityGrantWithRevalidation)
        {
            return Err(
                self.invalid("effects require explicit scope and revalidating grant authority")
            );
        }

        if self.effect.is_read_only() && self.inverse != InverseContract::NotApplicable {
            return Err(self.invalid("read-only capabilities cannot advertise an inverse"));
        }
        if self.effect.is_effect() && self.inverse == InverseContract::NotApplicable {
            return Err(self.invalid("effects must declare inverse availability"));
        }

        let base_terminals = [
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ];
        if base_terminals
            .iter()
            .any(|state| !self.terminal_states.contains(*state))
        {
            return Err(self.invalid("terminal states must preserve completed, cancelled, timed out, failed, and partial"));
        }

        if self.effect.is_effect() {
            if self.receipt != ReceiptContract::DurableEffect
                || self.idempotency != IdempotencyContract::Required
                || self.reconciliation != ReconciliationContract::Required
                || !matches!(
                    self.authority_revalidation,
                    RevalidationContract::Required { .. }
                )
                || self.deadline.behavior() != DeadlineBehavior::ReturnEffectReceipt
                || !self.terminal_states.contains(TerminalState::EffectUnknown)
                || !self.cancellation.observes(CancellationPoint::BeforeEffect)
                || !self
                    .cancellation
                    .observes(CancellationPoint::EffectInFlight)
            {
                return Err(self.invalid(
                    "effects require durable receipt, idempotency, revalidation, reconciliation, effect deadline behavior, and effect cancellation states",
                ));
            }
        } else if self.receipt != ReceiptContract::Operation
            || self.idempotency != IdempotencyContract::NotRequired
            || self.reconciliation != ReconciliationContract::NotRequired
            || self.terminal_states.contains(TerminalState::EffectUnknown)
        {
            return Err(self.invalid(
                "read and preview capabilities use operation receipts and cannot declare effect-only contracts",
            ));
        }

        if self.effect == EffectClass::Read
            && self.deadline.behavior() == DeadlineBehavior::ReturnEffectReceipt
        {
            return Err(self.invalid("read capabilities cannot return effect deadline receipts"));
        }
        if self.effect == EffectClass::Preview
            && self.deadline.behavior() == DeadlineBehavior::ReturnEffectReceipt
        {
            return Err(self.invalid("preview capabilities cannot return effect deadline receipts"));
        }

        Ok(())
    }

    fn invalid(&self, reason: &'static str) -> CatalogValidationError {
        CatalogValidationError::InvalidCapability {
            capability_id: self.capability_id.clone(),
            reason,
        }
    }
}

fn validate_routing_text(value: &str, field: &'static str) -> Result<(), CatalogValidationError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        return Err(CatalogValidationError::InvalidValue {
            field,
            reason: "must be non-empty, trimmed, bounded, and control-character free",
        });
    }
    Ok(())
}

pub(crate) fn canonicalize_set<T: Ord>(
    values: &mut [T],
    field: &'static str,
) -> Result<(), CatalogValidationError> {
    values.sort();
    if values.windows(2).any(|window| window[0] == window[1]) {
        return Err(CatalogValidationError::DuplicateValue { field });
    }
    Ok(())
}
