use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::binding::{BindingSurface, SurfaceBindingV1, SurfaceOperationName};
use crate::executable::ExecutableSchemaAuthority;
use crate::id::{
    BindingId, CapabilityId, CatalogDigest, ContributionId, FeatureId, ProfileId, SchemaId,
    UseCaseId,
};
use crate::manifest::{CapabilityManifestV1, SchemaRef, ScopeDimension, canonicalize_set};
use crate::profile::ProfileDefinition;
use crate::retrieval::RetrievalPrimitiveManifestV1;
use crate::validation::{CatalogValidationError, validate_catalog};

/// Validation-only evidence that an owning application use case exists.
///
/// This descriptor intentionally cannot invoke anything: it contains no
/// function pointer, trait object, service locator, or runtime registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationHandlerDescriptorV1 {
    capability_id: CapabilityId,
    use_case_id: UseCaseId,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
}

impl ApplicationHandlerDescriptorV1 {
    pub fn new(
        capability_id: CapabilityId,
        use_case_id: UseCaseId,
        request_schema: SchemaRef,
        result_schema: SchemaRef,
    ) -> Self {
        Self {
            capability_id,
            use_case_id,
            request_schema,
            result_schema,
        }
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub fn use_case_id(&self) -> &UseCaseId {
        &self.use_case_id
    }

    pub fn request_schema(&self) -> &SchemaRef {
        &self.request_schema
    }

    pub fn result_schema(&self) -> &SchemaRef {
        &self.result_schema
    }
}

/// Input used to create an inert application-owned catalog contribution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogContributionInputV1 {
    pub contribution_id: ContributionId,
    pub depends_on: Vec<ContributionId>,
    pub capabilities: Vec<CapabilityManifestV1>,
    pub retrieval_primitives: Vec<RetrievalPrimitiveManifestV1>,
    pub bindings: Vec<SurfaceBindingV1>,
}

/// A reviewed, application-owned set of inert catalog records.
///
/// Contributions carry metadata only. The root composition layer validates and
/// folds them into a snapshot; neither contributions nor this crate dispatch
/// to a handler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogContributionV1 {
    contribution_id: ContributionId,
    depends_on: Vec<ContributionId>,
    capabilities: Vec<CapabilityManifestV1>,
    retrieval_primitives: Vec<RetrievalPrimitiveManifestV1>,
    bindings: Vec<SurfaceBindingV1>,
    executable_schemas: Vec<ExecutableSchemaAuthority>,
}

impl CatalogContributionV1 {
    pub fn new(input: CatalogContributionInputV1) -> Result<Self, CatalogValidationError> {
        let mut depends_on = input.depends_on;
        let mut capabilities = input.capabilities;
        let mut retrieval_primitives = input.retrieval_primitives;
        let mut bindings = input.bindings;
        canonicalize_set(&mut depends_on, "contribution dependencies")?;
        capabilities.sort_by(|left, right| left.capability_id().cmp(right.capability_id()));
        retrieval_primitives.sort_by(|left, right| {
            left.capability_id()
                .cmp(right.capability_id())
                .then_with(|| left.retriever_id().cmp(right.retriever_id()))
        });
        bindings.sort_by(|left, right| left.binding_id().cmp(right.binding_id()));

        Ok(Self {
            contribution_id: input.contribution_id,
            depends_on,
            capabilities,
            retrieval_primitives,
            bindings,
            executable_schemas: Vec::new(),
        })
    }

    /// Attach SDK-grade schema bodies supplied by the same application module
    /// that owns the capability manifest and wire types.
    pub fn with_executable_schemas(
        mut self,
        mut executable_schemas: Vec<ExecutableSchemaAuthority>,
    ) -> Result<Self, CatalogValidationError> {
        executable_schemas.sort_by(|left, right| left.capability_id().cmp(right.capability_id()));
        for pair in executable_schemas.windows(2) {
            if pair[0].capability_id() == pair[1].capability_id() {
                return Err(CatalogValidationError::DuplicateValue {
                    field: "contribution executable schema capability IDs",
                });
            }
        }
        for authority in &executable_schemas {
            let manifest = self
                .capabilities
                .binary_search_by(|manifest| {
                    manifest.capability_id().cmp(authority.capability_id())
                })
                .ok()
                .map(|index| &self.capabilities[index])
                .ok_or_else(|| CatalogValidationError::InvalidCapability {
                    capability_id: authority.capability_id().clone(),
                    reason: "executable schema authority has no owning manifest",
                })?;
            if authority.request_schema().schema_ref() != manifest.request_schema()
                || authority.result_schema().schema_ref() != manifest.result_schema()
            {
                return Err(CatalogValidationError::InvalidCapability {
                    capability_id: authority.capability_id().clone(),
                    reason: "executable schema authority does not match the manifest",
                });
            }
        }
        self.executable_schemas = executable_schemas;
        Ok(self)
    }

    pub fn contribution_id(&self) -> &ContributionId {
        &self.contribution_id
    }

    pub fn depends_on(&self) -> &[ContributionId] {
        &self.depends_on
    }

    pub fn capabilities(&self) -> &[CapabilityManifestV1] {
        &self.capabilities
    }

    pub fn retrieval_primitives(&self) -> &[RetrievalPrimitiveManifestV1] {
        &self.retrieval_primitives
    }

    pub fn bindings(&self) -> &[SurfaceBindingV1] {
        &self.bindings
    }

    pub fn executable_schemas(&self) -> &[ExecutableSchemaAuthority] {
        &self.executable_schemas
    }

    pub fn executable_schema(
        &self,
        capability_id: &CapabilityId,
    ) -> Option<&ExecutableSchemaAuthority> {
        self.executable_schemas
            .binary_search_by(|authority| authority.capability_id().cmp(capability_id))
            .ok()
            .map(|index| &self.executable_schemas[index])
    }
}

/// Mutable assembly input that is consumed to create one immutable snapshot.
#[derive(Clone, Debug, Default)]
pub struct CatalogSnapshotBuilderV1 {
    contributions: Vec<CatalogContributionV1>,
    profiles: Vec<ProfileDefinition>,
    handlers: Vec<ApplicationHandlerDescriptorV1>,
}

impl CatalogSnapshotBuilderV1 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_contribution(&mut self, contribution: CatalogContributionV1) -> &mut Self {
        self.contributions.push(contribution);
        self
    }

    pub fn add_profile(&mut self, profile: ProfileDefinition) -> &mut Self {
        self.profiles.push(profile);
        self
    }

    pub fn add_handler(&mut self, handler: ApplicationHandlerDescriptorV1) -> &mut Self {
        self.handlers.push(handler);
        self
    }

    pub fn build(self) -> Result<CatalogSnapshotV1, CatalogValidationError> {
        validate_catalog(&self.contributions, &self.profiles, &self.handlers)?;

        let mut capabilities = BTreeMap::new();
        let mut retrieval_primitives = BTreeMap::new();
        let mut bindings = BTreeMap::new();
        let mut executable_schemas = BTreeMap::new();
        for contribution in &self.contributions {
            for capability in contribution.capabilities() {
                capabilities.insert(capability.capability_id().clone(), capability.clone());
            }
            for retrieval in contribution.retrieval_primitives() {
                retrieval_primitives.insert(retrieval.capability_id().clone(), retrieval.clone());
            }
            for binding in contribution.bindings() {
                bindings.insert(binding.binding_id().clone(), binding.clone());
            }
            for authority in contribution.executable_schemas() {
                executable_schemas.insert(authority.capability_id().clone(), authority.clone());
            }
        }

        let profiles: BTreeMap<_, _> = self
            .profiles
            .into_iter()
            .map(|profile| (profile.profile_id().clone(), profile))
            .collect();
        let binding_lookup: BTreeMap<_, _> = bindings
            .iter()
            .map(|(binding_id, binding)| {
                (
                    (binding.surface(), binding.operation().clone()),
                    binding_id.clone(),
                )
            })
            .collect();
        let schema_index = collect_schema_index(&capabilities, &retrieval_primitives);
        let digest = calculate_digest(
            &self.contributions,
            &capabilities,
            &retrieval_primitives,
            &bindings,
            &executable_schemas,
            &profiles,
        );

        Ok(CatalogSnapshotV1 {
            digest,
            capabilities,
            retrieval_primitives,
            bindings,
            executable_schemas,
            profiles,
            schema_index,
            binding_lookup,
        })
    }
}

/// Versioned immutable catalog state used for discovery and validation-only
/// lookup. It does not retain handlers or provide invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSnapshotV1 {
    digest: CatalogDigest,
    capabilities: BTreeMap<CapabilityId, CapabilityManifestV1>,
    retrieval_primitives: BTreeMap<CapabilityId, RetrievalPrimitiveManifestV1>,
    bindings: BTreeMap<BindingId, SurfaceBindingV1>,
    executable_schemas: BTreeMap<CapabilityId, ExecutableSchemaAuthority>,
    profiles: BTreeMap<ProfileId, ProfileDefinition>,
    schema_index: BTreeMap<(SchemaId, u32), SchemaRef>,
    binding_lookup: BTreeMap<(BindingSurface, SurfaceOperationName), BindingId>,
}

impl CatalogSnapshotV1 {
    pub const fn digest(&self) -> CatalogDigest {
        self.digest
    }

    pub fn capability(&self, capability_id: &CapabilityId) -> Option<&CapabilityManifestV1> {
        self.capabilities.get(capability_id)
    }

    pub fn retrieval_primitive(
        &self,
        capability_id: &CapabilityId,
    ) -> Option<&RetrievalPrimitiveManifestV1> {
        self.retrieval_primitives.get(capability_id)
    }

    pub fn binding(&self, binding_id: &BindingId) -> Option<&SurfaceBindingV1> {
        self.bindings.get(binding_id)
    }

    pub fn executable_schema(
        &self,
        capability_id: &CapabilityId,
    ) -> Option<&ExecutableSchemaAuthority> {
        self.executable_schemas.get(capability_id)
    }

    pub fn profile(&self, profile_id: &ProfileId) -> Option<&ProfileDefinition> {
        self.profiles.get(profile_id)
    }

    pub fn schema(&self, schema_id: &SchemaId, revision: u32) -> Option<&SchemaRef> {
        self.schema_index.get(&(schema_id.clone(), revision))
    }

    pub fn capabilities(&self) -> impl Iterator<Item = &CapabilityManifestV1> {
        self.capabilities.values()
    }

    pub fn profiles(&self) -> impl Iterator<Item = &ProfileDefinition> {
        self.profiles.values()
    }

    /// Resolves metadata only. `None` deliberately covers unknown, unavailable,
    /// feature-incompatible, profile-hidden, and protocol-incompatible entries.
    pub fn resolve_binding(
        &self,
        profile_id: &ProfileId,
        surface: BindingSurface,
        operation: &SurfaceOperationName,
        protocol_revision: u32,
        negotiated_features: &BTreeSet<FeatureId>,
    ) -> Option<&CapabilityManifestV1> {
        let profile = self.profiles.get(profile_id)?;
        if !profile.enables_surface(surface) {
            return None;
        }
        let binding_id = self.binding_lookup.get(&(surface, operation.clone()))?;
        let binding = self.bindings.get(binding_id)?;
        if !binding.protocol_revisions().contains(protocol_revision)
            || !features_satisfied(binding.required_features(), negotiated_features)
        {
            return None;
        }
        let capability = self.capabilities.get(binding.capability_id())?;
        if !profile.includes_capability(capability.capability_id())
            || !capability.availability().is_callable()
            || !features_satisfied(capability.required_features(), negotiated_features)
        {
            return None;
        }
        Some(capability)
    }

    /// Lists catalog metadata that is both profile-visible and currently
    /// available, in stable capability-ID order.
    pub fn visible_capabilities(
        &self,
        profile_id: &ProfileId,
        negotiated_features: &BTreeSet<FeatureId>,
    ) -> Vec<&CapabilityManifestV1> {
        let Some(profile) = self.profiles.get(profile_id) else {
            return Vec::new();
        };
        profile
            .capability_ids()
            .iter()
            .filter_map(|capability_id| self.capabilities.get(capability_id))
            .filter(|capability| {
                capability.availability().is_callable()
                    && features_satisfied(capability.required_features(), negotiated_features)
            })
            .collect()
    }

    /// Lists callable bindings after applying every discovery boundary.
    ///
    /// The caller supplies its already-resolved scope and authorization
    /// intersection. This keeps transport adapters from publishing a static
    /// superset and preserves indistinguishable omission for hidden entries.
    #[allow(clippy::too_many_arguments)]
    pub fn visible_bindings<'a>(
        &'a self,
        profile_id: &ProfileId,
        surface: BindingSurface,
        protocol_revision: u32,
        negotiated_features: &BTreeSet<FeatureId>,
        authorized_capabilities: &BTreeSet<CapabilityId>,
        available_scope: &BTreeSet<ScopeDimension>,
    ) -> Vec<(&'a SurfaceBindingV1, &'a CapabilityManifestV1)> {
        let Some(profile) = self.profiles.get(profile_id) else {
            return Vec::new();
        };
        if !profile.enables_surface(surface) {
            return Vec::new();
        }
        let mut visible = Vec::new();
        for capability in self.visible_capabilities(profile_id, negotiated_features) {
            if !authorized_capabilities.contains(capability.capability_id())
                || !capability
                    .scope()
                    .dimensions()
                    .iter()
                    .all(|dimension| available_scope.contains(dimension))
            {
                continue;
            }
            for binding_id in capability.binding_ids() {
                let Some(binding) = self.bindings.get(binding_id) else {
                    continue;
                };
                if binding.surface() == surface
                    && binding.protocol_revisions().contains(protocol_revision)
                    && features_satisfied(binding.required_features(), negotiated_features)
                {
                    visible.push((binding, capability));
                }
            }
        }
        visible.sort_by(|(left, _), (right, _)| {
            left.operation().as_str().cmp(right.operation().as_str())
        });
        visible
    }
}

fn features_satisfied(
    required_features: &[FeatureId],
    negotiated_features: &BTreeSet<FeatureId>,
) -> bool {
    required_features
        .iter()
        .all(|feature| negotiated_features.contains(feature))
}

fn collect_schema_index(
    capabilities: &BTreeMap<CapabilityId, CapabilityManifestV1>,
    retrievals: &BTreeMap<CapabilityId, RetrievalPrimitiveManifestV1>,
) -> BTreeMap<(SchemaId, u32), SchemaRef> {
    let mut schemas = BTreeMap::new();
    for schema in capabilities
        .values()
        .flat_map(CapabilityManifestV1::schema_refs)
        .chain(
            retrievals
                .values()
                .flat_map(RetrievalPrimitiveManifestV1::schema_refs),
        )
    {
        schemas
            .entry((schema.schema_id().clone(), schema.revision()))
            .or_insert_with(|| schema.clone());
    }
    schemas
}

fn calculate_digest(
    contributions: &[CatalogContributionV1],
    capabilities: &BTreeMap<CapabilityId, CapabilityManifestV1>,
    retrieval_primitives: &BTreeMap<CapabilityId, RetrievalPrimitiveManifestV1>,
    bindings: &BTreeMap<BindingId, SurfaceBindingV1>,
    executable_schemas: &BTreeMap<CapabilityId, ExecutableSchemaAuthority>,
    profiles: &BTreeMap<ProfileId, ProfileDefinition>,
) -> CatalogDigest {
    let mut contributions: Vec<_> = contributions.iter().collect();
    contributions.sort_by(|left, right| left.contribution_id().cmp(right.contribution_id()));

    let document = SnapshotDigestDocument {
        revision: 1,
        contributions: contributions
            .into_iter()
            .map(|contribution| ContributionDigestEntry {
                contribution_id: contribution.contribution_id().clone(),
                depends_on: contribution.depends_on().to_vec(),
            })
            .collect(),
        capabilities: capabilities.values().cloned().collect(),
        retrieval_primitives: retrieval_primitives.values().cloned().collect(),
        bindings: bindings.values().cloned().collect(),
        executable_schemas: executable_schemas.values().cloned().collect(),
        profiles: profiles.values().cloned().collect(),
    };
    let document =
        serde_json::to_vec(&document).expect("catalog records serialize without fallible values");
    let mut canonical = b"tracedecay-tool-catalog.snapshot.v1\0".to_vec();
    canonical.extend_from_slice(&document);
    CatalogDigest::sha256(canonical)
}

#[derive(Serialize)]
struct SnapshotDigestDocument {
    revision: u8,
    contributions: Vec<ContributionDigestEntry>,
    capabilities: Vec<CapabilityManifestV1>,
    retrieval_primitives: Vec<RetrievalPrimitiveManifestV1>,
    bindings: Vec<SurfaceBindingV1>,
    executable_schemas: Vec<ExecutableSchemaAuthority>,
    profiles: Vec<ProfileDefinition>,
}

#[derive(Serialize)]
struct ContributionDigestEntry {
    contribution_id: ContributionId,
    depends_on: Vec<ContributionId>,
}
