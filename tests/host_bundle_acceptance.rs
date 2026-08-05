//! Host lifecycle behavior and authentic provider decoder coverage.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::agents::host_bundle_registry::{
    HostBundleRegistryError, default_components, unsupported_host_component_set_reason,
    verified_embedded_default_host_component_set, verified_embedded_host_bundle,
    verified_embedded_host_component_set,
};
use tracedecay::agents::host_bundle_v2::{
    ClineFamilyAdmissionV1, ClineFamilyProviderV1, HostBundleComponentDoctorStateV1,
    HostBundleComponentV1, HostBundleError, HostBundleExecutionRequestV1,
    HostBundleInstallReceiptV1, HostBundleLifecycleOpV1, HostBundleLifecycleRequestV1,
    HostBundleReceiptArtifactV1, HostBundleRegistrationInspectorV1, HostBundleRegistrationStateV1,
    HostBundleRollbackBoundaryV1, HostBundleWriterV1, HostCapabilityStateV1, HostCapabilityV1,
    HostComponentSetExecutionRequestV1, HostComponentSetLifecycleRequestV1,
    HostComponentSetRegistrationV1, HostComponentSetTransactionV1, HostKindV1,
    cline_family_evidence, dry_run_host_component_set_lifecycle_with_lifecycle_root_at,
    inspect_installed_host_bundle_components_at, native_host_edit_stop_conformance_evidence,
    stock_host_kinds, stock_host_registration_evidence,
};
use tracedecay::agents::host_component_registration::HostComponentRegistrationDelegate;
use tracedecay::agents::{
    AgentIntegration, HealthcheckContext, KimiIntegration, OpenCodeIntegration,
    inspect_receipt_backed_host_components,
};
use tracedecay_hooks::{
    HookHostV1, OpenCodePluginSurfaceV1, decode_native_hook_event, decode_opencode_lsp_event,
    decode_opencode_plugin_event,
};

fn parse_fixture(value: &str) -> Value {
    serde_json::from_str(value).expect("checked-in fixture parses")
}

fn assert_agent_integration<T: AgentIntegration>() {}

struct CurrentRegistration;

impl HostBundleRegistrationInspectorV1 for CurrentRegistration {
    fn inspect_registration(
        &self,
        _host: HostKindV1,
        _component: HostBundleComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        HostBundleRegistrationStateV1::Current
    }
}

impl HostComponentSetRegistrationV1 for CurrentRegistration {}

struct MissingRegistration;

impl HostBundleRegistrationInspectorV1 for MissingRegistration {
    fn inspect_registration(
        &self,
        _host: HostKindV1,
        _component: HostBundleComponentV1,
    ) -> HostBundleRegistrationStateV1 {
        HostBundleRegistrationStateV1::Missing
    }
}

#[test]
fn public_host_contracts_are_compiler_referenced() {
    assert_agent_integration::<KimiIntegration>();
    assert_agent_integration::<OpenCodeIntegration>();
    let _native_decoder = decode_native_hook_event;
    let _opencode_decoder = decode_opencode_plugin_event;
}

#[test]
fn embedded_component_backup_restore_runs_through_the_real_lifecycle_writer() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let bundle =
        verified_embedded_host_bundle(HostKindV1::Codex, HostBundleComponentV1::Core, 0).unwrap();
    let verifier = bundle.verifier();
    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap();
    writer
        .execute(
            &bundle.manifest,
            &HostBundleExecutionRequestV1 {
                lifecycle: HostBundleLifecycleRequestV1 {
                    operation: HostBundleLifecycleOpV1::Install,
                    expected_host: HostKindV1::Codex,
                    expected_component: HostBundleComponentV1::Core,
                    explicit_confirmation: true,
                    hermes_profile_bindings: 0,
                },
                operation_id: [81; 16],
            },
            &bundle.contents,
            &verifier,
        )
        .unwrap();
    writer
        .backup_component(&bundle.manifest, [82; 16], true, &verifier)
        .unwrap();

    let first = &bundle.contents[0];
    fs::write(
        artifacts.path().join(&first.relative_path),
        b"interrupted-host-edit",
    )
    .unwrap();
    let restored = writer
        .restore_component_backup([82; 16], [83; 16], true, &verifier)
        .unwrap();
    assert_eq!(
        restored.restored_receipt.rollback_boundary,
        HostBundleRollbackBoundaryV1::Passed
    );
    for content in &bundle.contents {
        assert_eq!(
            fs::read(artifacts.path().join(&content.relative_path)).unwrap(),
            content.bytes
        );
    }
}

#[test]
fn receipt_backed_doctor_checks_deployed_digests_registration_and_repair() {
    let artifact_root = tempfile::tempdir().unwrap();
    let lifecycle_root = tempfile::tempdir().unwrap();

    let bundle =
        verified_embedded_host_bundle(HostKindV1::KimiCode, HostBundleComponentV1::Core, 0)
            .unwrap();
    for content in &bundle.contents {
        let path = artifact_root.path().join(&content.relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &content.bytes).unwrap();
    }
    let managed_dir = artifact_root
        .path()
        .join(".kimi-code/plugins/managed/tracedecay")
        .canonicalize()
        .unwrap();
    let installed_path = artifact_root
        .path()
        .join(".kimi-code/plugins/installed.json");
    fs::create_dir_all(installed_path.parent().unwrap()).unwrap();
    fs::write(
        installed_path,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "plugins": [{
                "id": "tracedecay",
                "enabled": true,
                "source": "local-path",
                "root": managed_dir,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let receipt = HostBundleInstallReceiptV1 {
        schema_version: 1,
        operation_id: [7; 16],
        host: HostKindV1::KimiCode,
        component: HostBundleComponentV1::Core,
        operation: HostBundleLifecycleOpV1::Install,
        manifest_digest: bundle.manifest.canonical_digest().unwrap(),
        artifacts: bundle
            .manifest
            .artifacts
            .iter()
            .map(|artifact| HostBundleReceiptArtifactV1 {
                relative_path: artifact.relative_path.clone(),
                artifact_digest: artifact.artifact_digest,
                ownership_marker: artifact.ownership_marker.clone(),
            })
            .collect(),
        rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
        rollback_history: Vec::new(),
    };
    let control = lifecycle_root.path().join(".tracedecay-host-bundle-v1");
    fs::create_dir_all(&control).unwrap();
    fs::write(
        control.join("receipt.kimi-code.core.v1.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();

    let direct = inspect_installed_host_bundle_components_at(
        artifact_root.path(),
        lifecycle_root.path(),
        &CurrentRegistration,
    )
    .unwrap();
    assert_eq!(direct.components.len(), 1);
    let component = &direct.components[0];
    assert_eq!(component.state, HostBundleComponentDoctorStateV1::Current);
    assert_eq!(
        component.registration,
        Some(HostBundleRegistrationStateV1::Current)
    );
    assert!(
        component
            .artifacts
            .iter()
            .all(|artifact| artifact.observed_digest == Some(artifact.expected_digest))
    );
    assert_eq!(component.repair_action, "none");

    let owner = inspect_receipt_backed_host_components(
        &HealthcheckContext {
            home: artifact_root.path().to_path_buf(),
            project_path: artifact_root.path().to_path_buf(),
        },
        lifecycle_root.path(),
    )
    .unwrap();
    assert_eq!(owner.components.len(), 1);
    assert_eq!(
        owner.components[0].registration,
        Some(HostBundleRegistrationStateV1::Current)
    );

    // The receipt still owns the path, so moved bytes are content drift the
    // ordinary reinstall converges — not a contested claim.
    let modified = artifact_root
        .path()
        .join(&receipt.artifacts[0].relative_path);
    fs::write(modified, b"modified").unwrap();
    let repair = inspect_installed_host_bundle_components_at(
        artifact_root.path(),
        lifecycle_root.path(),
        &CurrentRegistration,
    )
    .unwrap();
    assert_eq!(
        repair.components[0].state,
        HostBundleComponentDoctorStateV1::Drifted
    );
    assert!(!repair.components[0].repair_action.is_empty());
}

#[test]
fn cursor_native_extension_receipt_matches_embedded_assets() {
    let artifact_root = tempfile::tempdir().unwrap();
    let lifecycle_root = tempfile::tempdir().unwrap();
    let bundle =
        verified_embedded_host_bundle(HostKindV1::CursorDesktop, HostBundleComponentV1::Agent, 0)
            .unwrap();
    for content in &bundle.contents {
        let path = artifact_root.path().join(&content.relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &content.bytes).unwrap();
    }
    let receipt = HostBundleInstallReceiptV1 {
        schema_version: 1,
        operation_id: [8; 16],
        host: HostKindV1::CursorDesktop,
        component: HostBundleComponentV1::Agent,
        operation: HostBundleLifecycleOpV1::Install,
        manifest_digest: bundle.manifest.canonical_digest().unwrap(),
        artifacts: bundle
            .manifest
            .artifacts
            .iter()
            .map(|artifact| HostBundleReceiptArtifactV1 {
                relative_path: artifact.relative_path.clone(),
                artifact_digest: artifact.artifact_digest,
                ownership_marker: artifact.ownership_marker.clone(),
            })
            .collect(),
        rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
        rollback_history: Vec::new(),
    };
    assert!(receipt.artifacts.iter().all(|artifact| {
        artifact
            .relative_path
            .starts_with(".cursor/extensions/tracedecay.cursor-native-0.0.0/")
    }));
    let control = lifecycle_root.path().join(".tracedecay-host-bundle-v1");
    fs::create_dir_all(&control).unwrap();
    fs::write(
        control.join("receipt.cursor-desktop.agent.v1.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    let report = inspect_installed_host_bundle_components_at(
        artifact_root.path(),
        lifecycle_root.path(),
        &CurrentRegistration,
    )
    .unwrap();
    assert_eq!(report.components.len(), 1);
    assert_eq!(
        report.components[0].state,
        HostBundleComponentDoctorStateV1::Current
    );
    assert!(
        report.components[0]
            .artifacts
            .iter()
            .all(|artifact| artifact.observed_digest == Some(artifact.expected_digest))
    );
}

#[test]
fn component_set_dry_run_retains_analyzers_but_refuses_registration_aliases() {
    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let component_set =
        verified_embedded_default_host_component_set(HostKindV1::OpenCode, 0).unwrap();
    let request = HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation: HostBundleLifecycleOpV1::Repair,
            expected_host: HostKindV1::OpenCode,
            expected_components: default_components(HostKindV1::OpenCode),
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
        },
        operation_id: [31; 16],
    };

    let mut clean_registration = HostComponentRegistrationDelegate::new(
        "opencode",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
    )
    .unwrap();
    dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
        home.path(),
        lifecycle.path(),
        &component_set.component_set,
        &request,
        &component_set,
        &mut clean_registration,
    )
    .expect("a clean temporary OpenCode profile passes dry run");

    let config_path = home.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!({
            "lsp": {
                "rust-analyzer": {
                    "command": ["rust-analyzer"],
                    "extensions": [".rs"]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut retained_registration = HostComponentRegistrationDelegate::new(
        "opencode",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
    )
    .unwrap();
    dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
        home.path(),
        lifecycle.path(),
        &component_set.component_set,
        &request,
        &component_set,
        &mut retained_registration,
    )
    .expect("an existing language analyzer is retained beside projection-only TraceDecay LSP");

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!({
            "lsp": {
                "third-party-tracedecay": {
                    "command": ["third-party-tracedecay"],
                    "extensions": [".rs"]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut conflicting_registration = HostComponentRegistrationDelegate::new(
        "opencode",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
    )
    .unwrap();
    assert_eq!(
        dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
            home.path(),
            lifecycle.path(),
            &component_set.component_set,
            &request,
            &component_set,
            &mut conflicting_registration,
        ),
        Err(HostBundleError::OwnershipConflict),
        "dry run must refuse a third-party registration aliasing TraceDecay"
    );
}

#[test]
fn unsupported_host_components_are_not_advertised_or_constructible() {
    for host in [
        HostKindV1::CursorCloud,
        HostKindV1::ClineFamily,
        HostKindV1::Cline,
        HostKindV1::RooCode,
        HostKindV1::Kilo,
    ] {
        assert!(
            default_components(host).is_empty(),
            "{host:?} must stay typed unavailable until its safety evidence is sufficient"
        );
        let reason = unsupported_host_component_set_reason(host)
            .unwrap_or_else(|| panic!("{host:?} must carry an exact unavailable reason"));
        assert_eq!(
            verified_embedded_host_component_set(host, &[HostBundleComponentV1::Core], 0),
            Err(HostBundleRegistryError::HostComponentSetUnavailable { host, reason }),
            "{host:?} must refuse an explicit component request with its exact reason"
        );
        assert_eq!(
            verified_embedded_default_host_component_set(host, 0),
            Err(HostBundleRegistryError::HostComponentSetUnavailable { host, reason }),
            "{host:?} must report a typed unavailable default set, never an empty one"
        );
    }
}

#[test]
fn native_host_evidence_is_embedded_and_covers_every_advertised_native_route() {
    let evidence = native_host_edit_stop_conformance_evidence();
    for host in [
        HostKindV1::ClaudeCode,
        HostKindV1::CursorDesktop,
        HostKindV1::Codex,
        HostKindV1::Hermes,
        HostKindV1::KimiCode,
        HostKindV1::OpenCode,
    ] {
        let record = evidence
            .iter()
            .find(|record| record.host == host)
            .unwrap_or_else(|| panic!("{host:?} has no embedded native fixture evidence"));
        assert_ne!(record.fixture_digest, [0; 32]);
        assert!(!record.source_path.is_empty());
    }
}

#[test]
fn cursor_cloud_is_recognized_but_never_advertised_as_supported() {
    assert!(
        stock_host_registration_evidence(HostKindV1::CursorCloud)
            .iter()
            .all(|record| matches!(record.state, HostCapabilityStateV1::Unavailable(_)))
    );
}

/// The official component-set dry run must supply conflict discovery to its
/// own guard: a third-party analyzer already serving a language TraceDecay
/// projects is reported, demands confirmation, and binds into the plan digest
/// so it cannot appear between preview and apply.
#[test]
fn component_set_dry_run_reports_competing_claims_and_binds_them_to_the_plan() {
    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let component_set =
        verified_embedded_default_host_component_set(HostKindV1::OpenCode, 0).unwrap();
    let request = HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation: HostBundleLifecycleOpV1::Repair,
            expected_host: HostKindV1::OpenCode,
            expected_components: default_components(HostKindV1::OpenCode),
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
        },
        operation_id: [43; 16],
    };
    let preview_now = |home: &Path| {
        let mut registration = HostComponentRegistrationDelegate::new(
            "opencode",
            home,
            lifecycle.path(),
            request.lifecycle.operation,
        )
        .unwrap();
        dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
            home,
            lifecycle.path(),
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        )
    };

    let clean = preview_now(home.path()).expect("a clean profile previews");
    assert!(clean.competing_extension_claims.is_empty());
    assert!(!clean.confirmation_required);

    let config_path = home.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!({
            "lsp": {
                "rust-analyzer": { "command": ["rust-analyzer"], "extensions": [".rs"] },
                "elm-language-server": { "command": ["elm-ls"], "extensions": [".elm"] }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let contested =
        preview_now(home.path()).expect("a competing analyzer is reported, not refused");
    let claims = &contested.competing_extension_claims;
    assert_eq!(
        claims
            .iter()
            .map(|claim| claim.extension_id.as_str())
            .collect::<Vec<_>>(),
        vec!["rust-analyzer"],
        "only an analyzer claiming a language TraceDecay projects is a competing claim"
    );
    assert_eq!(claims[0].capability, HostCapabilityV1::Lsp);
    assert_ne!(claims[0].evidence_digest, [0; 32]);
    assert!(
        contested.confirmation_required,
        "ambiguous ownership must demand explicit confirmation"
    );
    assert_ne!(
        contested.plan_digest, clean.plan_digest,
        "a competing claim must change the confirmed plan identity"
    );
    assert_eq!(
        contested.component_plans, clean.component_plans,
        "discovery reports a conflict; it never rewrites the artifact plan"
    );

    // A claim appearing after the operator confirmed invalidates that plan.
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!({
            "lsp": {
                "rust-analyzer": { "command": ["rust-analyzer"], "extensions": [".rs"] },
                "pyright": { "command": ["pyright-langserver"], "extensions": [".py"] }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let widened = preview_now(home.path()).expect("a second competing analyzer previews");
    assert_eq!(widened.competing_extension_claims.len(), 2);
    assert_ne!(widened.plan_digest, contested.plan_digest);

    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path()).unwrap();
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let mut registration = HostComponentRegistrationDelegate::new(
        "opencode",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
    )
    .unwrap();
    assert!(
        matches!(
            transaction.execute_confirmed(
                &component_set.component_set,
                &request,
                &contested,
                &component_set,
                &mut registration,
            ),
            Err(HostBundleError::StalePreview(_))
        ),
        "apply must refuse a preview confirmed before the newest competing claim"
    );
}

/// Discovery that cannot read the host's registration surface must refuse
/// rather than report a clear one.
#[test]
fn unreadable_host_registration_refuses_instead_of_reporting_no_conflict() {
    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let config_path = home.path().join(".config/opencode/opencode.json");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::write(&config_path, b"{ this is not json").unwrap();

    let component_set =
        verified_embedded_default_host_component_set(HostKindV1::OpenCode, 0).unwrap();
    let request = HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation: HostBundleLifecycleOpV1::Repair,
            expected_host: HostKindV1::OpenCode,
            expected_components: default_components(HostKindV1::OpenCode),
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
        },
        operation_id: [44; 16],
    };
    let mut registration = HostComponentRegistrationDelegate::new(
        "opencode",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
    )
    .unwrap();
    assert_eq!(
        dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
            home.path(),
            lifecycle.path(),
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        ),
        Err(HostBundleError::InvalidObservedState)
    );
}

/// Cline-family support is whatever the checked-in evidence packet admits for
/// that exact provider — never a source file, a shared configuration shape, or
/// family branding.
#[test]
fn cline_family_routes_come_only_from_the_checked_in_evidence_packet() {
    let packet: Value = serde_json::from_str(include_str!(
        "../crates/tracedecay-hooks/fixtures/host_events/cline-family.json"
    ))
    .expect("the checked-in Cline evidence packet parses");

    for (provider, packet_id) in [
        (ClineFamilyProviderV1::Cline, "cline"),
        (ClineFamilyProviderV1::RooCode, "roo-code"),
        (ClineFamilyProviderV1::Kilo, "kilo"),
    ] {
        let evidence = cline_family_evidence(provider)
            .unwrap_or_else(|| panic!("{provider:?} is described by the packet"));
        let entry = packet["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["provider"] == packet_id)
            .unwrap_or_else(|| panic!("{packet_id} is listed in the packet"));

        assert_ne!(evidence.evidence_packet_digest, [0; 32]);
        assert_eq!(
            evidence.evidence_packet_path,
            "crates/tracedecay-hooks/fixtures/host_events/cline-family.json"
        );
        assert_eq!(
            evidence.registration.evidence_ref, evidence.evidence_packet_path,
            "the packet, not an adapter source file, is the registration evidence"
        );
        assert_ne!(
            evidence.admission,
            ClineFamilyAdmissionV1::Verified,
            "{packet_id} is not captured natively, so it must not claim a verified route"
        );
        assert_eq!(
            evidence.unavailable_reason.as_deref(),
            entry["reason"].as_str(),
            "{packet_id} must report the packet's exact reason"
        );
        for state in [evidence.registration.state, evidence.edit, evidence.stop] {
            assert!(
                matches!(state, HostCapabilityStateV1::Unavailable(_)),
                "{packet_id} must stay typed unavailable on every surface"
            );
        }
    }
}

/// The Doctor report is the production consumer of the native conformance
/// matrix, and reports it even when nothing is installed.
#[test]
fn doctor_reports_native_edit_stop_conformance_without_any_install() {
    let lifecycle = tempfile::tempdir().unwrap();
    let report = inspect_installed_host_bundle_components_at(
        lifecycle.path(),
        &lifecycle.path().join("absent"),
        &CurrentRegistration,
    )
    .expect("an absent lifecycle root still reports host evidence");

    assert!(report.components.is_empty());
    assert_eq!(
        report.native_edit_stop_conformance,
        native_host_edit_stop_conformance_evidence(),
        "an empty install must not read as an absence of host conformance evidence"
    );
    assert!(
        report
            .native_edit_stop_conformance
            .iter()
            .any(|evidence| evidence.host == HostKindV1::ClaudeCode)
    );
}

#[test]
fn embedded_component_sets_complete_lifecycle_for_all_supported_hosts() {
    let mut covered_hosts = 0;
    for host in stock_host_kinds() {
        let mut expected_components = default_components(host);
        if expected_components.is_empty() {
            continue;
        }
        expected_components.sort_unstable();
        covered_hosts += 1;
        let artifacts = tempfile::tempdir().unwrap();
        let lifecycle = tempfile::tempdir().unwrap();
        let component_set = verified_embedded_default_host_component_set(host, 0).unwrap();
        assert_eq!(
            component_set
                .component_set
                .components
                .iter()
                .map(|component| component.manifest.component)
                .collect::<Vec<_>>(),
            expected_components
        );
        let request = |operation, operation_id| HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation,
                expected_host: host,
                expected_components: expected_components.clone(),
                explicit_confirmation: true,
                // Hermes binds exactly one user TraceDecay profile; other hosts
                // must pass zero (not an ambient profile-discovery mechanism).
                hermes_profile_bindings: u8::from(host == HostKindV1::Hermes),
            },
            operation_id: [operation_id; 16],
        };
        let mut writer =
            HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path())
                .unwrap();
        let mut registration = CurrentRegistration;

        let install_request = request(HostBundleLifecycleOpV1::Install, 41);
        let install_preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &component_set.component_set,
                &install_request,
                &component_set,
                &mut registration,
            )
            .unwrap();
        let install = HostComponentSetTransactionV1::new(&mut writer)
            .execute_confirmed(
                &component_set.component_set,
                &install_request,
                &install_preview,
                &component_set,
                &mut registration,
            )
            .unwrap();
        assert_eq!(install.component_receipts.len(), expected_components.len());
        assert_eq!(
            install.confirmed_plan_digest,
            Some(install_preview.plan_digest)
        );
        assert_eq!(
            install.base_registration_revision,
            Some(install_preview.base_registration_revision)
        );
        assert_eq!(
            install.current_registration_revision,
            Some(install_preview.current_registration_revision)
        );
        assert_eq!(
            install.artifact_state_revision,
            Some(install_preview.artifact_state_revision)
        );
        let replay = HostComponentSetTransactionV1::new(&mut writer)
            .execute_confirmed(
                &component_set.component_set,
                &install_request,
                &install_preview,
                &component_set,
                &mut registration,
            )
            .unwrap();
        assert_eq!(replay, install);
        let mut wrong_preview = install_preview.clone();
        wrong_preview.plan_digest[0] ^= 1;
        assert!(matches!(
            HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
                &component_set.component_set,
                &install_request,
                &wrong_preview,
                &component_set,
                &mut registration,
            ),
            Err(HostBundleError::StalePreview(_))
        ));
        assert!(
            inspect_installed_host_bundle_components_at(
                artifacts.path(),
                lifecycle.path(),
                &CurrentRegistration,
            )
            .unwrap()
            .components
            .iter()
            .all(|component| component.state == HostBundleComponentDoctorStateV1::Current)
        );

        let update_request = request(HostBundleLifecycleOpV1::Update, 42);
        let update_preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &component_set.component_set,
                &update_request,
                &component_set,
                &mut registration,
            )
            .unwrap();
        let update = HostComponentSetTransactionV1::new(&mut writer)
            .execute_confirmed(
                &component_set.component_set,
                &update_request,
                &update_preview,
                &component_set,
                &mut registration,
            )
            .unwrap();
        assert!(
            update
                .component_receipts
                .iter()
                .all(|receipt| receipt.operation == HostBundleLifecycleOpV1::Update)
        );

        let repair_target = component_set.component_set.components[0].manifest.artifacts[0]
            .relative_path
            .clone();
        fs::write(artifacts.path().join(repair_target), b"corrupted").unwrap();
        let repair_request = request(HostBundleLifecycleOpV1::Repair, 43);
        let repair_preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &component_set.component_set,
                &repair_request,
                &component_set,
                &mut registration,
            )
            .unwrap();
        let repair = HostComponentSetTransactionV1::new(&mut writer)
            .execute_confirmed(
                &component_set.component_set,
                &repair_request,
                &repair_preview,
                &component_set,
                &mut registration,
            )
            .unwrap();
        assert!(
            repair
                .component_receipts
                .iter()
                .all(|receipt| receipt.operation == HostBundleLifecycleOpV1::Repair)
        );
        assert!(
            inspect_installed_host_bundle_components_at(
                artifacts.path(),
                lifecycle.path(),
                &CurrentRegistration,
            )
            .unwrap()
            .components
            .iter()
            .all(|component| component.state == HostBundleComponentDoctorStateV1::Current)
        );

        let uninstall_request = request(HostBundleLifecycleOpV1::Uninstall, 44);
        let uninstall_preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &component_set.component_set,
                &uninstall_request,
                &component_set,
                &mut registration,
            )
            .unwrap();
        let uninstall = HostComponentSetTransactionV1::new(&mut writer)
            .execute_confirmed(
                &component_set.component_set,
                &uninstall_request,
                &uninstall_preview,
                &component_set,
                &mut registration,
            )
            .unwrap();
        assert!(uninstall.component_receipts.iter().all(|receipt| {
            receipt.operation == HostBundleLifecycleOpV1::Uninstall && receipt.artifacts.is_empty()
        }));
        assert!(
            inspect_installed_host_bundle_components_at(
                artifacts.path(),
                lifecycle.path(),
                &MissingRegistration,
            )
            .unwrap()
            .components
            .is_empty()
        );
        // A host that still advertises an uninstalled component is a
        // registered orphan no receipt owns, and must stay visible.
        assert!(
            inspect_installed_host_bundle_components_at(
                artifacts.path(),
                lifecycle.path(),
                &CurrentRegistration,
            )
            .unwrap()
            .components
            .iter()
            .all(|component| component.state
                == HostBundleComponentDoctorStateV1::OrphanedRegistration)
        );
    }
    assert!(covered_hosts > 0);
}

/// Cursor Core is the host that provoked this: its receipt-owned plugin bundle
/// is regenerated on every version bump (`.cursor-plugin/plugin.json` stamps
/// the package version, `hooks/hooks.json` bakes the resolved binary path), so
/// a second writer outside the transaction guarantees byte drift.
///
/// Drift on a path the receipt still owns must be a warning Doctor reports, not
/// a blocking ownership conflict, and `Repair` must converge it while backing
/// the previous bytes up. Nothing under `.cursor` that TraceDecay does not own
/// may change — including when a run is interrupted before it mutates anything.
#[test]
fn cursor_core_drift_warns_and_reinstall_converges_with_a_backup() {
    let artifacts = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let component_set = verified_embedded_host_component_set(
        HostKindV1::CursorDesktop,
        &[HostBundleComponentV1::Core],
        0,
    )
    .unwrap();
    let request = |operation, operation_id| HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation,
            expected_host: HostKindV1::CursorDesktop,
            expected_components: vec![HostBundleComponentV1::Core],
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
        },
        operation_id: [operation_id; 16],
    };

    // Unrelated, user-owned Cursor config that TraceDecay never claims.
    let unrelated = artifacts.path().join(".cursor/mcp.json");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    let unrelated_bytes = br#"{"mcpServers":{"someone-else":{"command":"other"}}}"#;
    fs::write(&unrelated, unrelated_bytes).unwrap();

    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(artifacts.path(), lifecycle.path()).unwrap();
    let mut registration = CurrentRegistration;
    let install_request = request(HostBundleLifecycleOpV1::Install, 71);
    let install_preview = HostComponentSetTransactionV1::new(&mut writer)
        .preview(
            &component_set.component_set,
            &install_request,
            &component_set,
            &mut registration,
        )
        .unwrap();
    HostComponentSetTransactionV1::new(&mut writer)
        .execute_confirmed(
            &component_set.component_set,
            &install_request,
            &install_preview,
            &component_set,
            &mut registration,
        )
        .unwrap();

    // Rewrite a receipt-owned file the way the generated-plugin refresh did:
    // same path, same owner, different rendered bytes.
    let manifest_path = ".cursor/plugins/local/tracedecay/.cursor-plugin/plugin.json";
    assert!(
        component_set.component_set.components[0]
            .manifest
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == manifest_path),
        "the Cursor plugin manifest must be receipt-owned"
    );
    const REFRESHED: &[u8] = br#"{"name":"tracedecay","version":"9.9.9"}"#;
    let deployed = artifacts.path().join(manifest_path);
    let owned_bytes = fs::read(&deployed).unwrap();
    fs::write(&deployed, REFRESHED).unwrap();

    let drifted = inspect_installed_host_bundle_components_at(
        artifacts.path(),
        lifecycle.path(),
        &CurrentRegistration,
    )
    .unwrap();
    assert_eq!(
        drifted.components[0].state,
        HostBundleComponentDoctorStateV1::Drifted,
        "a receipt-owned path whose owner is unchanged is drift, not a conflict"
    );
    assert!(
        drifted.components[0]
            .artifacts
            .iter()
            .any(|artifact| artifact.relative_path == manifest_path
                && artifact.state == HostBundleComponentDoctorStateV1::Drifted)
    );

    // An interrupted run — refused before it mutates anything — leaves both the
    // drifted artifact and the unrelated Cursor config exactly as they were.
    let repair_request = request(HostBundleLifecycleOpV1::Repair, 72);
    let repair_preview = HostComponentSetTransactionV1::new(&mut writer)
        .preview(
            &component_set.component_set,
            &repair_request,
            &component_set,
            &mut registration,
        )
        .unwrap();
    let mut stale = repair_preview.clone();
    stale.plan_digest[0] ^= 1;
    assert!(matches!(
        HostComponentSetTransactionV1::new(&mut writer).execute_confirmed(
            &component_set.component_set,
            &repair_request,
            &stale,
            &component_set,
            &mut registration,
        ),
        Err(HostBundleError::StalePreview(_))
    ));
    assert_eq!(fs::read(&unrelated).unwrap(), unrelated_bytes);
    assert_eq!(fs::read(&deployed).unwrap(), REFRESHED);

    HostComponentSetTransactionV1::new(&mut writer)
        .execute_confirmed(
            &component_set.component_set,
            &repair_request,
            &repair_preview,
            &component_set,
            &mut registration,
        )
        .unwrap();

    assert_eq!(fs::read(&deployed).unwrap(), owned_bytes);
    assert_eq!(
        inspect_installed_host_bundle_components_at(
            artifacts.path(),
            lifecycle.path(),
            &CurrentRegistration,
        )
        .unwrap()
        .components[0]
            .state,
        HostBundleComponentDoctorStateV1::Current
    );
    assert_eq!(fs::read(&unrelated).unwrap(), unrelated_bytes);

    // The replaced bytes were backed up before the repair overwrote them.
    let backups = lifecycle
        .path()
        .join(".tracedecay-host-bundle-v1")
        .join("backups");
    let backed_up = walk_files(&backups)
        .into_iter()
        .any(|path| fs::read(&path).is_ok_and(|bytes| bytes == REFRESHED));
    assert!(
        backed_up,
        "repair must back the replaced bytes up before it re-owns the path"
    );
}

fn walk_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn authentic_host_fixtures_use_production_typed_decoders() {
    let fixtures: &[(HookHostV1, &str)] = &[
        (
            HookHostV1::ClaudeCode,
            include_str!(
                "../crates/tracedecay-hooks/fixtures/host_events/claude/post_tool_use_write.json"
            ),
        ),
        (
            HookHostV1::ClaudeCode,
            include_str!("../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json"),
        ),
        (
            HookHostV1::Codex,
            include_str!("../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json"),
        ),
        (
            HookHostV1::CursorDesktop,
            include_str!(
                "../crates/tracedecay-hooks/fixtures/host_events/cursor/after-file-edit.json"
            ),
        ),
        (
            HookHostV1::Hermes,
            include_str!("../crates/tracedecay-hooks/fixtures/host_events/hermes/saved-edit.json"),
        ),
        (
            HookHostV1::Hermes,
            include_str!("../crates/tracedecay-hooks/fixtures/host_events/hermes/stop.json"),
        ),
        (
            HookHostV1::KimiCode,
            include_str!(
                "../crates/tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
            ),
        ),
        (
            HookHostV1::KimiCode,
            include_str!("../crates/tracedecay-hooks/fixtures/host_events/kimi/stop.json"),
        ),
    ];
    for (host, fixture) in fixtures {
        decode_native_hook_event(*host, fixture.as_bytes())
            .unwrap_or_else(|error| panic!("{host:?} fixture rejected: {error}"));
    }

    let opencode = parse_fixture(include_str!(
        "../crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json"
    ));
    let events = opencode["events"].as_array().expect("OpenCode events");
    for identity in ["saved_edit", "stop"] {
        let event = events
            .iter()
            .find(|event| event["identity"] == identity)
            .expect("OpenCode fixture identity");
        decode_opencode_plugin_event(
            OpenCodePluginSurfaceV1::Event,
            serde_json::to_vec(&event["request"]).unwrap().as_slice(),
        )
        .unwrap_or_else(|error| panic!("OpenCode {identity} rejected: {error}"));
    }
    let tool_after = events
        .iter()
        .find(|event| event["identity"] == "post_tool_use")
        .expect("OpenCode tool.execute.after");
    decode_opencode_plugin_event(
        OpenCodePluginSurfaceV1::ToolExecuteAfter,
        serde_json::to_vec(&tool_after["request"])
            .unwrap()
            .as_slice(),
    )
    .expect("OpenCode tool.execute.after decodes");
    let lsp_updated = events
        .iter()
        .find(|event| event["identity"] == "lsp_updated")
        .expect("OpenCode lsp.updated");
    decode_opencode_lsp_event(
        serde_json::to_vec(&lsp_updated["request"])
            .unwrap()
            .as_slice(),
    )
    .expect("OpenCode lsp.updated decodes");
}

#[test]
fn corrupted_host_identity_fails_typed_decoder() {
    let mut fixture = parse_fixture(include_str!(
        "../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json"
    ));
    fixture["hook_event_name"] = json!("NotARealEvent");
    assert!(
        decode_native_hook_event(
            HookHostV1::ClaudeCode,
            serde_json::to_vec(&fixture).unwrap().as_slice(),
        )
        .is_err()
    );
}
