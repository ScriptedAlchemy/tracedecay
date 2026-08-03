use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
#[cfg(test)]
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract, RevalidationContract,
    RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement,
    StreamingContract, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use crate::{
    ApplicationContractError, ApplicationHandlerDescriptor, ApplicationOperation,
    ResultContractRef, current_bindings_with_slug,
};

const API_MIGRATION_PLAN_DIGEST_DOMAIN_V1: &str = "tracedecay.application.api-migration-plan.v1";
const API_MIGRATION_DEFINITION_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.api-migration-definition.v1";
const API_MIGRATION_FILE_DIGEST_DOMAIN_V1: &str = "tracedecay.api-migration.file.v1";
const API_MIGRATION_SCHEMA_REVISION_V1: u32 = 1;

pub fn api_migration_definition_digest(
    source: &str,
) -> Result<ManifestDigest, ApplicationContractError> {
    Ok(canonical_sha256(&(
        API_MIGRATION_DEFINITION_DIGEST_DOMAIN_V1,
        source,
    ))?)
}

pub fn api_migration_file_digest(source: &str) -> Result<ManifestDigest, ApplicationContractError> {
    Ok(canonical_sha256(&(
        API_MIGRATION_FILE_DIGEST_DOMAIN_V1,
        source,
    ))?)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationSymbolV1 {
    pub node_id: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub old_name: String,
}

impl ApiMigrationSymbolV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_text("api migration symbol node id", &self.node_id)?;
        validate_text("api migration symbol qualified name", &self.qualified_name)?;
        validate_text("api migration symbol kind", &self.kind)?;
        validate_relative_path("api migration symbol file", &self.file)?;
        validate_identifier("api migration symbol old name", &self.old_name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiCompatibilityLifetimeV1 {
    StablePublicContract,
    Temporary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiCompatibilityDispositionV1 {
    pub lifetime: ApiCompatibilityLifetimeV1,
    pub external_consumer: String,
    pub owner: String,
    pub deprecation_policy: String,
    /// Product sunset predicate for temporary compatibility inserts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletion_condition: Option<String>,
}

impl ApiCompatibilityDispositionV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_text(
            "api migration compatibility external consumer",
            &self.external_consumer,
        )?;
        validate_text("api migration compatibility owner", &self.owner)?;
        validate_text(
            "api migration compatibility deprecation policy",
            &self.deprecation_policy,
        )?;
        match self.lifetime {
            ApiCompatibilityLifetimeV1::StablePublicContract
                if self.deletion_condition.is_some() =>
            {
                Err(ApplicationContractError::Inconsistent {
                    field: "stable API compatibility deletion condition",
                })
            }
            ApiCompatibilityLifetimeV1::Temporary => validate_text(
                "temporary API compatibility deletion condition",
                self.deletion_condition.as_deref().unwrap_or_default(),
            ),
            ApiCompatibilityLifetimeV1::StablePublicContract => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiDefinitionInsertionV1 {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApiMigrationOperationRequestV1 {
    PromotePrimary {
        operation_id: String,
        #[serde(default)]
        depends_on: Vec<String>,
        symbol: ApiMigrationSymbolV1,
        expected_definition_digest: ManifestDigest,
        replacement_definition: String,
    },
    ReplaceDefinition {
        operation_id: String,
        #[serde(default)]
        depends_on: Vec<String>,
        symbol: ApiMigrationSymbolV1,
        expected_definition_digest: ManifestDigest,
        replacement_definition: String,
    },
    RenameBoundSymbol {
        operation_id: String,
        #[serde(default)]
        depends_on: Vec<String>,
        symbol: ApiMigrationSymbolV1,
        new_name: String,
    },
    InsertCompatibility {
        operation_id: String,
        #[serde(default)]
        depends_on: Vec<String>,
        anchor: ApiMigrationSymbolV1,
        position: ApiDefinitionInsertionV1,
        definition: String,
        disposition: ApiCompatibilityDispositionV1,
    },
    ReplaceSelectedTerminology {
        operation_id: String,
        #[serde(default)]
        depends_on: Vec<String>,
        enclosing_symbol: ApiMigrationSymbolV1,
        old_term: String,
        new_term: String,
        occurrence_indexes: Vec<u32>,
    },
    AssertStableValue {
        operation_id: String,
        #[serde(default)]
        depends_on: Vec<String>,
        enclosing_symbol: ApiMigrationSymbolV1,
        category: String,
        exact_bytes: String,
        occurrence_indexes: Vec<u32>,
    },
}

impl ApiMigrationOperationRequestV1 {
    pub fn operation_id(&self) -> &str {
        match self {
            Self::PromotePrimary { operation_id, .. }
            | Self::ReplaceDefinition { operation_id, .. }
            | Self::RenameBoundSymbol { operation_id, .. }
            | Self::InsertCompatibility { operation_id, .. }
            | Self::ReplaceSelectedTerminology { operation_id, .. }
            | Self::AssertStableValue { operation_id, .. } => operation_id,
        }
    }

    pub fn depends_on(&self) -> &[String] {
        match self {
            Self::PromotePrimary { depends_on, .. }
            | Self::ReplaceDefinition { depends_on, .. }
            | Self::RenameBoundSymbol { depends_on, .. }
            | Self::InsertCompatibility { depends_on, .. }
            | Self::ReplaceSelectedTerminology { depends_on, .. }
            | Self::AssertStableValue { depends_on, .. } => depends_on,
        }
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_text("api migration operation id", self.operation_id())?;
        let mut dependencies = BTreeSet::new();
        for dependency in self.depends_on() {
            validate_text("api migration operation dependency", dependency)?;
            if dependency == self.operation_id() || !dependencies.insert(dependency) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration operation dependencies",
                });
            }
        }
        match self {
            Self::PromotePrimary {
                symbol,
                expected_definition_digest,
                replacement_definition,
                ..
            }
            | Self::ReplaceDefinition {
                symbol,
                expected_definition_digest,
                replacement_definition,
                ..
            } => {
                symbol.validate()?;
                expected_definition_digest.validate()?;
                validate_text(
                    "api migration replacement definition",
                    replacement_definition,
                )
            }
            Self::RenameBoundSymbol {
                symbol, new_name, ..
            } => {
                symbol.validate()?;
                validate_identifier("api migration new symbol name", new_name)?;
                if symbol.old_name == *new_name {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "api migration changed symbol name",
                    });
                }
                Ok(())
            }
            Self::InsertCompatibility {
                anchor,
                definition,
                disposition,
                ..
            } => {
                anchor.validate()?;
                validate_text("api migration compatibility definition", definition)?;
                disposition.validate()
            }
            Self::ReplaceSelectedTerminology {
                enclosing_symbol,
                old_term,
                new_term,
                occurrence_indexes,
                ..
            } => {
                enclosing_symbol.validate()?;
                validate_identifier("api migration old terminology", old_term)?;
                validate_identifier("api migration new terminology", new_term)?;
                validate_indexes(occurrence_indexes)
            }
            Self::AssertStableValue {
                enclosing_symbol,
                category,
                exact_bytes,
                occurrence_indexes,
                ..
            } => {
                enclosing_symbol.validate()?;
                validate_text("api migration protected value category", category)?;
                validate_text("api migration protected exact bytes", exact_bytes)?;
                validate_indexes(occurrence_indexes)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationPlanRequestV1 {
    pub family_id: String,
    pub operations: Vec<ApiMigrationOperationRequestV1>,
}

impl ApiMigrationPlanRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_text("api migration family id", &self.family_id)?;
        if self.operations.is_empty() {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "api migration operations",
            });
        }
        let mut positions = BTreeMap::new();
        for (index, operation) in self.operations.iter().enumerate() {
            operation.validate()?;
            if positions
                .insert(operation.operation_id().to_owned(), index)
                .is_some()
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration unique operation ids",
                });
            }
        }
        for (index, operation) in self.operations.iter().enumerate() {
            for dependency in operation.depends_on() {
                if positions
                    .get(dependency)
                    .is_none_or(|position| *position >= index)
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "api migration dependency order",
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiMigrationSiteDispositionV1 {
    Changed,
    Unchanged,
    Skipped,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationSiteV1 {
    pub site_id: String,
    pub operation_id: String,
    pub path: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub expected_bytes: String,
    pub replacement_bytes: String,
    pub disposition: ApiMigrationSiteDispositionV1,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_node_id: Option<String>,
}

impl ApiMigrationSiteV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_text("api migration site id", &self.site_id)?;
        validate_text("api migration site operation id", &self.operation_id)?;
        validate_relative_path("api migration site path", &self.path)?;
        if self.start_byte > self.end_byte {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration site span",
            });
        }
        if self.disposition == ApiMigrationSiteDispositionV1::Changed
            && self.expected_bytes == self.replacement_bytes
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration changed site bytes",
            });
        }
        validate_text("api migration site reason", &self.reason)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationFilePlanV1 {
    pub path: String,
    pub expected_digest: ManifestDigest,
    pub predicted_digest: ManifestDigest,
    pub expected_content: String,
    pub intended_content: String,
}

impl ApiMigrationFilePlanV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_relative_path("api migration file path", &self.path)?;
        self.expected_digest.validate()?;
        self.predicted_digest.validate()?;
        if api_migration_file_digest(&self.expected_content)? != self.expected_digest
            || api_migration_file_digest(&self.intended_content)? != self.predicted_digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration file content digest",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationPlanV1 {
    pub family_id: String,
    pub repository_revision: String,
    pub graph_revision: ManifestDigest,
    pub operations: Vec<ApiMigrationOperationRequestV1>,
    pub sites: Vec<ApiMigrationSiteV1>,
    pub files: Vec<ApiMigrationFilePlanV1>,
    pub blocked: bool,
    pub plan_digest: ManifestDigest,
}

impl ApiMigrationPlanV1 {
    pub fn compute_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        Ok(canonical_sha256(&(
            API_MIGRATION_PLAN_DIGEST_DOMAIN_V1,
            &self.family_id,
            &self.repository_revision,
            &self.graph_revision,
            &self.operations,
            &self.sites,
            &self.files,
            self.blocked,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        ApiMigrationPlanRequestV1 {
            family_id: self.family_id.clone(),
            operations: self.operations.clone(),
        }
        .validate()?;
        validate_text(
            "api migration repository revision",
            &self.repository_revision,
        )?;
        self.graph_revision.validate()?;
        if self.files.is_empty() {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "api migration files",
            });
        }
        let mut paths = BTreeSet::new();
        for file in &self.files {
            file.validate()?;
            if !paths.insert(file.path.as_str()) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration unique files",
                });
            }
        }
        let operations = self
            .operations
            .iter()
            .map(|operation| (operation.operation_id(), operation))
            .collect::<BTreeMap<_, _>>();
        let mut site_ids = BTreeSet::new();
        let mut covered_operations = BTreeSet::new();
        for site in &self.sites {
            site.validate()?;
            let Some(operation) = operations.get(site.operation_id.as_str()) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration typed site operation",
                });
            };
            if !site_ids.insert(site.site_id.as_str()) || !paths.contains(site.path.as_str()) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration site identity",
                });
            }
            let Some(file) = self.files.iter().find(|file| file.path == site.path) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration site file",
                });
            };
            let start = usize::try_from(site.start_byte).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "api migration site preimage span",
                }
            })?;
            let end = usize::try_from(site.end_byte).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "api migration site preimage span",
                }
            })?;
            if file.expected_content.get(start..end) != Some(site.expected_bytes.as_str()) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration site preimage bytes",
                });
            }
            covered_operations.insert(site.operation_id.as_str());
            validate_site_for_operation(site, operation)?;
        }
        if covered_operations.len() != operations.len() {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration operation site coverage",
            });
        }
        validate_materialized_files(&self.files, &self.sites)?;
        for operation in &self.operations {
            validate_operation_result(operation, &self.sites, &self.files)?;
        }
        if self.blocked
            != self
                .sites
                .iter()
                .any(|site| site.disposition == ApiMigrationSiteDispositionV1::Blocked)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration blocked state",
            });
        }
        if self.compute_digest()? != self.plan_digest {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration plan digest",
            });
        }
        Ok(())
    }
}

fn validate_materialized_files(
    files: &[ApiMigrationFilePlanV1],
    sites: &[ApiMigrationSiteV1],
) -> Result<(), ApplicationContractError> {
    for file in files {
        let mut intended = file.expected_content.clone();
        let mut edits = sites
            .iter()
            .filter(|site| {
                site.path == file.path && site.disposition == ApiMigrationSiteDispositionV1::Changed
            })
            .collect::<Vec<_>>();
        edits.sort_by(|left, right| {
            right
                .start_byte
                .cmp(&left.start_byte)
                .then(right.end_byte.cmp(&left.end_byte))
        });
        for pair in edits.windows(2) {
            let right = pair[0];
            let left = pair[1];
            if right.start_byte < left.end_byte
                || (right.start_byte == right.end_byte
                    && left.start_byte == left.end_byte
                    && right.start_byte == left.start_byte)
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration non-overlapping changed sites",
                });
            }
        }
        for edit in edits {
            let start = usize::try_from(edit.start_byte).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "api migration changed site span",
                }
            })?;
            let end = usize::try_from(edit.end_byte).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "api migration changed site span",
                }
            })?;
            if intended.get(start..end) != Some(edit.expected_bytes.as_str()) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration changed site preimage",
                });
            }
            intended.replace_range(start..end, &edit.replacement_bytes);
        }
        if intended != file.intended_content {
            return Err(ApplicationContractError::Inconsistent {
                field: "api migration materialized file content",
            });
        }
    }
    Ok(())
}

fn validate_site_for_operation(
    site: &ApiMigrationSiteV1,
    operation: &ApiMigrationOperationRequestV1,
) -> Result<(), ApplicationContractError> {
    match operation {
        ApiMigrationOperationRequestV1::AssertStableValue {
            enclosing_symbol,
            exact_bytes,
            ..
        } => {
            if site.path != enclosing_symbol.file
                || site.expected_bytes != exact_bytes.as_str()
                || site.replacement_bytes != exact_bytes.as_str()
                || !matches!(
                    site.disposition,
                    ApiMigrationSiteDispositionV1::Skipped | ApiMigrationSiteDispositionV1::Blocked
                )
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration protected value site",
                });
            }
        }
        ApiMigrationOperationRequestV1::InsertCompatibility { anchor, .. } => {
            if site.path != anchor.file
                || !matches!(
                    site.disposition,
                    ApiMigrationSiteDispositionV1::Changed
                        | ApiMigrationSiteDispositionV1::Unchanged
                        | ApiMigrationSiteDispositionV1::Blocked
                )
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration compatibility site",
                });
            }
        }
        ApiMigrationOperationRequestV1::PromotePrimary { symbol, .. }
        | ApiMigrationOperationRequestV1::ReplaceDefinition { symbol, .. }
        | ApiMigrationOperationRequestV1::RenameBoundSymbol { symbol, .. } => {
            if site.path != symbol.file && site.caller_node_id.is_none() {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration symbol site",
                });
            }
        }
        ApiMigrationOperationRequestV1::ReplaceSelectedTerminology {
            enclosing_symbol, ..
        } => {
            if site.path != enclosing_symbol.file {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration terminology site",
                });
            }
        }
    }
    Ok(())
}

fn validate_operation_result(
    operation: &ApiMigrationOperationRequestV1,
    sites: &[ApiMigrationSiteV1],
    files: &[ApiMigrationFilePlanV1],
) -> Result<(), ApplicationContractError> {
    let operation_sites = sites
        .iter()
        .filter(|site| site.operation_id == operation.operation_id())
        .collect::<Vec<_>>();
    if operation_sites.is_empty() {
        return Err(ApplicationContractError::Inconsistent {
            field: "api migration operation site coverage",
        });
    }
    let expected_site_count = match operation {
        ApiMigrationOperationRequestV1::PromotePrimary { .. }
        | ApiMigrationOperationRequestV1::ReplaceDefinition { .. }
        | ApiMigrationOperationRequestV1::InsertCompatibility { .. } => Some(1),
        ApiMigrationOperationRequestV1::ReplaceSelectedTerminology {
            occurrence_indexes, ..
        }
        | ApiMigrationOperationRequestV1::AssertStableValue {
            occurrence_indexes, ..
        } => Some(occurrence_indexes.len()),
        ApiMigrationOperationRequestV1::RenameBoundSymbol { .. } => None,
    };
    if expected_site_count.is_some_and(|expected| operation_sites.len() != expected) {
        return Err(ApplicationContractError::Inconsistent {
            field: "api migration operation site cardinality",
        });
    }
    if let ApiMigrationOperationRequestV1::AssertStableValue { exact_bytes, .. } = operation {
        let required_by_path = operation_sites
            .iter()
            .filter(|site| site.disposition != ApiMigrationSiteDispositionV1::Blocked)
            .fold(BTreeMap::<&str, usize>::new(), |mut counts, site| {
                *counts.entry(site.path.as_str()).or_default() += 1;
                counts
            });
        for (path, required) in required_by_path {
            let Some(file) = files.iter().find(|file| file.path == path) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration protected value file",
                });
            };
            if file.intended_content.matches(exact_bytes.as_str()).count() < required {
                return Err(ApplicationContractError::Inconsistent {
                    field: "api migration protected predicted bytes",
                });
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiMigrationApplyResultV1 {
    pub success: bool,
    pub dry_run: bool,
    pub family_id: String,
    pub plan_digest: ManifestDigest,
    pub changed_files: Vec<String>,
    pub changed_sites: usize,
    pub compatibility_sites: usize,
    pub protected_values_verified: usize,
    pub rolled_back: bool,
    pub message: String,
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ApplicationContractError> {
    if value.trim().is_empty() {
        return Err(ApplicationContractError::InvalidIdentifier { field });
    }
    if value.len() > 1_048_576 {
        return Err(ApplicationContractError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ApplicationContractError> {
    validate_text(field, value)?;
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|character| character == '_' || character.is_alphabetic())
        || !chars.all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(ApplicationContractError::Inconsistent { field });
    }
    Ok(())
}

fn validate_relative_path(
    field: &'static str,
    value: &str,
) -> Result<(), ApplicationContractError> {
    validate_text(field, value)?;
    let path = std::path::Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
    {
        return Err(ApplicationContractError::Inconsistent { field });
    }
    Ok(())
}

fn validate_indexes(indexes: &[u32]) -> Result<(), ApplicationContractError> {
    if indexes.is_empty() {
        return Err(ApplicationContractError::InvalidIdentifier {
            field: "api migration selected occurrence indexes",
        });
    }
    let mut unique = BTreeSet::new();
    if indexes.iter().any(|index| !unique.insert(*index)) {
        return Err(ApplicationContractError::Inconsistent {
            field: "api migration selected occurrence indexes",
        });
    }
    Ok(())
}

pub fn api_migration_plan_operation() -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = api_migration_schema("result")?;
    Ok(ApplicationOperation::new(
        CapabilityId::new("capability.application.api-migration.plan")?,
        UseCaseId::new("use-case.application.api-migration.plan")?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub fn api_migration_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    Ok(vec![ApplicationHandlerDescriptor::new(
        api_migration_plan_operation()?,
        api_migration_schema("request")?,
        api_migration_schema("result")?,
    )?])
}

pub fn api_migration_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let operation = api_migration_plan_operation()?;
    let (bindings, binding_ids) = current_bindings_with_slug(
        operation.capability_id(),
        "api_migration_plan",
        "api-migration-plan",
        [BindingSurface::Cli, BindingSurface::Mcp],
    )?;
    let capability = CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: operation.capability_id().clone(),
        use_case_id: operation.use_case_id().clone(),
        routing: RoutingContractV1::new(
            1,
            "Plan a compatibility-aware API migration",
            "Resolve one dependency-ordered API migration family through graph and AST authorities.",
            vec!["Plan this API migration before applying any source edit".to_owned()],
        )?,
        request_schema: api_migration_schema("request")?,
        result_schema: api_migration_schema("result")?,
        effect: EffectClass::Preview,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::Sensitive,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ])?,
        deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnOperationReceipt)?,
        pagination: None,
        idempotency: IdempotencyContract::NotRequired,
        inverse: tracedecay_tool_catalog::InverseContract::NotApplicable,
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?;
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.api-migration")?,
        depends_on: Vec::new(),
        capabilities: vec![capability],
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

fn api_migration_schema(suffix: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!("schema.application.api-migration.{suffix}"))?,
        API_MIGRATION_SCHEMA_REVISION_V1,
    )?)
}

#[cfg(test)]
fn canonical_json_schema_bytes<T: JsonSchema>() -> Result<Vec<u8>, ApplicationContractError> {
    let generator = SchemaSettings::default().for_serialize().into_generator();
    serde_json::to_vec(&generator.into_root_schema_for::<T>()).map_err(|error| {
        ApplicationContractError::Catalog(format!(
            "cannot serialize API migration JSON schema: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(value: &str) -> ManifestDigest {
        api_migration_file_digest(value).unwrap()
    }

    fn symbol(old_name: &str) -> ApiMigrationSymbolV1 {
        ApiMigrationSymbolV1 {
            node_id: format!("node-{old_name}"),
            qualified_name: format!("crate::{old_name}"),
            kind: "function".to_owned(),
            file: "src/lib.rs".to_owned(),
            old_name: old_name.to_owned(),
        }
    }

    fn plan_with(
        operation: ApiMigrationOperationRequestV1,
        site: ApiMigrationSiteV1,
        expected_content: &str,
        intended_content: &str,
    ) -> ApiMigrationPlanV1 {
        let mut plan = ApiMigrationPlanV1 {
            family_id: "family.contract".to_owned(),
            repository_revision: "HEAD".to_owned(),
            graph_revision: digest("graph"),
            operations: vec![operation],
            sites: vec![site],
            files: vec![ApiMigrationFilePlanV1 {
                path: "src/lib.rs".to_owned(),
                expected_digest: digest(expected_content),
                predicted_digest: digest(intended_content),
                expected_content: expected_content.to_owned(),
                intended_content: intended_content.to_owned(),
            }],
            blocked: false,
            plan_digest: digest("placeholder"),
        };
        plan.plan_digest = plan.compute_digest().unwrap();
        plan
    }

    #[test]
    fn temporary_compatibility_requires_deletion_condition() {
        let disposition = ApiCompatibilityDispositionV1 {
            lifetime: ApiCompatibilityLifetimeV1::Temporary,
            external_consumer: "published users".to_owned(),
            owner: "api".to_owned(),
            deprecation_policy: "warn for one release".to_owned(),
            deletion_condition: None,
        };
        assert!(disposition.validate().is_err());

        let with_condition = ApiCompatibilityDispositionV1 {
            deletion_condition: Some("remove after consumers migrate".to_owned()),
            ..disposition.clone()
        };
        with_condition.validate().unwrap();
        let serialized = serde_json::to_value(&with_condition).unwrap();
        assert_eq!(
            serialized["deletion_condition"],
            "remove after consumers migrate"
        );
        assert!(serialized.get("pr19_deletion_condition").is_none());
    }

    #[test]
    fn plan_digest_binds_deletion_condition() {
        let expected = "pub fn current() {}\n";
        let replacement = "pub fn legacy() { current(); }\n";
        let intended = format!("{expected}{replacement}");
        let insert_at = u64::try_from(expected.len()).expect("fixture length fits u64");
        let operation = ApiMigrationOperationRequestV1::InsertCompatibility {
            operation_id: "compatibility".to_owned(),
            depends_on: vec![],
            anchor: symbol("current"),
            position: ApiDefinitionInsertionV1::After,
            definition: "pub fn legacy() { current(); }".to_owned(),
            disposition: ApiCompatibilityDispositionV1 {
                lifetime: ApiCompatibilityLifetimeV1::Temporary,
                external_consumer: "published users".to_owned(),
                owner: "api".to_owned(),
                deprecation_policy: "remove after consumers migrate".to_owned(),
                deletion_condition: Some("consumers no longer call legacy()".to_owned()),
            },
        };
        let plan = plan_with(
            operation,
            ApiMigrationSiteV1 {
                site_id: "site.compatibility".to_owned(),
                operation_id: "compatibility".to_owned(),
                path: "src/lib.rs".to_owned(),
                start_byte: insert_at,
                end_byte: insert_at,
                expected_bytes: String::new(),
                replacement_bytes: replacement.to_owned(),
                disposition: ApiMigrationSiteDispositionV1::Changed,
                reason: "deliberate compatibility definition".to_owned(),
                caller_node_id: None,
            },
            expected,
            &intended,
        );
        plan.validate().unwrap();

        assert_eq!(plan.compute_digest().unwrap(), plan.plan_digest);

        let mut mutated = plan.clone();
        if let ApiMigrationOperationRequestV1::InsertCompatibility { disposition, .. } =
            &mut mutated.operations[0]
        {
            disposition.deletion_condition =
                Some("different sunset predicate changes the digest".to_owned());
        } else {
            panic!("expected InsertCompatibility");
        }
        assert_ne!(mutated.compute_digest().unwrap(), plan.plan_digest);
    }

    #[test]
    fn api_migration_catalog_advertises_canonical_bindings_and_schema_revision() {
        let contribution = api_migration_catalog_contribution().unwrap();
        for binding in contribution.bindings() {
            assert!(
                binding
                    .binding_id()
                    .as_str()
                    .ends_with("api-migration-plan.v1"),
                "unexpected binding {}",
                binding.binding_id().as_str()
            );
        }
        let capability = contribution
            .capabilities()
            .first()
            .expect("api migration capability");
        assert_eq!(
            capability.request_schema().revision(),
            API_MIGRATION_SCHEMA_REVISION_V1
        );
        assert_eq!(
            capability.result_schema().revision(),
            API_MIGRATION_SCHEMA_REVISION_V1
        );
    }

    #[test]
    fn plan_requires_every_site_to_belong_to_its_typed_operation() {
        let operation = ApiMigrationOperationRequestV1::InsertCompatibility {
            operation_id: "compatibility".to_owned(),
            depends_on: vec![],
            anchor: symbol("current"),
            position: ApiDefinitionInsertionV1::After,
            definition: "pub fn legacy() { current(); }".to_owned(),
            disposition: ApiCompatibilityDispositionV1 {
                lifetime: ApiCompatibilityLifetimeV1::StablePublicContract,
                external_consumer: "published users".to_owned(),
                owner: "api".to_owned(),
                deprecation_policy: "retain indefinitely".to_owned(),
                deletion_condition: None,
            },
        };
        let plan = plan_with(
            operation,
            ApiMigrationSiteV1 {
                site_id: "site.compatibility".to_owned(),
                operation_id: "untyped-operation".to_owned(),
                path: "src/lib.rs".to_owned(),
                start_byte: 16,
                end_byte: 16,
                expected_bytes: String::new(),
                replacement_bytes: "\npub fn legacy() { current(); }".to_owned(),
                disposition: ApiMigrationSiteDispositionV1::Changed,
                reason: "deliberate compatibility definition".to_owned(),
                caller_node_id: None,
            },
            "pub fn current() {}\n",
            "pub fn current() {}\n\npub fn legacy() { current(); }",
        );

        assert!(plan.validate().is_err());
    }

    #[test]
    fn protected_value_sites_are_skipped_and_survive_the_predicted_bytes() {
        let operation = ApiMigrationOperationRequestV1::AssertStableValue {
            operation_id: "protect-wire-name".to_owned(),
            depends_on: vec![],
            enclosing_symbol: symbol("wire_name"),
            category: "wire field".to_owned(),
            exact_bytes: "\"stable_name\"".to_owned(),
            occurrence_indexes: vec![0],
        };
        let expected = "pub fn wire_name() -> &'static str { \"stable_name\" }\n";
        let unchanged_site = ApiMigrationSiteV1 {
            site_id: "site.protected".to_owned(),
            operation_id: "protect-wire-name".to_owned(),
            path: "src/lib.rs".to_owned(),
            start_byte: 37,
            end_byte: 50,
            expected_bytes: "\"stable_name\"".to_owned(),
            replacement_bytes: "\"stable_name\"".to_owned(),
            disposition: ApiMigrationSiteDispositionV1::Unchanged,
            reason: "protected wire field remains byte-identical".to_owned(),
            caller_node_id: Some("node-wire_name".to_owned()),
        };
        let unchanged = plan_with(operation.clone(), unchanged_site, expected, expected);
        assert!(
            unchanged.validate().is_err(),
            "protected values must use the explicit skipped disposition"
        );

        let removed = plan_with(
            operation,
            ApiMigrationSiteV1 {
                disposition: ApiMigrationSiteDispositionV1::Skipped,
                ..unchanged.sites[0].clone()
            },
            expected,
            "pub fn wire_name() -> &'static str { \"changed_name\" }\n",
        );
        assert!(
            removed.validate().is_err(),
            "the predicted file must retain every asserted protected byte sequence"
        );
    }

    #[test]
    fn plan_digest_binds_file_preimages_and_dependency_order() {
        let operation = ApiMigrationOperationRequestV1::ReplaceDefinition {
            operation_id: "promote".to_owned(),
            depends_on: vec![],
            symbol: ApiMigrationSymbolV1 {
                node_id: "node-old".to_owned(),
                qualified_name: "crate::old".to_owned(),
                kind: "function".to_owned(),
                file: "src/lib.rs".to_owned(),
                old_name: "old".to_owned(),
            },
            expected_definition_digest: api_migration_definition_digest("fn old() {}").unwrap(),
            replacement_definition: "fn current() {}".to_owned(),
        };
        let expected_content = "fn old() {}\n".to_owned();
        let intended_content = "fn current() {}\n".to_owned();
        let mut plan = ApiMigrationPlanV1 {
            family_id: "family.provider-neutral".to_owned(),
            repository_revision: "HEAD".to_owned(),
            graph_revision: digest("graph"),
            operations: vec![operation],
            sites: vec![ApiMigrationSiteV1 {
                site_id: "site.definition".to_owned(),
                operation_id: "promote".to_owned(),
                path: "src/lib.rs".to_owned(),
                start_byte: 0,
                end_byte: 11,
                expected_bytes: "fn old() {}".to_owned(),
                replacement_bytes: "fn current() {}".to_owned(),
                disposition: ApiMigrationSiteDispositionV1::Changed,
                reason: "whole definition replacement".to_owned(),
                caller_node_id: None,
            }],
            files: vec![ApiMigrationFilePlanV1 {
                path: "src/lib.rs".to_owned(),
                expected_digest: digest(&expected_content),
                predicted_digest: digest(&intended_content),
                expected_content,
                intended_content,
            }],
            blocked: false,
            plan_digest: digest("placeholder"),
        };
        plan.plan_digest = plan.compute_digest().unwrap();
        plan.validate().unwrap();
        plan.files[0].intended_content.push(' ');
        assert!(plan.validate().is_err());
    }

    #[test]
    fn api_migration_types_generate_minified_schema_with_shared_definitions() {
        let request_bytes =
            canonical_json_schema_bytes::<ApiMigrationPlanRequestV1>().expect("request schema");
        let request_schema: serde_json::Value =
            serde_json::from_slice(&request_bytes).expect("request schema JSON");
        assert!(
            request_schema["$defs"]
                .as_object()
                .is_some_and(|definitions| definitions.contains_key("ApiMigrationSymbolV1"))
        );
        assert!(
            serde_json::to_vec_pretty(&request_schema)
                .expect("pretty request schema")
                .len()
                > request_bytes.len()
        );
    }
}
