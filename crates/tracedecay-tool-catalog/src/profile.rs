use serde::Serialize;

use crate::binding::BindingSurface;
use crate::id::{CapabilityId, ProfileId};
use crate::manifest::canonicalize_set;
use crate::validation::CatalogValidationError;

/// Named profile categories. The ceiling for each profile is chosen by the
/// composer that builds it, never inferred from the category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Default,
    Compact,
    Administrative,
    HostLimited,
}

/// Hard discovery/routing limits for one explicit profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileBudget {
    maximum_bindings: u32,
    maximum_routing_tokens: u32,
}

impl ProfileBudget {
    pub fn new(
        maximum_bindings: u32,
        maximum_routing_tokens: u32,
    ) -> Result<Self, CatalogValidationError> {
        if maximum_bindings == 0 || maximum_routing_tokens == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "profile budget",
                reason: "all ceilings must be greater than zero",
            });
        }
        Ok(Self {
            maximum_bindings,
            maximum_routing_tokens,
        })
    }

    pub const fn maximum_bindings(&self) -> u32 {
        self.maximum_bindings
    }

    pub const fn maximum_routing_tokens(&self) -> u32 {
        self.maximum_routing_tokens
    }
}

/// Input used to construct an explicit immutable surface profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileDefinitionInputV1 {
    pub profile_id: ProfileId,
    pub kind: ProfileKind,
    pub capability_ids: Vec<CapabilityId>,
    pub enabled_surfaces: Vec<BindingSurface>,
    pub requires_cli_mcp_pairing: bool,
    pub budget: ProfileBudget,
}

/// Explicit membership and ceilings for one surface/companion profile.
///
/// A profile carries membership, surfaces, and budgets only. Routing quality
/// is exercised by the agent-adoption evals against real hosts, never by
/// utterance fixtures embedded in catalog identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileDefinition {
    profile_id: ProfileId,
    kind: ProfileKind,
    capability_ids: Vec<CapabilityId>,
    enabled_surfaces: Vec<BindingSurface>,
    requires_cli_mcp_pairing: bool,
    budget: ProfileBudget,
}

impl ProfileDefinition {
    pub fn new(input: ProfileDefinitionInputV1) -> Result<Self, CatalogValidationError> {
        let mut capability_ids = input.capability_ids;
        let mut enabled_surfaces = input.enabled_surfaces;
        canonicalize_set(&mut capability_ids, "profile capability IDs")?;
        canonicalize_set(&mut enabled_surfaces, "profile enabled surfaces")?;

        if input.requires_cli_mcp_pairing
            && (!enabled_surfaces.contains(&BindingSurface::Cli)
                || !enabled_surfaces.contains(&BindingSurface::Mcp))
        {
            return Err(CatalogValidationError::InvalidValue {
                field: "paired CLI/MCP profile",
                reason: "must enable both CLI and MCP surfaces",
            });
        }

        Ok(Self {
            profile_id: input.profile_id,
            kind: input.kind,
            capability_ids,
            enabled_surfaces,
            requires_cli_mcp_pairing: input.requires_cli_mcp_pairing,
            budget: input.budget,
        })
    }

    pub fn profile_id(&self) -> &ProfileId {
        &self.profile_id
    }

    pub const fn kind(&self) -> ProfileKind {
        self.kind
    }

    pub fn capability_ids(&self) -> &[CapabilityId] {
        &self.capability_ids
    }

    pub fn enabled_surfaces(&self) -> &[BindingSurface] {
        &self.enabled_surfaces
    }

    pub const fn requires_cli_mcp_pairing(&self) -> bool {
        self.requires_cli_mcp_pairing
    }

    pub const fn budget(&self) -> ProfileBudget {
        self.budget
    }

    pub fn includes_capability(&self, capability_id: &CapabilityId) -> bool {
        self.capability_ids.binary_search(capability_id).is_ok()
    }

    pub fn enables_surface(&self, surface: BindingSurface) -> bool {
        self.enabled_surfaces.binary_search(&surface).is_ok()
    }
}
