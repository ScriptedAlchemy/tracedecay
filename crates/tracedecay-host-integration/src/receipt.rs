use serde::{Deserialize, Serialize};
use tracedecay_domain::{HostComponentV1, HostKindV1};

use crate::manifest::{HostBundleLifecycleOpV1, HostBundleManifestV1};

/// Version 2 dropped the rollback boundary and rollback history: receipts are
/// ownership records only, and no operation keeps prior bytes after it ends.
pub const HOST_BUNDLE_RECEIPT_SCHEMA_VERSION: u16 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleReceiptArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
}

/// Durable local receipt. It is a host-install ownership record, not a
/// product/configuration store and contains no artifact content or credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleInstallReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostComponentV1,
    pub operation: HostBundleLifecycleOpV1,
    pub manifest_digest: [u8; 32],
    pub artifacts: Vec<HostBundleReceiptArtifactV1>,
}

/// Durable aggregate commit marker for a complete host component set. The root
/// adapter owns the aggregate transaction; this contract binds its receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostComponentSetReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub operation: HostBundleLifecycleOpV1,
    pub component_manifests: Vec<HostBundleManifestV1>,
    pub component_receipts: Vec<HostBundleInstallReceiptV1>,
    #[serde(default)]
    pub confirmed_plan_digest: Option<[u8; 32]>,
    #[serde(default)]
    pub base_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub current_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub artifact_state_revision: Option<[u8; 32]>,
}
