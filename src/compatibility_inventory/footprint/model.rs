use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const FOOTPRINT_SCHEMA: &str = "tracedecay.v2.compatibility-footprint.v1";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedFootprintDescriptors {
    #[serde(default)]
    pub public_items: Vec<NamedCount>,
    #[serde(default)]
    pub extension_points: Vec<ExtensionPointDescriptor>,
    #[serde(default)]
    pub duplicate_clusters: Vec<DuplicateClusterDescriptor>,
    #[serde(default)]
    pub generated_views: Vec<GeneratedViewDescriptor>,
    #[serde(default)]
    pub storage: Vec<StorageFootprint>,
    #[serde(default)]
    pub runtime: Vec<RuntimeFootprint>,
    #[serde(default)]
    pub negative_code: Vec<NegativeCodeDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NamedCount {
    pub owner: String,
    pub count: u64,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtensionPointDescriptor {
    pub id: String,
    pub owner: String,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DuplicateClusterDescriptor {
    pub id: String,
    pub owner: String,
    pub members: Vec<String>,
    pub classification: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GeneratedViewDescriptor {
    pub output_ref: String,
    pub expected_digest: String,
    pub actual_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StorageFootprint {
    pub id: String,
    pub owner: String,
    pub file_count: u64,
    pub byte_count: u64,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RuntimeFootprint {
    pub id: String,
    pub owner: String,
    pub binary_ref: String,
    pub binary_bytes: u64,
    pub idle_rss_bytes: u64,
    pub startup_millis: u64,
    pub hot_build_millis: u64,
    pub clean_build_millis: u64,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NegativeCodeDelta {
    pub id: String,
    pub owner: String,
    pub retired_v1_lines: u64,
    pub adapter_lines: u64,
    pub handwritten_v2_lines: u64,
    pub generated_v2_lines: u64,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FootprintSnapshot {
    pub schema: String,
    pub package_count: usize,
    pub rust_package_ceiling: u64,
    pub packages: Vec<PackageFootprint>,
    pub dependency_edges: Vec<DependencyEdge>,
    pub architecture_edges: Vec<ArchitectureEdge>,
    pub public_items: Vec<NamedCount>,
    pub extension_points: Vec<ExtensionPointDescriptor>,
    pub duplicate_clusters: Vec<DuplicateClusterDescriptor>,
    pub semantic_clusters: Vec<SemanticCluster>,
    pub generated_views: Vec<GeneratedViewDescriptor>,
    pub storage: Vec<StorageFootprint>,
    pub runtime: Vec<RuntimeFootprint>,
    pub adapters: Vec<AdapterDeleteBy>,
    pub negative_code: Vec<NegativeCodeDelta>,
    pub convergence_metrics: Vec<ConvergenceMetric>,
    pub budgets: FootprintBudgets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFootprint {
    pub name: String,
    pub manifest_ref: String,
    pub features: Vec<String>,
    pub targets: Vec<TargetFootprint>,
    pub dependency_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TargetFootprint {
    pub name: String,
    pub kinds: Vec<String>,
    pub source_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub package: String,
    pub manifest_ref: String,
    pub dependency: String,
    pub alias: Option<String>,
    pub kind: String,
    pub target: Option<String>,
    pub optional: bool,
    pub default_features: bool,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ArchitectureEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticCluster {
    pub id: String,
    pub owner: String,
    pub disposition: String,
    pub delete_by_pr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterDeleteBy {
    pub id: String,
    pub owner: String,
    pub delete_by_pr: String,
    pub new_callers_forbidden: bool,
    pub policy_forbidden: bool,
    pub required_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvergenceMetric {
    pub metric: String,
    pub detector: String,
    pub target: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FootprintBudgets {
    pub definite_duplicate_body_lines: u64,
    pub default_binary_ratio_max: f64,
    pub idle_rss_ratio_max: f64,
    pub hot_build_ratio_max: f64,
    pub clean_build_ratio_max: f64,
    pub parity_replacement: String,
    pub generated_accounting: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FootprintError {
    #[error("invalid architecture manifest: {0}")]
    Architecture(String),
    #[error("invalid cargo metadata: {0}")]
    CargoMetadata(String),
    #[error("inventory reference must be relative and normalized: {0}")]
    UnsafeReference(String),
    #[error("duplicate footprint descriptor: {0}")]
    DuplicateDescriptor(String),
}

#[derive(Debug, Deserialize)]
pub(super) struct ArchitectureManifest {
    pub(super) package_ceiling: u64,
    #[serde(default)]
    pub(super) generated_views: Vec<String>,
    #[serde(default)]
    pub(super) edges: Vec<ArchitectureEdge>,
    #[serde(default)]
    pub(super) replaced_v1_clusters: Vec<SemanticCluster>,
    #[serde(default)]
    pub(super) adapter_contracts: Vec<AdapterDeleteBy>,
    #[serde(default)]
    pub(super) scorecard: Vec<ConvergenceMetric>,
    pub(super) budgets: FootprintBudgets,
}

#[derive(Debug, Deserialize)]
pub(super) struct CargoMetadata {
    pub(super) packages: Vec<CargoPackage>,
    pub(super) workspace_members: Vec<String>,
    pub(super) workspace_root: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct CargoPackage {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) manifest_path: String,
    #[serde(default)]
    pub(super) dependencies: Vec<CargoDependency>,
    #[serde(default)]
    pub(super) targets: Vec<CargoTarget>,
    #[serde(default)]
    pub(super) features: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CargoDependency {
    pub(super) name: String,
    pub(super) rename: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) target: Option<String>,
    pub(super) optional: bool,
    pub(super) uses_default_features: bool,
    #[serde(default)]
    pub(super) features: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CargoTarget {
    pub(super) name: String,
    #[serde(default)]
    pub(super) kind: Vec<String>,
    pub(super) src_path: String,
}
