//! Typed Observatory read surface shared by MCP discovery and daemon dispatch.
//!
//! The surface exposes only the canonical observability and cost read models.
//! Analytics retains its distinct facts and automation rollups; those private
//! counters, sections, and Markdown rendering are not Observatory DTOs.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, ExecutableSchemaAuthority,
    IdempotencyContract, LifecycleClass, PaginationContract, PrivacyClass, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::{
    ApplicationContractError, ApplicationHandlerDescriptor, ApplicationOperation, CostsReadModelV1,
    ObservatoryReadModelV1, ResultContractRef, current_bindings,
};

pub const OBSERVATORY_READ_OPERATION: &str = "observatory_read";
const CAPABILITY_ID: &str = "capability.application.observatory-read";
const USE_CASE_ID: &str = "use-case.application.observatory-read";
const CONTRIBUTION_ID: &str = "contribution.application.observatory-read";
const DEFAULT_WINDOW_DAYS: u16 = 14;
const MAX_WINDOW_DAYS: u16 = 365;

/// One project-scoped horizon for the canonical Observatory and Costs models.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservatoryReadRequestV1 {
    #[serde(
        default = "default_window_days",
        deserialize_with = "deserialize_window_days"
    )]
    #[schemars(range(min = 1, max = 365))]
    pub window_days: u16,
}

impl Default for ObservatoryReadRequestV1 {
    fn default() -> Self {
        Self {
            window_days: DEFAULT_WINDOW_DAYS,
        }
    }
}

impl ObservatoryReadRequestV1 {
    pub const fn since_seconds(self) -> i64 {
        self.window_days as i64 * 24 * 60 * 60
    }
}

/// Canonical project-scoped observability and costs read models.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservatoryReadResultV1 {
    pub observatory: ObservatoryReadModelV1,
    pub costs: CostsReadModelV1,
}

pub type ObservatoryReadFuture<'a> = Pin<
    Box<dyn Future<Output = Result<ObservatoryReadResultV1, ApplicationContractError>> + Send + 'a>,
>;

/// Daemon-owned access to the registered project observation authorities.
pub trait ObservatoryReadPortV1: Send + Sync {
    fn read<'a>(&'a self, request: ObservatoryReadRequestV1) -> ObservatoryReadFuture<'a>;
}

pub struct ObservatoryReadServiceV1<P> {
    port: P,
}

impl<P> ObservatoryReadServiceV1<P>
where
    P: ObservatoryReadPortV1,
{
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    pub async fn read(
        &self,
        request: ObservatoryReadRequestV1,
    ) -> Result<ObservatoryReadResultV1, ApplicationContractError> {
        self.port.read(request).await
    }
}

pub fn observatory_read_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let capability_id = CapabilityId::new(CAPABILITY_ID)?;
    let (bindings, binding_ids) = current_bindings(
        &capability_id,
        OBSERVATORY_READ_OPERATION,
        [BindingSurface::Mcp],
    )?;
    let manifest = CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(USE_CASE_ID)?,
        routing: RoutingContractV1::new(
            1,
            "Read project Observatory state".to_owned(),
            "Read canonical project-scoped observability and costs models from registered observation authorities."
                .to_owned(),
            vec!["Read this project's canonical Observatory state".to_owned()],
        )?,
        request_schema: observatory_read_request_schema()?,
        result_schema: observatory_read_result_schema()?,
        effect: EffectClass::Read,
        scope: ScopeRequirement::new(vec![ScopeDimension::Project])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ])?,
        deadline: DeadlineContract::new(15_000, DeadlineBehavior::ReturnOperationReceipt)?,
        pagination: None::<PaginationContract>,
        idempotency: IdempotencyContract::NotRequired,
        inverse: tracedecay_tool_catalog::InverseContract::NotApplicable,
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
        ])?,
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Unavailable,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: crate::retrieval::catalog::application_profile_ids(&[
            crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID,
            crate::retrieval::catalog::APPLICATION_ADMINISTRATIVE_PROFILE_ID,
        ])?,
        required_features: Vec::new(),
    })?;
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new(CONTRIBUTION_ID)?,
        depends_on: Vec::new(),
        capabilities: vec![manifest],
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let executable_schema = observatory_read_executable_schema(&contribution)?;
    Ok(contribution.with_executable_schemas(vec![executable_schema])?)
}

pub fn observatory_read_handler_descriptor()
-> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    Ok(ApplicationHandlerDescriptor::new(
        observatory_read_operation()?,
        observatory_read_request_schema()?,
        observatory_read_result_schema()?,
    )?)
}

pub fn observatory_read_operation() -> Result<ApplicationOperation, ApplicationContractError> {
    Ok(ApplicationOperation::new(
        CapabilityId::new(CAPABILITY_ID)?,
        UseCaseId::new(USE_CASE_ID)?,
        ResultContractRef::from_schema(&observatory_read_result_schema()?),
        true,
    ))
}

pub fn observatory_read_request_schema() -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new("schema.application.observatory-read.request")?,
        1,
    )?)
}

pub fn observatory_read_result_schema() -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new("schema.application.observatory-read.result")?,
        1,
    )?)
}

fn observatory_read_executable_schema(
    contribution: &CatalogContributionV1,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError> {
    let capability_id = CapabilityId::new(CAPABILITY_ID)?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == &capability_id)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "observatory read executable capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        ObservatoryReadRequestV1,
        ObservatoryReadResultV1,
    >(
        manifest,
        "tracedecay_application::observatory_surface::ObservatoryReadRequestV1",
        "tracedecay_application::observatory_surface::ObservatoryReadResultV1",
    )?)
}

const fn default_window_days() -> u16 {
    DEFAULT_WINDOW_DAYS
}

fn deserialize_window_days<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let window_days = u16::deserialize(deserializer)?;
    if !(1..=MAX_WINDOW_DAYS).contains(&window_days) {
        return Err(serde::de::Error::custom(
            "window_days must be between 1 and 365",
        ));
    }
    Ok(window_days)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_tool_catalog::BindingSurface;

    use super::{
        OBSERVATORY_READ_OPERATION, ObservatoryReadRequestV1,
        observatory_read_catalog_contribution, observatory_read_handler_descriptor,
    };

    #[test]
    fn observatory_request_defaults_and_rejects_out_of_range_horizons() {
        assert_eq!(
            serde_json::from_value::<ObservatoryReadRequestV1>(json!({}))
                .expect("default request")
                .window_days,
            14
        );
        for value in [0, 366] {
            assert!(
                serde_json::from_value::<ObservatoryReadRequestV1>(json!({
                    "window_days": value
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn observatory_catalog_is_mcp_only_and_matches_its_handler() {
        let contribution = observatory_read_catalog_contribution().expect("catalog contribution");
        let binding = contribution
            .bindings()
            .iter()
            .find(|binding| binding.operation().as_str() == OBSERVATORY_READ_OPERATION)
            .expect("observatory MCP binding");
        assert_eq!(binding.surface(), BindingSurface::Mcp);
        assert_eq!(
            observatory_read_handler_descriptor()
                .expect("handler descriptor")
                .operation()
                .use_case_id(),
            contribution.capabilities()[0].use_case_id()
        );
    }
}
