use serde::{Deserialize, Serialize};
use tracedecay_domain::HostKindV1;

use crate::manifest::{HostBundleComponentV1, HostBundleLifecycleOpV1, HostBundleManifestV1};

pub const HOST_BUNDLE_RECEIPT_SCHEMA_VERSION: u16 = 1;

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
    pub component: HostBundleComponentV1,
    pub operation: HostBundleLifecycleOpV1,
    pub manifest_digest: [u8; 32],
    pub artifacts: Vec<HostBundleReceiptArtifactV1>,
    pub rollback_boundary: HostBundleRollbackBoundaryV1,
    #[serde(default)]
    pub rollback_history: Vec<[u8; 16]>,
}

/// Durable, content-free inventory for an operator-requested host-component
/// backup. Artifact bytes live in the lifecycle directory; the receipt binds
/// their exact digests and the manifest needed to restore them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleBackupReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub manifest: HostBundleManifestV1,
    pub source_receipt_digest: [u8; 32],
    pub artifacts: Vec<HostBundleBackupArtifactV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleBackupArtifactV1 {
    pub relative_path: String,
    pub artifact_digest: [u8; 32],
    pub ownership_marker: String,
    pub snapshot_name: String,
}

/// Durable proof that a named backup was restored through the rollback-safe
/// lifecycle writer. The embedded install receipt remains the ownership
/// authority for the restored component.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleRestoreReceiptV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub backup_operation_id: [u8; 16],
    pub restored_receipt: HostBundleInstallReceiptV1,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleRollbackBoundaryV1 {
    Pending,
    Passed,
}

/// Serialized single-component recovery state. Root adapters own opening,
/// writing, and recovering this journal; this crate owns its stable schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostBundleJournalStateV1 {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleJournalEntryV1 {
    pub relative_path: String,
    pub backup_name: Option<String>,
    pub backup_created: bool,
    pub wrote_new: bool,
    pub installed_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBundleJournalV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub operation: HostBundleLifecycleOpV1,
    pub manifest_digest: [u8; 32],
    pub state: HostBundleJournalStateV1,
    pub previous_receipt: Option<HostBundleInstallReceiptV1>,
    pub entries: Vec<HostBundleJournalEntryV1>,
}

/// Serialized aggregate recovery state. Its filesystem lifecycle remains a
/// root adapter responsibility, so this is intentionally only data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostComponentSetJournalStateV1 {
    Prepared,
    Staged,
    Applied,
    Verified,
    Committed,
    RolledBack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostComponentSetJournalComponentV1 {
    pub manifest: HostBundleManifestV1,
    pub previous_receipt: Option<HostBundleInstallReceiptV1>,
    pub entries: Vec<HostBundleJournalEntryV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostComponentSetJournalV1 {
    pub schema_version: u16,
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub operation: HostBundleLifecycleOpV1,
    /// Exact operator authority admitted before any lifecycle mutation.
    ///
    /// Recovery must replay this value rather than manufacturing confirmation.
    #[serde(default)]
    pub explicit_confirmation: bool,
    /// Exact Hermes profile binding admitted with the original request.
    #[serde(default)]
    pub hermes_profile_bindings: u8,
    /// Canonical configuration/runtime preview authority, when the operation
    /// was applied through confirmed preview.
    #[serde(default)]
    pub confirmed_plan_digest: Option<[u8; 32]>,
    #[serde(default)]
    pub base_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub current_registration_revision: Option<[u8; 32]>,
    #[serde(default)]
    pub artifact_state_revision: Option<[u8; 32]>,
    pub state: HostComponentSetJournalStateV1,
    pub registration_staged: bool,
    pub registration_applied: bool,
    pub components: Vec<HostComponentSetJournalComponentV1>,
}

impl HostComponentSetJournalV1 {
    /// Whether the recorded phase and the two registration flags describe a
    /// combination a writer can actually produce.
    ///
    /// The writer raises each flag immediately *before* invoking the
    /// registration hook it names and advances `state` only *after* that hook
    /// returns, so every phase implies the flags of the phases behind it:
    ///
    /// - `Prepared` precedes `registration.apply`, so `registration_applied`
    ///   can never be set there.
    /// - `Staged` and later are reached only after `registration.stage` was
    ///   invoked, which requires `registration_staged`.
    /// - `Applied` and later are reached only after `registration.apply` was
    ///   invoked, which requires `registration_applied`.
    /// - `registration_applied` is never raised without `registration_staged`.
    ///
    /// `RolledBack` is deliberately unconstrained: rollback preserves whichever
    /// flags the failed attempt had reached, so every combination is authentic
    /// there. Recovery must therefore not read the flags as proof that a
    /// rolled-back journal needs no compensation - see
    /// [`Self::registration_compensation_required`].
    #[must_use]
    pub fn registration_flags_match_state(&self) -> bool {
        let staged_required = matches!(
            self.state,
            HostComponentSetJournalStateV1::Staged
                | HostComponentSetJournalStateV1::Applied
                | HostComponentSetJournalStateV1::Verified
                | HostComponentSetJournalStateV1::Committed
        );
        let applied_required = matches!(
            self.state,
            HostComponentSetJournalStateV1::Applied
                | HostComponentSetJournalStateV1::Verified
                | HostComponentSetJournalStateV1::Committed
        );
        if self.registration_applied && !self.registration_staged {
            return false;
        }
        if staged_required && !self.registration_staged {
            return false;
        }
        if applied_required && !self.registration_applied {
            return false;
        }
        !(self.state == HostComponentSetJournalStateV1::Prepared && self.registration_applied)
    }

    /// Whether recovery must attempt host-native registration compensation.
    ///
    /// Only a `Prepared` journal proves registration was never entered: its
    /// flags are raised before the hooks they name, so `Prepared` with both
    /// flags clear is the single state where skipping compensation is sound.
    /// Every other phase - including `RolledBack`, whose flags describe the
    /// interrupted attempt rather than the work still outstanding - must
    /// re-attempt rollback. That re-attempt is already load-bearing today,
    /// because a crash between `rollback_component_set` and journal cleanup
    /// replays the same compensation; the registration adapter contract is
    /// idempotent and no-ops when it finds no staged backup.
    #[must_use]
    pub fn registration_compensation_required(&self) -> bool {
        self.registration_staged
            || self.registration_applied
            || self.state != HostComponentSetJournalStateV1::Prepared
    }
}
