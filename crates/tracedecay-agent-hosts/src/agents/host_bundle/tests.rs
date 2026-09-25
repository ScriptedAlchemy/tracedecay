use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_storage_failure;

use super::control::{
    HOST_BUNDLE_CONTROL_DIR, HOST_BUNDLE_LOCK_FILE, component_set_receipt_file,
    expected_ownership_marker, receipt_file, writer_lock_file,
};
use super::doctor::doctor_artifact_state;
use super::planner::plan_artifact_action;
use super::*;

#[derive(Clone)]
struct FirstPartyVerifier([u8; 32]);

impl HostBundleVerificationAdapterV1 for FirstPartyVerifier {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
        manifest.validate_structure()?;
        if manifest.canonical_digest()? == self.0 {
            Ok(())
        } else {
            Err(HostBundleError::CatalogMismatch)
        }
    }
}

fn manifest(host: HostKindV1, bytes: &[u8]) -> HostBundleManifestV1 {
    let identity: [u8; 32] = Sha256::digest(b"first-party.catalog.v1").into();
    HostBundleManifestV1 {
        schema_version: HOST_BUNDLE_SCHEMA_VERSION,
        host,
        component: HostComponentV1::Core,
        integration_manifest_digest: identity,
        catalog_digest: identity,
        configuration_snapshot_id: "first-party.v1".to_string(),
        effective_behavior_digest: identity,
        resolution_provenance_digest: identity,
        protocol_min: 1,
        protocol_max: 1,
        artifacts: vec![HostBundleArtifactV1 {
            relative_path: "plugins/tracedecay.json".to_string(),
            artifact_digest: Sha256::digest(bytes).into(),
            ownership_marker: expected_ownership_marker(host, HostComponentV1::Core),
        }],
    }
}

fn verifier(manifest: &HostBundleManifestV1) -> FirstPartyVerifier {
    FirstPartyVerifier(manifest.canonical_digest().unwrap())
}

fn execution(
    host: HostKindV1,
    operation: HostBundleLifecycleOpV1,
    operation_id: u8,
    confirmed: bool,
) -> HostBundleExecutionRequestV1 {
    HostBundleExecutionRequestV1 {
        lifecycle: HostBundleLifecycleRequestV1 {
            operation,
            expected_host: host,
            expected_component: HostComponentV1::Core,
            explicit_confirmation: confirmed,
            hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
            adopt_receiptless: false,
        },
        operation_id: [operation_id; 16],
    }
}

/// [`execution`] with operator-confirmed receiptless adoption, the
/// `--yes --adopt` shape.
fn adopting_execution(
    host: HostKindV1,
    operation: HostBundleLifecycleOpV1,
    operation_id: u8,
) -> HostBundleExecutionRequestV1 {
    let mut request = execution(host, operation, operation_id, true);
    request.lifecycle.adopt_receiptless = true;
    request
}

fn content(bytes: &[u8]) -> Vec<HostBundleArtifactContentV1> {
    vec![HostBundleArtifactContentV1 {
        relative_path: "plugins/tracedecay.json".to_string(),
        bytes: bytes.to_vec(),
    }]
}

fn component_manifest(
    host: HostKindV1,
    component: HostComponentV1,
    relative_path: &str,
    bytes: &[u8],
) -> HostBundleManifestV1 {
    let identity: [u8; 32] = Sha256::digest(b"first-party.catalog.v1").into();
    HostBundleManifestV1 {
        schema_version: HOST_BUNDLE_SCHEMA_VERSION,
        host,
        component,
        integration_manifest_digest: identity,
        catalog_digest: identity,
        configuration_snapshot_id: "first-party.v1".to_string(),
        effective_behavior_digest: identity,
        resolution_provenance_digest: identity,
        protocol_min: 1,
        protocol_max: 1,
        artifacts: vec![HostBundleArtifactV1 {
            relative_path: relative_path.to_string(),
            artifact_digest: Sha256::digest(bytes).into(),
            ownership_marker: expected_ownership_marker(host, component),
        }],
    }
}

fn component_entry(manifest: HostBundleManifestV1, bytes: &[u8]) -> HostComponentSetEntryV1 {
    HostComponentSetEntryV1 {
        contents: vec![HostBundleArtifactContentV1 {
            relative_path: manifest.artifacts[0].relative_path.clone(),
            bytes: bytes.to_vec(),
        }],
        manifest,
    }
}

fn component_set(host: HostKindV1, core_bytes: &[u8], agent_bytes: &[u8]) -> HostComponentSetV1 {
    HostComponentSetV1 {
        host,
        components: vec![
            component_entry(
                component_manifest(host, HostComponentV1::Core, "plugins/core.json", core_bytes),
                core_bytes,
            ),
            component_entry(
                component_manifest(
                    host,
                    HostComponentV1::Agent,
                    "plugins/agent.json",
                    agent_bytes,
                ),
                agent_bytes,
            ),
        ],
    }
}

fn component_set_request(
    host: HostKindV1,
    operation: HostBundleLifecycleOpV1,
    operation_id: u8,
) -> HostComponentSetExecutionRequestV1 {
    HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation,
            expected_host: host,
            expected_components: vec![HostComponentV1::Core, HostComponentV1::Agent],
            explicit_confirmation: true,
            hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
            explicit_adoption: false,
        },
        operation_id: [operation_id; 16],
    }
}

#[derive(Clone)]
struct ComponentSetVerifier(Vec<[u8; 32]>);

impl ComponentSetVerifier {
    fn from_set(component_set: &HostComponentSetV1) -> Self {
        Self(
            component_set
                .components
                .iter()
                .map(|component| component.manifest.canonical_digest().unwrap())
                .collect(),
        )
    }
}

impl HostBundleVerificationAdapterV1 for ComponentSetVerifier {
    fn verify_manifest(&self, manifest: &HostBundleManifestV1) -> Result<(), HostBundleError> {
        manifest.validate_structure()?;
        self.0
            .contains(&manifest.canonical_digest()?)
            .then_some(())
            .ok_or(HostBundleError::CatalogMismatch)
    }
}

/// The exact failure [`FailingSetRegistration::verify`] injects. Named so
/// assertions can compare the whole error value: `StorageFailure` carries
/// the source site that raised it, so a second `host_bundle_storage_failure!()`
/// written at the assertion would never equal the one raised in the fake.
const FAILING_SET_REGISTRATION_VERIFY: HostBundleError =
    HostBundleError::StorageFailure("test:FailingSetRegistration::verify");

#[derive(Default)]
struct FailingSetRegistration {
    applied: bool,
    rolled_back: bool,
}

struct ArtifactOnlyTestRegistration;

impl HostComponentSetRegistrationV1 for ArtifactOnlyTestRegistration {}

struct RevisionedTestRegistration {
    revision: [u8; 32],
}

impl HostComponentSetRegistrationV1 for RevisionedTestRegistration {
    fn current_revision(
        &self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<[u8; 32], HostBundleError> {
        Ok(self.revision)
    }
}

impl HostComponentSetRegistrationV1 for FailingSetRegistration {
    fn apply(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        self.applied = true;
        Ok(())
    }

    fn verify(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Err(FAILING_SET_REGISTRATION_VERIFY)
    }

    fn rollback(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        self.rolled_back = true;
        Ok(())
    }
}

#[test]
fn component_set_transaction_is_idempotent_and_rolls_back_every_component() {
    let root = tempfile::tempdir().unwrap();
    let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
    let initial_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 21);
    let initial_verifier = ComponentSetVerifier::from_set(&initial);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let mut registration = ArtifactOnlyTestRegistration;
    let first = HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &initial,
            &initial_request,
            &initial_verifier,
            &mut registration,
        )
        .unwrap();
    let repeated = HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &initial,
            &initial_request,
            &initial_verifier,
            &mut registration,
        )
        .unwrap();
    assert_eq!(repeated, first);

    let updated = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v2");
    let update_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 22);
    let updated_verifier = ComponentSetVerifier::from_set(&updated);
    let mut failing_registration = FailingSetRegistration::default();
    let preview = HostComponentSetTransactionV1::new(&mut writer)
        .preview(
            &updated,
            &update_request,
            &updated_verifier,
            &mut failing_registration,
        )
        .unwrap();
    assert_eq!(
        HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
            &updated,
            &update_request,
            &preview,
            &updated_verifier,
            &mut failing_registration,
        ),
        Err(FAILING_SET_REGISTRATION_VERIFY)
    );
    assert!(failing_registration.applied);
    assert!(failing_registration.rolled_back);
    assert_eq!(
        std::fs::read(root.path().join("plugins/core.json")).unwrap(),
        b"core-v1"
    );
    assert_eq!(
        std::fs::read(root.path().join("plugins/agent.json")).unwrap(),
        b"agent-v1"
    );
    assert_eq!(
        writer
            .load_receipt(HostKindV1::OpenCode, HostComponentV1::Core)
            .unwrap()
            .expect("previous core receipt remains published")
            .operation_id,
        [21; 16]
    );
    assert_eq!(
        writer
            .load_receipt(HostKindV1::OpenCode, HostComponentV1::Agent)
            .unwrap()
            .expect("previous agent receipt remains published")
            .operation_id,
        [21; 16]
    );
    let prior_set_receipt = component_set_receipt_file([21; 16]);
    for entry in fs::read_dir(root.path().join(HOST_BUNDLE_CONTROL_DIR)).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            name.starts_with("receipt.")
                || name.starts_with("writer.")
                || name == prior_set_receipt,
            "a rolled-back operation leaves no journal, backup, staging, or aggregate receipt: {name}"
        );
    }
    drop(writer);
    HostBundleWriterV1::open(root.path()).expect("reopen after rollback");
}

/// Rollback puts back every path it can, but never overwrites bytes a second
/// writer placed at a path mid-operation; that path is a typed conflict.
#[test]
fn component_set_rollback_leaves_a_second_writers_bytes_in_place() {
    let root = tempfile::tempdir().unwrap();
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
    HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &initial,
            &component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 41),
            &ComponentSetVerifier::from_set(&initial),
            &mut ArtifactOnlyTestRegistration,
        )
        .unwrap();
    let repaired = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v2");
    let mut registration = SecondWriterRegistration {
        artifact_root: root.path().to_path_buf(),
        relative_path: "plugins/core.json",
        bytes: b"foreign".to_vec(),
        rolled_back: false,
    };

    let error = HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &repaired,
            &component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Repair, 42),
            &ComponentSetVerifier::from_set(&repaired),
            &mut registration,
        )
        .unwrap_err();

    assert!(
        matches!(error, HostBundleError::OwnershipConflict(_)),
        "{error}"
    );
    assert!(registration.rolled_back);
    assert_eq!(
        fs::read(root.path().join("plugins/core.json")).unwrap(),
        b"foreign"
    );
    assert_eq!(
        fs::read(root.path().join("plugins/agent.json")).unwrap(),
        b"agent-v1",
        "the untouched path is still put back"
    );
    assert_eq!(
        writer
            .load_receipt(HostKindV1::OpenCode, HostComponentV1::Core)
            .unwrap()
            .expect("the prior receipt was never replaced")
            .operation_id,
        [41; 16]
    );
}

/// Stand-in for a concurrent second writer. It rewrites one deployed path
/// during `apply`, so artifact verification fails afterwards and rollback
/// has to cope with the foreign bytes.
struct SecondWriterRegistration {
    artifact_root: PathBuf,
    relative_path: &'static str,
    bytes: Vec<u8>,
    rolled_back: bool,
}

impl HostComponentSetRegistrationV1 for SecondWriterRegistration {
    fn apply(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        fs::write(self.artifact_root.join(self.relative_path), &self.bytes)
            .map_err(|_| host_bundle_storage_failure!())
    }

    fn rollback(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        self.rolled_back = true;
        Ok(())
    }
}

fn host_scoped_component_set(host: HostKindV1, slug: &str, tag: &[u8]) -> HostComponentSetV1 {
    let core_path = format!("{slug}/core.json");
    let agent_path = format!("{slug}/agent.json");
    HostComponentSetV1 {
        host,
        components: vec![
            component_entry(
                component_manifest(host, HostComponentV1::Core, &core_path, tag),
                tag,
            ),
            component_entry(
                component_manifest(host, HostComponentV1::Agent, &agent_path, tag),
                tag,
            ),
        ],
    }
}

/// Defect: one `writer.v1.lock` serialized every host. A writer that has
/// already admitted OpenCode must not stop Codex.
#[test]
fn a_host_lock_does_not_exclude_an_unrelated_host() {
    let root = tempfile::tempdir().unwrap();
    let opencode = host_scoped_component_set(HostKindV1::OpenCode, "opencode", b"v1");
    let opencode_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 61);
    let mut holder = HostBundleWriterV1::open(root.path()).unwrap();
    HostComponentSetTransactionV1::new(&mut holder)
        .execute(
            &opencode,
            &opencode_request,
            &ComponentSetVerifier::from_set(&opencode),
            &mut ArtifactOnlyTestRegistration,
        )
        .expect("opencode install admits the host lock");
    assert!(
        root.path()
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(writer_lock_file(HostKindV1::OpenCode))
            .is_file()
    );
    assert!(
        !root
            .path()
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(HOST_BUNDLE_LOCK_FILE)
            .exists(),
        "the retired lifecycle-root lock must not be recreated"
    );

    let codex = host_scoped_component_set(HostKindV1::Codex, "codex", b"v1");
    let codex_request =
        component_set_request(HostKindV1::Codex, HostBundleLifecycleOpV1::Install, 62);
    let mut other = HostBundleWriterV1::open(root.path()).unwrap();
    HostComponentSetTransactionV1::new(&mut other)
        .execute(
            &codex,
            &codex_request,
            &ComponentSetVerifier::from_set(&codex),
            &mut ArtifactOnlyTestRegistration,
        )
        .expect("codex must not wait on opencode's writer lock");
    assert_eq!(
        fs::read(root.path().join("codex/core.json")).unwrap(),
        b"v1"
    );

    let contended = host_scoped_component_set(HostKindV1::OpenCode, "opencode", b"v2");
    let contended_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Repair, 63);
    assert!(
        matches!(
            HostComponentSetTransactionV1::new(&mut other)
                .execute(
                    &contended,
                    &contended_request,
                    &ComponentSetVerifier::from_set(&contended),
                    &mut ArtifactOnlyTestRegistration,
                )
                .err(),
            Some(HostBundleError::HostWriterBusy)
        ),
        "the same host still has exactly one writer"
    );
}

#[test]
fn unchanged_companion_receipt_keeps_original_operation_provenance() {
    let root = tempfile::tempdir().unwrap();
    let initial = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
    let initial_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 81);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let mut registration = ArtifactOnlyTestRegistration;
    HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &initial,
            &initial_request,
            &ComponentSetVerifier::from_set(&initial),
            &mut registration,
        )
        .unwrap();

    let core_only_change = component_set(HostKindV1::OpenCode, b"core-v2", b"agent-v1");
    let update_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 82);
    let receipt = HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &core_only_change,
            &update_request,
            &ComponentSetVerifier::from_set(&core_only_change),
            &mut registration,
        )
        .unwrap();

    let core = receipt
        .component_receipts
        .iter()
        .find(|receipt| receipt.component == HostComponentV1::Core)
        .unwrap();
    let companion = receipt
        .component_receipts
        .iter()
        .find(|receipt| receipt.component == HostComponentV1::Agent)
        .unwrap();
    assert_eq!(core.operation_id, [82; 16]);
    assert_eq!(core.operation, HostBundleLifecycleOpV1::Update);
    assert_eq!(companion.operation_id, [81; 16]);
    assert_eq!(companion.operation, HostBundleLifecycleOpV1::Install);

    // A companion whose manifest changed but whose artifact bytes did not
    // still earns a fresh receipt. The change must keep the set's shared
    // configuration authority (`configuration_snapshot_id`,
    // `integration_manifest_digest`, `catalog_digest`) uniform across
    // components, so bump a per-component manifest field
    // (`effective_behavior_digest`) that shifts only the agent's canonical
    // digest and leaves the core component entirely unchanged.
    let mut metadata_only_change = core_only_change.clone();
    metadata_only_change.components[1]
        .manifest
        .effective_behavior_digest = Sha256::digest(b"first-party.behavior.v2").into();
    let metadata_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Update, 83);
    let receipt = HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &metadata_only_change,
            &metadata_request,
            &ComponentSetVerifier::from_set(&metadata_only_change),
            &mut registration,
        )
        .unwrap();
    let metadata_updated = receipt
        .component_receipts
        .iter()
        .find(|receipt| receipt.component == HostComponentV1::Agent)
        .unwrap();
    assert_eq!(metadata_updated.operation_id, [83; 16]);
    assert_eq!(
        metadata_updated.manifest_digest,
        metadata_only_change.components[1]
            .manifest
            .canonical_digest()
            .unwrap()
    );
}

#[test]
fn component_set_preflights_cross_component_path_conflicts_before_artifact_writes() {
    let root = tempfile::tempdir().unwrap();
    let component_set = HostComponentSetV1 {
        host: HostKindV1::OpenCode,
        components: vec![
            component_entry(
                component_manifest(
                    HostKindV1::OpenCode,
                    HostComponentV1::Core,
                    "plugins/shared.json",
                    b"core",
                ),
                b"core",
            ),
            component_entry(
                component_manifest(
                    HostKindV1::OpenCode,
                    HostComponentV1::Agent,
                    "plugins/shared.json",
                    b"agent",
                ),
                b"agent",
            ),
        ],
    };
    let request = component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 23);
    let verifier = ComponentSetVerifier::from_set(&component_set);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let mut registration = ArtifactOnlyTestRegistration;

    assert_eq!(
        HostComponentSetTransactionV1::new(&mut writer).execute(
            &component_set,
            &request,
            &verifier,
            &mut registration,
        ),
        Err(HostBundleError::InvalidManifest)
    );
    assert!(
        !root.path().join("plugins").exists(),
        "cross-component conflicts are rejected before artifact paths are created"
    );
}

#[test]
fn confirmed_component_set_rejects_stale_registration_revision_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let component_set = component_set(HostKindV1::OpenCode, b"core", b"agent");
    let request = component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 24);
    let verifier = ComponentSetVerifier::from_set(&component_set);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let mut registration = RevisionedTestRegistration { revision: [1; 32] };
    let preview = HostComponentSetTransactionV1::new(&mut writer)
        .preview(&component_set, &request, &verifier, &mut registration)
        .unwrap();
    assert_eq!(preview.operation_id, request.operation_id);
    assert_eq!(preview.base_registration_revision, [1; 32]);
    assert_eq!(preview.current_registration_revision, [1; 32]);
    assert_ne!(preview.plan_digest, [0; 32]);
    let repeated = HostComponentSetTransactionV1::new(&mut writer)
        .preview(&component_set, &request, &verifier, &mut registration)
        .unwrap();
    assert_eq!(repeated, preview);

    registration.revision = [2; 32];
    assert!(matches!(
        HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
            &component_set,
            &request,
            &preview,
            &verifier,
            &mut registration,
        ),
        Err(HostBundleError::StalePreview(_))
    ));
    assert!(!root.path().join("plugins").exists());
}

#[test]
fn confirmed_component_set_rejects_narrowed_plan_identity_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let full = component_set(HostKindV1::OpenCode, b"core", b"agent");
    let full_request =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 25);
    let verifier = ComponentSetVerifier::from_set(&full);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let mut registration = RevisionedTestRegistration { revision: [3; 32] };
    let preview = HostComponentSetTransactionV1::new(&mut writer)
        .preview(&full, &full_request, &verifier, &mut registration)
        .unwrap();
    let narrowed = HostComponentSetV1 {
        host: full.host,
        components: vec![full.components[0].clone()],
    };
    let narrowed_request = HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            expected_components: vec![HostComponentV1::Core],
            ..full_request.lifecycle.clone()
        },
        operation_id: full_request.operation_id,
    };

    assert!(matches!(
        HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
            &narrowed,
            &narrowed_request,
            &preview,
            &verifier,
            &mut registration,
        ),
        Err(HostBundleError::StalePreview(_))
    ));
    assert!(!root.path().join("plugins").exists());
}

#[test]
fn confirmed_component_set_rejects_changed_artifact_state_without_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let component_set = component_set(HostKindV1::OpenCode, b"core", b"agent");
    let request = component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 26);
    let verifier = ComponentSetVerifier::from_set(&component_set);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let mut registration = RevisionedTestRegistration { revision: [4; 32] };
    let preview = HostComponentSetTransactionV1::new(&mut writer)
        .preview(&component_set, &request, &verifier, &mut registration)
        .unwrap();

    std::fs::create_dir_all(root.path().join("plugins")).unwrap();
    std::fs::write(root.path().join("plugins/core.json"), b"external").unwrap();
    // Somebody else owns the bytes on this artifact path, no adoption
    // authority was granted, and the adapter recognizes no provenance in
    // them. That is a standing refusal, not preview staleness: retrying
    // cannot clear it, so it must be reported as the ownership conflict
    // it is.
    assert!(matches!(
        HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
            &component_set,
            &request,
            &preview,
            &verifier,
            &mut registration,
        ),
        Err(HostBundleError::OwnershipConflict(_))
    ));
    assert_eq!(
        std::fs::read(root.path().join("plugins/core.json")).unwrap(),
        b"external"
    );
    assert!(!root.path().join("plugins/agent.json").exists());
}

#[test]
fn feedback_switch_apply_restore_and_aggregate_receipt_share_one_writer() {
    let root = tempfile::tempdir().unwrap();
    let previous = manifest(HostKindV1::KimiCode, b"previous");
    let target = manifest(HostKindV1::KimiCode, b"target");
    let verifier = ComponentSetVerifier(vec![
        previous.canonical_digest().unwrap(),
        target.canonical_digest().unwrap(),
    ]);
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    writer
        .execute(
            &previous,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Install,
                31,
                true,
            ),
            &content(b"previous"),
            &verifier,
        )
        .unwrap();
    let lifecycle = HostBundleLifecycleRuntimeV1::new(verifier.clone(), writer);
    let mut switch = FeedbackPathRollbackSwitchV1::new(lifecycle);
    let apply = switch
        .feedback_rollback_switch_apply(
            &previous,
            &target,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Update,
                32,
                true,
            ),
            &content(b"target"),
            &[],
        )
        .unwrap();
    let mut corrupted_apply = apply.clone();
    corrupted_apply.apply_receipt.operation_id = [0; 16];
    assert_eq!(
        switch.feedback_rollback_switch_restore(
            &corrupted_apply,
            &previous,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Repair,
                33,
                true,
            ),
            &content(b"previous"),
            &[],
        ),
        Err(HostBundleError::ReceiptCorrupted)
    );
    assert_eq!(
        std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
        b"target"
    );
    let restore = switch
        .feedback_rollback_switch_restore(
            &apply,
            &previous,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Repair,
                33,
                true,
            ),
            &content(b"previous"),
            &[],
        )
        .unwrap();
    let mut writer = switch.into_lifecycle().into_storage();
    let aggregate = writer
        .publish_feedback_component_set_receipt(&previous, &restore.restore_receipt)
        .unwrap();
    assert_eq!(aggregate.component_manifests, vec![previous]);
    assert_eq!(
        std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
        b"previous"
    );
}

#[test]
fn corruption_and_external_bundle_paths_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let bundle = manifest(HostKindV1::OpenCode, b"expected");
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    assert_eq!(
        writer.execute(
            &bundle,
            &execution(
                HostKindV1::OpenCode,
                HostBundleLifecycleOpV1::Install,
                1,
                true
            ),
            &content(b"corrupt"),
            &verifier(&bundle),
        ),
        Err(HostBundleError::ArtifactContentMismatch)
    );
    let mut external = bundle.clone();
    external.artifacts[0].relative_path = "/tmp/third-party.json".to_string();
    assert_eq!(
        external.validate_structure(),
        Err(HostBundleError::UnsafeInstallPath)
    );
}

#[test]
fn lifecycle_ops_converge_cataloged_pre_receipt_artifacts_only_with_adoption() {
    let bundle = manifest(HostKindV1::KimiCode, b"expected");
    for (operation, operation_id) in [
        (HostBundleLifecycleOpV1::Repair, 20),
        (HostBundleLifecycleOpV1::Install, 21),
        (HostBundleLifecycleOpV1::Update, 24),
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("plugins")).unwrap();
        std::fs::write(root.path().join("plugins/tracedecay.json"), b"legacy").unwrap();
        let mut writer = HostBundleWriterV1::open(root.path()).unwrap();

        // Without adoption authority the divergent receiptless file is a
        // typed conflict and stays byte-for-byte untouched.
        assert!(matches!(
            writer.execute(
                &bundle,
                &execution(HostKindV1::KimiCode, operation, operation_id, true),
                &content(b"expected"),
                &verifier(&bundle),
            ),
            Err(HostBundleError::OwnershipConflict(_))
        ));
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"legacy"
        );

        // Operator-confirmed adoption converges it and records ownership.
        let receipt = writer
            .execute(
                &bundle,
                &adopting_execution(HostKindV1::KimiCode, operation, operation_id),
                &content(b"expected"),
                &verifier(&bundle),
            )
            .unwrap_or_else(|error| {
                panic!("{operation:?} must adopt a pre-receipt cataloged deploy: {error}")
            });
        assert_eq!(
            std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
            b"expected"
        );
        assert_eq!(receipt.artifacts.len(), 1);
        assert_eq!(
            receipt.artifacts[0].ownership_marker,
            bundle.artifacts[0].ownership_marker
        );
    }
}

/// The documented stock-host hand-over: TraceDecay stages the deploy, the
/// host activates it natively, and the re-run records the staged source.
/// The staged file is receiptless but byte-identical to the catalog, so
/// `Install` adopts it instead of reporting an ownership conflict.
#[test]
fn install_records_a_byte_identical_receiptless_staged_deploy() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("plugins")).unwrap();
    std::fs::write(root.path().join("plugins/tracedecay.json"), b"expected").unwrap();
    let bundle = manifest(HostKindV1::KimiCode, b"expected");
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();

    let receipt = writer
        .execute(
            &bundle,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Install,
                22,
                true,
            ),
            &content(b"expected"),
            &verifier(&bundle),
        )
        .expect("recording the staged source is a legitimate install state");

    assert_eq!(receipt.artifacts.len(), 1);
    assert_eq!(
        std::fs::read(root.path().join("plugins/tracedecay.json")).unwrap(),
        b"expected"
    );
    // The recorded receipt now owns the path: a follow-up uninstall can
    // prove ownership and remove it.
    let uninstall = writer
        .execute(
            &bundle,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Uninstall,
                23,
                true,
            ),
            &content(b"expected"),
            &verifier(&bundle),
        )
        .expect("the adopted install must leave provable ownership behind");
    assert_eq!(uninstall.operation, HostBundleLifecycleOpV1::Uninstall);
    assert!(!root.path().join("plugins/tracedecay.json").exists());
}

#[test]
fn lifecycle_preserves_ownership_receipts_and_rollback_plan() {
    let root = tempfile::tempdir().unwrap();
    let first = manifest(HostKindV1::KimiCode, b"first");
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    let receipt = writer
        .execute(
            &first,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Install,
                2,
                true,
            ),
            &content(b"first"),
            &verifier(&first),
        )
        .unwrap();
    assert_eq!(receipt.host, HostKindV1::KimiCode);
    assert_eq!(receipt.artifacts.len(), 1);
    drop(writer);

    let second = manifest(HostKindV1::KimiCode, b"second");
    let preview = dry_run_host_bundle_lifecycle_at(
        root.path(),
        &second,
        &execution(
            HostKindV1::KimiCode,
            HostBundleLifecycleOpV1::Update,
            3,
            false,
        ),
        &verifier(&second),
        &[],
    )
    .unwrap();
    assert!(preview.confirmation_required);
    assert!(preview.plan.rollback_required);
    assert_eq!(preview.rollback.replaced_relative_paths.len(), 1);

    std::fs::write(root.path().join("plugins/tracedecay.json"), b"foreign").unwrap();
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();
    assert!(matches!(
        writer.execute(
            &second,
            &execution(
                HostKindV1::KimiCode,
                HostBundleLifecycleOpV1::Update,
                4,
                true
            ),
            &content(b"second"),
            &verifier(&second),
        ),
        Err(HostBundleError::OwnershipConflict(_))
    ));
}

fn pre_v2_artifact(
    artifact: &HostBundleArtifactV1,
    observed_bytes: &[u8],
    cataloged_ownership_marker: Option<String>,
) -> ObservedHostArtifactV1 {
    ObservedHostArtifactV1 {
        relative_path: artifact.relative_path.clone(),
        kind: ObservedArtifactKindV1::RegularFile,
        artifact_digest: Some(Sha256::digest(observed_bytes).into()),
        ownership_marker: None,
        owned_artifact_digest: None,
        cataloged_ownership_marker,
    }
}

/// The receiptless-adoption boundary: a cataloged deploy path alone never
/// authorizes taking a file over. Divergent bytes are refused without
/// adoption authority (the operator's explicit `--adopt`), adopted when that
/// authority is present, byte-identical bytes are recorded without authority (the
/// staged hand-over journey), and Uninstall never adopts anything.
#[test]
fn receiptless_adoption_requires_provenance_or_explicit_authority() {
    let bundle = manifest(HostKindV1::KimiCode, b"current");
    let artifact = &bundle.artifacts[0];
    let marker = Some(artifact.ownership_marker.clone());

    for operation in [
        HostBundleLifecycleOpV1::Install,
        HostBundleLifecycleOpV1::Update,
        HostBundleLifecycleOpV1::Repair,
    ] {
        // Custom or unowned bytes parked at the cataloged path: refused
        // without adoption authority, the path proves nothing.
        let refused = plan_artifact_action(
            operation,
            artifact,
            Some(&pre_v2_artifact(artifact, b"pre-v2", marker.clone())),
            false,
        )
        .expect_err("a receiptless divergent file must refuse without adoption authority");
        assert!(
            matches!(refused, HostBundleError::OwnershipConflict(_)),
            "{operation:?}: {refused}"
        );
        assert!(
            refused.to_string().contains("--yes --adopt"),
            "the refusal must name the explicit adoption remedy: {refused}"
        );

        // With adoption authority the stale bytes are replaced.
        assert_eq!(
            plan_artifact_action(
                operation,
                artifact,
                Some(&pre_v2_artifact(artifact, b"pre-v2", marker.clone())),
                true,
            ),
            Ok(HostArtifactActionV1::Replace),
            "{operation:?} must adopt a receiptless cataloged deploy path when authorized"
        );
        // Bytes identical to the staged catalog are the recorded staged
        // deploy: adoptable without any extra authority, as a no-op.
        assert_eq!(
            plan_artifact_action(
                operation,
                artifact,
                Some(&pre_v2_artifact(artifact, b"current", marker.clone())),
                false,
            ),
            Ok(HostArtifactActionV1::Noop)
        );
    }

    // Uninstall stays fail-closed even with explicit adoption authority:
    // it must never delete a file whose ownership it cannot prove.
    assert!(
        matches!(
            plan_artifact_action(
                HostBundleLifecycleOpV1::Uninstall,
                artifact,
                Some(&pre_v2_artifact(artifact, b"pre-v2", marker.clone())),
                true,
            ),
            Err(HostBundleError::OwnershipConflict(_))
        ),
        "Uninstall must not adopt an artifact no receipt records"
    );

    // A deploy path inside TraceDecay's own staging namespace is
    // TraceDecay-staged by construction, so a divergent staging left by
    // another binary version converges without extra authority. Kimi's
    // native activation flow deploys exactly there.
    assert!(
        crate::agents::kimi::KIMI_STAGED_PLUGIN_RELATIVE
            .starts_with(HOST_BUNDLE_STAGE_ROOT_RELATIVE),
        "the Kimi staged source must live inside the shared staging namespace"
    );
    let mut staged_bundle = manifest(HostKindV1::KimiCode, b"current");
    staged_bundle.artifacts[0].relative_path = format!(
        "{}/kimi/tracedecay/.kimi-plugin/plugin.json",
        HOST_BUNDLE_STAGE_ROOT_RELATIVE
    );
    let staged_artifact = &staged_bundle.artifacts[0];
    assert_eq!(
        plan_artifact_action(
            HostBundleLifecycleOpV1::Repair,
            staged_artifact,
            Some(&pre_v2_artifact(
                staged_artifact,
                b"staged-by-previous-binary",
                Some(staged_artifact.ownership_marker.clone()),
            )),
            false,
        ),
        Ok(HostArtifactActionV1::Replace),
        "a divergent first-party staging must converge without explicit adoption"
    );
}

#[test]
fn repair_refuses_a_receiptless_artifact_whose_ownership_marker_does_not_match() {
    let bundle = manifest(HostKindV1::KimiCode, b"current");
    let artifact = &bundle.artifacts[0];
    let foreign = expected_ownership_marker(HostKindV1::Hermes, HostComponentV1::Core);
    assert_ne!(foreign, artifact.ownership_marker);

    // A foreign marker on the same deploy path is still a conflict even
    // with explicit adoption authority, and the refusal names the
    // conflicting deploy path.
    let foreign_conflict = plan_artifact_action(
        HostBundleLifecycleOpV1::Repair,
        artifact,
        Some(&pre_v2_artifact(artifact, b"pre-v2", Some(foreign))),
        true,
    )
    .expect_err("a foreign marker must refuse");
    assert!(matches!(
        foreign_conflict,
        HostBundleError::OwnershipConflict(_)
    ));
    assert!(
        foreign_conflict
            .to_string()
            .contains(&artifact.relative_path),
        "the conflict must name the contested path: {foreign_conflict}"
    );
    assert!(
        foreign_conflict
            .to_string()
            .contains("move or remove the conflicting file"),
        "an uncataloged observation must name a usable recovery: {foreign_conflict}"
    );
    // So is an absent marker: receipt- and orphan-derived observations
    // never carry one, so they can never be adopted.
    assert!(matches!(
        plan_artifact_action(
            HostBundleLifecycleOpV1::Repair,
            artifact,
            Some(&pre_v2_artifact(artifact, b"pre-v2", None)),
            true,
        ),
        Err(HostBundleError::OwnershipConflict(_))
    ));
    // A receipt claiming the path with a foreign marker keeps the original
    // ownership boundary; adoption never applies to receipt-backed state.
    let mut claimed = pre_v2_artifact(artifact, b"pre-v2", Some(artifact.ownership_marker.clone()));
    claimed.ownership_marker = Some(expected_ownership_marker(
        HostKindV1::Kiro,
        HostComponentV1::Core,
    ));
    claimed.owned_artifact_digest = Some(Sha256::digest(b"pre-v2").into());
    let claimed_conflict = plan_artifact_action(
        HostBundleLifecycleOpV1::Repair,
        artifact,
        Some(&claimed),
        true,
    )
    .expect_err("a foreign receipt marker must refuse");
    assert!(matches!(
        claimed_conflict,
        HostBundleError::OwnershipConflict(_)
    ));
    assert!(
        claimed_conflict
            .to_string()
            .contains("uninstall the component named by the recorded marker"),
        "a receipt-backed conflict must name the recorded-owner recovery: {claimed_conflict}"
    );
    assert!(
        !claimed_conflict
            .to_string()
            .contains("re-runs with `--yes --adopt`"),
        "receipt-backed conflicts must not advertise a receiptless-only remedy: {claimed_conflict}"
    );
}

/// Discovery and planning must agree on the ownership boundary: whenever
/// `Repair` refuses a path as contested, the doctor reports an ownership
/// conflict, and whenever `Repair` would converge it, the doctor reports
/// drift or current. A disagreement means the doctor either fails on
/// something `reinstall` fixes, or hides something it cannot.
#[test]
fn doctor_discovery_mirrors_the_repair_ownership_boundary() {
    for host in [HostKindV1::OpenCode, HostKindV1::CursorDesktop] {
        let bundle = manifest(host, b"current");
        let artifact = &bundle.artifacts[0];
        let owned = Some(artifact.ownership_marker.clone());
        let foreign = Some(expected_ownership_marker(
            HostKindV1::Hermes,
            HostComponentV1::Core,
        ));

        for (marker, bytes, expected) in [
            (
                owned.clone(),
                b"current".as_slice(),
                HostBundleComponentDoctorStateV1::Current,
            ),
            (
                owned.clone(),
                b"drifted".as_slice(),
                HostBundleComponentDoctorStateV1::Drifted,
            ),
            (
                foreign.clone(),
                b"drifted".as_slice(),
                HostBundleComponentDoctorStateV1::OwnershipConflict,
            ),
            (
                None,
                b"drifted".as_slice(),
                HostBundleComponentDoctorStateV1::OwnershipConflict,
            ),
        ] {
            let mut observed = pre_v2_artifact(artifact, bytes, None);
            observed.ownership_marker = marker;
            observed.owned_artifact_digest = observed
                .ownership_marker
                .as_ref()
                .map(|_| Sha256::digest(b"current").into());

            let state = doctor_artifact_state(&observed, artifact);
            assert_eq!(state, expected, "{host:?}: observed {observed:?}");
            assert_eq!(
                plan_artifact_action(
                    HostBundleLifecycleOpV1::Repair,
                    artifact,
                    Some(&observed),
                    true,
                )
                .is_err(),
                state == HostBundleComponentDoctorStateV1::OwnershipConflict,
                "{host:?}: planning and discovery must refuse the same observations"
            );
        }
    }
}

struct CurrentRegistration;

impl HostBundleRegistrationInspectorV1 for CurrentRegistration {
    fn inspect_registration(
        &self,
        _host: HostKindV1,
        _component: HostComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        HostBundleRegistrationStateV1::Current
    }
}

struct MissingRegistration;

impl HostBundleRegistrationInspectorV1 for MissingRegistration {
    fn inspect_registration(
        &self,
        _host: HostKindV1,
        _component: HostComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        HostBundleRegistrationStateV1::Missing
    }
}

#[test]
fn profile_lifecycle_dry_run_does_not_create_missing_control_root() {
    let artifacts = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let lifecycle = profile.path().join("host-components");
    let manifest = manifest(HostKindV1::Hermes, b"first");

    let preview = dry_run_host_bundle_lifecycle_with_lifecycle_root_at(
        artifacts.path(),
        &lifecycle,
        &manifest,
        &execution(
            HostKindV1::Hermes,
            HostBundleLifecycleOpV1::Install,
            10,
            false,
        ),
        &verifier(&manifest),
        &[],
    )
    .unwrap();

    assert!(preview.confirmation_required);
    assert!(!lifecycle.exists());
}

#[test]
fn profile_owned_receipts_enumerate_only_installed_components() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let first = manifest(HostKindV1::Hermes, b"first");
    let second = manifest(HostKindV1::Hermes, b"second");
    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap();

    writer
        .execute(
            &first,
            &execution(
                HostKindV1::Hermes,
                HostBundleLifecycleOpV1::Install,
                11,
                true,
            ),
            &content(b"first"),
            &verifier(&first),
        )
        .unwrap();
    writer
        .execute(
            &second,
            &execution(
                HostKindV1::Hermes,
                HostBundleLifecycleOpV1::Update,
                12,
                true,
            ),
            &content(b"second"),
            &verifier(&second),
        )
        .unwrap();

    assert!(
        lifecycle
            .path()
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(receipt_file(HostKindV1::Hermes, HostComponentV1::Core))
            .is_file()
    );
    assert!(
        !artifacts.path().join(HOST_BUNDLE_CONTROL_DIR).exists(),
        "receipts must be profile-owned rather than ambient-home-owned"
    );

    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(report.components.len(), 1);
    // Synthetic fixture bytes are not the verified embedded Hermes catalog
    // entry, so Doctor surfaces Repairable (catalog drift) even when the
    // registration probe reports Current.
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Repairable
    );
    assert_eq!(report.components[0].host, Some(HostKindV1::Hermes));
    assert_eq!(report.components[0].component, Some(HostComponentV1::Core));
    let repairable = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &MissingRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(
        repairable.components[0].state,
        HostBundleComponentDoctorStateV1::Repairable
    );
    assert_eq!(
        repairable.components[0].repair_action,
        "run `tracedecay install --agent hermes`"
    );

    writer
        .execute(
            &second,
            &execution(
                HostKindV1::Hermes,
                HostBundleLifecycleOpV1::Uninstall,
                15,
                true,
            ),
            &[],
            &verifier(&second),
        )
        .unwrap();
    let uninstalled = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &MissingRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert!(uninstalled.components.is_empty());

    // The same uninstall receipt with the host still advertising the
    // component is a registered orphan: nothing owns the registration, so
    // Doctor must surface it rather than skip past the receipt.
    let orphaned = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(orphaned.components.len(), 1);
    assert_eq!(
        orphaned.components[0].state,
        HostBundleComponentDoctorStateV1::OrphanedRegistration
    );
    assert_eq!(orphaned.components[0].host, Some(HostKindV1::Hermes));
    assert!(orphaned.components[0].artifacts.is_empty());
}

#[test]
fn receipt_doctor_never_treats_unknown_embedded_bundle_as_current() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let manifest = manifest(HostKindV1::CursorCloud, b"unsupported");
    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap();
    writer
        .execute(
            &manifest,
            &execution(
                HostKindV1::CursorCloud,
                HostBundleLifecycleOpV1::Install,
                16,
                true,
            ),
            &content(b"unsupported"),
            &verifier(&manifest),
        )
        .unwrap();

    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Repairable
    );
}

#[test]
fn receipt_doctor_classifies_missing_conflicting_and_corrupt_components() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let bundle = manifest(HostKindV1::OpenCode, b"current");
    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap();
    writer
        .execute(
            &bundle,
            &execution(
                HostKindV1::OpenCode,
                HostBundleLifecycleOpV1::Install,
                13,
                true,
            ),
            &content(b"current"),
            &verifier(&bundle),
        )
        .unwrap();

    let artifact = artifacts.path().join("plugins/tracedecay.json");
    std::fs::remove_file(&artifact).unwrap();
    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Missing
    );

    // Same ownership marker, different bytes: ordinary content drift. The
    // planner would converge this with `Replace` under `Repair`,
    // so discovery must not report a contested path.
    std::fs::write(&artifact, b"drifted").unwrap();
    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Drifted
    );
    assert_eq!(
        report.components[0].artifacts[0].state,
        HostBundleComponentDoctorStateV1::Drifted
    );
    assert_eq!(
        report.components[0].repair_action,
        "run `tracedecay reinstall --component core` (refreshes tracedecay-owned files)"
    );

    // A second receipt claiming the same deploy path with a different
    // ownership marker is a foreign claim: no single component owns the
    // bytes, so both components report the conflict rather than one of
    // them silently adopting the other's path.
    let foreign_receipt = HostBundleInstallReceiptV1 {
        schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
        operation_id: [23; 16],
        host: HostKindV1::Hermes,
        component: HostComponentV1::Core,
        operation: HostBundleLifecycleOpV1::Install,
        manifest_digest: manifest(HostKindV1::Hermes, b"foreign")
            .canonical_digest()
            .unwrap(),
        artifacts: vec![HostBundleReceiptArtifactV1 {
            relative_path: "plugins/tracedecay.json".to_string(),
            artifact_digest: Sha256::digest(b"foreign").into(),
            ownership_marker: expected_ownership_marker(HostKindV1::Hermes, HostComponentV1::Core),
        }],
    };
    let foreign_receipt_path = lifecycle
        .path()
        .join(HOST_BUNDLE_CONTROL_DIR)
        .join(receipt_file(HostKindV1::Hermes, HostComponentV1::Core));
    std::fs::write(
        &foreign_receipt_path,
        serde_json::to_vec(&foreign_receipt).unwrap(),
    )
    .unwrap();
    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert!(
        report
            .components
            .iter()
            .all(|component| component.state
                == HostBundleComponentDoctorStateV1::OwnershipConflict),
        "a contested deploy path conflicts for every claimant"
    );
    std::fs::remove_file(&foreign_receipt_path).unwrap();

    let receipt_path = lifecycle
        .path()
        .join(HOST_BUNDLE_CONTROL_DIR)
        .join(receipt_file(HostKindV1::OpenCode, HostComponentV1::Core));
    std::fs::write(receipt_path, b"{").unwrap();
    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Corrupt
    );
}

/// Exactly what Codex's adapter reports: activation lives behind an
/// interactive plugin UI, and the staged source bundle the host would
/// materialise from is present (`Repairable`) but never activated.
const INTERACTIVE_ACTIVATION_GUIDANCE: &str = "Non-interactive Codex plugin activation is unavailable. In Codex's plugin UI, activate \
     tracedecay from the personal marketplace, then re-run doctor.";

struct InteractiveActivationRegistration(HostBundleRegistrationStateV1);

impl HostBundleRegistrationInspectorV1 for InteractiveActivationRegistration {
    fn inspect_registration(
        &self,
        _host: HostKindV1,
        _component: HostComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        self.0
    }

    fn interactive_activation_guidance(&self, _host: HostKindV1) -> Option<String> {
        Some(INTERACTIVE_ACTIVATION_GUIDANCE.to_string())
    }
}

/// A host TraceDecay can activate without the operator: no guidance, so a
/// missing receipt-owned artifact stays a blocking receipt-integrity fault.
struct NonInteractiveStagedRegistration;

impl HostBundleRegistrationInspectorV1 for NonInteractiveStagedRegistration {
    fn inspect_registration(
        &self,
        _host: HostKindV1,
        _component: HostComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        HostBundleRegistrationStateV1::Repairable
    }
}

/// Write one receipt claiming `artifacts`, materialising only the entries
/// whose bytes are `Some`. Absent entries are what the host would have
/// created during activation.
fn write_component_receipt(
    artifact_root: &Path,
    lifecycle_root: &Path,
    host: HostKindV1,
    component: HostComponentV1,
    artifacts: &[(&str, Option<&[u8]>)],
) {
    let receipt = HostBundleInstallReceiptV1 {
        schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
        operation_id: [31; 16],
        host,
        component,
        operation: HostBundleLifecycleOpV1::Install,
        manifest_digest: Sha256::digest(b"staged-component-set").into(),
        artifacts: artifacts
            .iter()
            .map(|(relative_path, bytes)| HostBundleReceiptArtifactV1 {
                relative_path: (*relative_path).to_string(),
                artifact_digest: Sha256::digest(bytes.unwrap_or(b"activated")).into(),
                ownership_marker: expected_ownership_marker(host, component),
            })
            .collect(),
    };
    for (relative_path, bytes) in artifacts {
        let Some(bytes) = bytes else {
            continue;
        };
        let path = artifact_root.join(relative_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    std::fs::write(
        lifecycle_root
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(receipt_file(host, component)),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}

/// A host that only activates through its own UI has no command that could
/// deploy these bytes, so a component whose receipt-owned artifacts were
/// never materialised is a pending user action, not receipt drift. Doctor
/// would otherwise fail forever on every machine whose operator has not
/// clicked through the host.
#[test]
fn never_activated_interactive_host_component_defers_instead_of_failing() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    drop(HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap());
    write_component_receipt(
        artifacts.path(),
        lifecycle.path(),
        HostKindV1::Codex,
        HostComponentV1::ContextMcp,
        &[(".codex/plugins/tracedecay/.mcp.json", None)],
    );

    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &InteractiveActivationRegistration(HostBundleRegistrationStateV1::Repairable),
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();

    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::ActivationDeferred
    );
    assert_eq!(
        report.components[0].repair_action, INTERACTIVE_ACTIVATION_GUIDANCE,
        "the deferral must carry the host's own activation guidance, never a reinstall that cannot converge"
    );
}

/// The deferral is scoped to components the host never materialised. Once
/// any receipt-owned byte is on disk, an absent sibling is a file that went
/// missing after activation, real drift, and still blocking.
#[test]
fn partially_materialised_interactive_host_component_still_fails() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    drop(HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap());
    write_component_receipt(
        artifacts.path(),
        lifecycle.path(),
        HostKindV1::Codex,
        HostComponentV1::Core,
        &[
            (
                ".codex/plugins/tracedecay/.codex-plugin/plugin.json",
                Some(b"activated"),
            ),
            (".codex/plugins/tracedecay/hooks/hooks.json", None),
        ],
    );

    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &InteractiveActivationRegistration(HostBundleRegistrationStateV1::Repairable),
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();

    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Missing
    );
}

/// Nothing staged is not a pending activation: with no source bundle the
/// operator has nothing to activate, so the component is genuinely missing.
#[test]
fn unstaged_interactive_host_component_still_fails() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    drop(HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap());
    write_component_receipt(
        artifacts.path(),
        lifecycle.path(),
        HostKindV1::Codex,
        HostComponentV1::ContextMcp,
        &[(".codex/plugins/tracedecay/.mcp.json", None)],
    );

    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &InteractiveActivationRegistration(HostBundleRegistrationStateV1::Missing),
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();

    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Missing
    );
}

/// Receipt-integrity checking is untouched for every host TraceDecay can
/// actually drive: there the reinstall converges the state, so an absent
/// receipt-owned artifact keeps blocking.
#[test]
fn non_interactive_host_missing_artifacts_still_fail() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    drop(HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap());
    write_component_receipt(
        artifacts.path(),
        lifecycle.path(),
        HostKindV1::CursorDesktop,
        HostComponentV1::ContextMcp,
        &[(".cursor/plugins/local/tracedecay/mcp.json", None)],
    );

    let report = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &NonInteractiveStagedRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();

    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Missing
    );
    assert_eq!(
        report.components[0].repair_action,
        "run `tracedecay reinstall --component context-mcp`"
    );
}

#[test]
fn doctor_surfaces_restart_safe_feedback_rollback_state() {
    let root = tempfile::tempdir().unwrap();
    let writer = HostBundleWriterV1::open(root.path()).unwrap();
    std::fs::write(
        root.path()
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join("feedback-rollback.kimi.v1.json"),
        serde_json::to_vec(&serde_json::json!({
            "host": "kimi_code",
            "status": "applied"
        }))
        .unwrap(),
    )
    .unwrap();
    drop(writer);

    let report = inspect_installed_host_bundle_components_at(
        root.path(),
        root.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(report.components.len(), 1);
    assert_eq!(report.components[0].host, Some(HostKindV1::KimiCode));
    assert_eq!(report.components[0].component, Some(HostComponentV1::Core));
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Repairable
    );
    assert!(
        report.components[0]
            .repair_action
            .contains("feedback-rollback restore")
    );
}

/// A receipt written under an older schema is never migrated: it reads as
/// `ReinstallRequired` everywhere until an explicitly adopting install
/// discards it and records a current receipt over the deployed bytes.
#[test]
fn stale_schema_receipts_require_an_adopting_reinstall() {
    let root = tempfile::tempdir().unwrap();
    let set = component_set(HostKindV1::OpenCode, b"core-v1", b"agent-v1");
    let control = root.path().join(HOST_BUNDLE_CONTROL_DIR);
    fs::create_dir_all(&control).unwrap();
    let stale = control.join(receipt_file(HostKindV1::OpenCode, HostComponentV1::Core));
    fs::write(
        &stale,
        br#"{"schema_version":1,"rollback_boundary":"passed","rollback_history":[]}"#,
    )
    .unwrap();
    let mut writer = HostBundleWriterV1::open(root.path()).unwrap();

    assert_eq!(
        writer.load_receipt(HostKindV1::OpenCode, HostComponentV1::Core),
        Err(HostBundleError::ReinstallRequired)
    );
    let doctor = inspect_installed_host_bundle_components_at(
        root.path(),
        root.path(),
        &CurrentRegistration,
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .unwrap();
    assert_eq!(
        doctor.components[0].state,
        HostBundleComponentDoctorStateV1::ReinstallRequired
    );
    let plain = component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 51);
    assert_eq!(
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &set,
                &plain,
                &ComponentSetVerifier::from_set(&set),
                &mut ArtifactOnlyTestRegistration,
            )
            .err(),
        Some(HostBundleError::ReinstallRequired)
    );
    assert!(
        stale.is_file(),
        "a non-adopting install leaves the stale receipt"
    );

    let mut adopting =
        component_set_request(HostKindV1::OpenCode, HostBundleLifecycleOpV1::Install, 52);
    adopting.lifecycle.explicit_adoption = true;
    HostComponentSetTransactionV1::new(&mut writer)
        .execute(
            &set,
            &adopting,
            &ComponentSetVerifier::from_set(&set),
            &mut ArtifactOnlyTestRegistration,
        )
        .expect("an adopting install replaces the stale receipt");
    assert_eq!(
        writer
            .load_receipt(HostKindV1::OpenCode, HostComponentV1::Core)
            .unwrap()
            .expect("a current receipt replaced the stale one")
            .operation_id,
        [52; 16]
    );
}

#[test]
fn pi_hook_capability_is_proven_by_its_embedded_fixture() {
    let hooks = stock_host_capabilities(HostKindV1::Pi)
        .into_iter()
        .find(|record| record.capability == HostCapabilityV1::Hooks)
        .unwrap();
    assert_eq!(hooks.state, HostCapabilityStateV1::Supported);

    let native = stock_host_native_fixture_evidence(HostKindV1::Pi)
        .expect("Pi native fixture evidence is embedded");
    assert_eq!(
        native.source_path,
        "crates/tracedecay-hooks/fixtures/host_events/pi.json"
    );
    assert_eq!(native.stop, HostCapabilityStateV1::Supported);
    assert_eq!(
        native.edit,
        HostCapabilityStateV1::Unavailable(HostCapabilityUnavailableReasonV1::NativeFixtureLimited),
        "the Pi extension forwards no edit callback"
    );
    assert!(
        native_host_edit_stop_conformance_evidence()
            .iter()
            .any(|record| record.host == HostKindV1::Pi)
    );

    let conformance = supported_host_edit_stop_conformance_evidence()
        .into_iter()
        .find(|record| record.host == HostKindV1::Pi)
        .unwrap();
    assert_eq!(conformance.stop.route, Some(HostRegistrationRouteV1::Hook));
    assert_eq!(conformance.edit.route, None);

    require_component_capabilities(HostKindV1::Pi, HostComponentV1::Core)
        .expect("Pi Core is admitted over proven Hooks and Cli");
}
