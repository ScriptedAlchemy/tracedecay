//! Kiro lifecycle: file-edited MCP registration, steering, and doctor.

use super::*;
#[cfg(unix)]
use tracedecay_runtime_core::test_executable::write_executable_script;

#[test]
fn every_steering_mutation_branch_requires_a_persisted_write_intent() {
    for (case, original) in steering_mutation_cases() {
        let root = tempfile::tempdir().unwrap();
        let steering = root.path().join("tracedecay.md");
        if let Some(original) = &original {
            std::fs::write(&steering, original).unwrap();
        }
        let blocked_intent_root = root.path().join("blocked-intent-root");
        std::fs::write(&blocked_intent_root, b"not a directory").unwrap();

        let error = crate::agents::with_host_config_write_intents(blocked_intent_root, || {
            install_steering_rules(&steering)
        })
        .expect_err(case);

        assert!(
            error
                .to_string()
                .contains("could not create host config write intent directory"),
            "{case}: unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read(&steering).ok().as_deref(),
            original.as_deref(),
            "{case}: failed intent persistence must leave the target byte-identical"
        );
    }
}

fn steering_mutation_cases() -> Vec<(&'static str, Option<Vec<u8>>)> {
    vec![
        (
            "current-sentinel refresh",
            Some(
                format!(
                    "operator rules\n\n{}\n",
                    STEERING_SENTINELS.render("## Older heading\n\nstale rules")
                )
                .into_bytes(),
            ),
        ),
        ("existing append", Some(b"operator rules\n".to_vec())),
        ("missing create", None),
    ]
}

#[test]
fn healthcheck_skips_steering_when_legacy_file_is_absent() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"tracedecay":{"command":"/bin/tracedecay","args":["serve"],"disabled":false}}}"#,
    )
    .unwrap();

    let mut counters = DoctorCounters::new();
    KiroIntegration.healthcheck(
        &mut counters,
        &HealthcheckContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(
        counters.issues, 0,
        "MCP-only global install must not fail doctor for missing legacy steering"
    );
}

#[test]
fn global_activate_does_not_create_missing_legacy_steering() {
    use crate::agents::host_bundle::HostComponentV1;
    use crate::agents::{AgentIntegration, InstallContext};

    let home = tempfile::tempdir().unwrap();
    let steering = home.path().join(".kiro/steering/tracedecay.md");
    assert!(!steering.exists());

    KiroIntegration
        .activate_deployed_host_component_registration(
            &[HostComponentV1::ContextMcp],
            &InstallContext {
                profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
                home: home.path().to_path_buf(),
                tracedecay_bin: "/bin/tracedecay".to_string(),
                project_root: None,
                dashboard: false,
            },
        )
        .expect("MCP-only activate must succeed without a steering file");

    assert!(
        !steering.exists(),
        "catalog-native global activate must not recreate retired steering"
    );
}

#[test]
fn every_steering_mutation_branch_refuses_a_stale_target() {
    for (case, original) in steering_mutation_cases() {
        let root = tempfile::tempdir().unwrap();
        let steering = root.path().join("tracedecay.md");
        if let Some(original) = original {
            std::fs::write(&steering, original).unwrap();
        }
        let pause = crate::agents::pause_next_host_config_write_after_validation(&steering);
        let writer_path = steering.clone();
        let writer = std::thread::spawn(move || {
            install_steering_rules(&writer_path).map_err(|error| error.to_string())
        });
        pause.wait_until_reached();
        let foreign = format!("foreign Kiro edit during {case}\n");
        std::fs::write(&steering, foreign.as_bytes()).unwrap();
        pause.resume();

        let error = writer.join().unwrap().expect_err(case);
        assert!(
            error.contains("changed since it was read"),
            "{case}: {error}"
        );
        assert_eq!(std::fs::read(&steering).unwrap(), foreign.as_bytes());
    }
}

#[test]
fn every_steering_mutation_branch_converges_through_the_same_writer() {
    let block = steering_block_text();
    for (case, original) in steering_mutation_cases() {
        let root = tempfile::tempdir().unwrap();
        let steering = root.path().join("tracedecay.md");
        if let Some(original) = original {
            std::fs::write(&steering, original).unwrap();
        }

        install_steering_rules(&steering).unwrap();

        let installed = std::fs::read_to_string(&steering).unwrap();
        assert_eq!(
            installed.matches(&block).count(),
            1,
            "{case}: the canonical block must appear exactly once"
        );
        if case != "missing create" {
            assert!(
                installed.contains("operator rules"),
                "{case}: operator content must survive"
            );
        }
    }
}

#[test]
fn steering_install_rejects_non_utf8_without_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let steering = root.path().join("tracedecay.md");
    let invalid = b"operator rules\n\xff\xfe";
    std::fs::write(&steering, invalid).unwrap();

    let error = install_steering_rules(&steering).unwrap_err();

    assert!(error.to_string().contains("as UTF-8"), "{error}");
    assert_eq!(std::fs::read(&steering).unwrap(), invalid);
}

#[cfg(unix)]
#[test]
fn steering_install_rejects_unreadable_input_without_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let steering = root.path().join("tracedecay.md");
    std::fs::write(&steering, b"operator rules\n").unwrap();
    std::fs::set_permissions(&steering, std::fs::Permissions::from_mode(0o000)).unwrap();
    let error = install_steering_rules(&steering).unwrap_err();
    std::fs::set_permissions(&steering, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(error.to_string().contains("failed to read"), "{error}");
    assert_eq!(std::fs::read(&steering).unwrap(), b"operator rules\n");
}

#[cfg(unix)]
#[test]
fn steering_install_refuses_a_symlink_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside.md");
    let steering = root.path().join("tracedecay.md");
    std::fs::write(&outside, b"operator rules\n").unwrap();
    symlink(&outside, &steering).unwrap();

    let error = install_steering_rules(&steering).unwrap_err();

    assert!(
        error.to_string().contains("unsafe host metadata path"),
        "{error}"
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"operator rules\n");
}

#[test]
fn steering_uninstall_refuses_a_concurrent_edit_before_nonempty_rewrite() {
    let root = tempfile::tempdir().unwrap();
    let steering = root.path().join("tracedecay.md");
    std::fs::write(&steering, b"operator rules\n").unwrap();
    install_steering_rules(&steering).unwrap();
    let pause = crate::agents::pause_next_host_config_write_at_publication(&steering);
    let writer_path = steering.clone();
    let remover = std::thread::spawn(move || {
        remove_steering_rules(&writer_path).map_err(|error| error.to_string())
    });
    pause.wait_until_reached();

    let foreign = b"foreign Kiro edit\n";
    std::fs::write(&steering, foreign).unwrap();
    pause.resume();
    let error = remover.join().unwrap().unwrap_err();

    assert!(error.contains("changed since it was read"), "{error}");
    assert_eq!(std::fs::read(&steering).unwrap(), foreign);
}

#[test]
fn steering_uninstall_refuses_a_concurrent_edit_before_empty_deletion() {
    let root = tempfile::tempdir().unwrap();
    let steering = root.path().join("tracedecay.md");
    install_steering_rules(&steering).unwrap();
    let pause = crate::agents::pause_next_host_config_write_at_publication(&steering);
    let writer_path = steering.clone();
    let remover = std::thread::spawn(move || {
        remove_steering_rules(&writer_path).map_err(|error| error.to_string())
    });
    pause.wait_until_reached();

    let foreign = b"foreign Kiro edit\n";
    std::fs::write(&steering, foreign).unwrap();
    pause.resume();
    let error = remover.join().unwrap().unwrap_err();

    assert!(error.contains("changed since it was read"), "{error}");
    assert_eq!(std::fs::read(&steering).unwrap(), foreign);
}

#[test]
fn steering_empty_deletion_requires_a_persisted_remove_intent() {
    let root = tempfile::tempdir().unwrap();
    let steering = root.path().join("tracedecay.md");
    let mut facts = Vec::new();
    crate::agents::recorded_lifecycle(root.path(), &mut facts, false, || {
        install_steering_rules(&steering)
    })
    .unwrap();
    let original = std::fs::read(&steering).unwrap();
    let blocked_intent_root = root.path().join("blocked-intent-root");
    std::fs::write(&blocked_intent_root, b"not a directory").unwrap();

    let error = crate::agents::with_host_config_write_intents(blocked_intent_root, || {
        crate::agents::recorded_lifecycle(root.path(), &mut facts, true, || {
            remove_steering_rules(&steering)
        })
    })
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("could not create host config remove intent directory"),
        "{error}"
    );
    assert_eq!(std::fs::read(&steering).unwrap(), original);
}

#[test]
fn steering_uninstall_rewrites_operator_content_and_deletes_an_empty_result() {
    let root = tempfile::tempdir().unwrap();
    let nonempty = root.path().join("nonempty.md");
    std::fs::write(&nonempty, b"operator rules\n").unwrap();
    install_steering_rules(&nonempty).unwrap();

    remove_steering_rules(&nonempty).unwrap();

    assert_eq!(std::fs::read(&nonempty).unwrap(), b"operator rules\n");

    let empty = root.path().join("empty.md");
    let mut facts = Vec::new();
    crate::agents::recorded_lifecycle(root.path(), &mut facts, false, || {
        install_steering_rules(&empty)
    })
    .unwrap();

    crate::agents::recorded_lifecycle(root.path(), &mut facts, true, || {
        remove_steering_rules(&empty)
    })
    .unwrap();

    assert!(!empty.exists());
}

fn install_context(home: &Path, tracedecay_bin: &str) -> InstallContext {
    InstallContext {
        profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home),
        home: home.to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string(),
        project_root: None,
        dashboard: false,
    }
}

const CONTEXT_MCP: &[crate::agents::host_bundle::HostComponentV1] =
    &[crate::agents::host_bundle::HostComponentV1::ContextMcp];

/// Kiro's CLI refuses every `mcp` command while signed out. Registration edits
/// the documented `~/.kiro/settings/mcp.json` itself, so that CLI on `PATH`
/// is never run and the operator's peer server and formatting survive both
/// directions byte for byte.
#[cfg(unix)]
#[test]
fn global_registration_edits_mcp_json_without_running_a_signed_out_kiro_cli() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let ran = bin_dir.path().join("kiro-cli-ran");
    write_executable_script(
        &bin_dir.path().join("kiro-cli"),
        format!(
            "#!/bin/sh\ntouch '{}'\necho 'error: You are not logged in, please log in with kiro-cli login' >&2\nexit 1\n",
            ran.display()
        ),
    )
    .unwrap();
    let _path = tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(bin_dir.path());
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let original = "{\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"other\",\n      \"args\": []\n    }\n  }\n}\n";
    std::fs::write(&mcp_path, original).unwrap();

    KiroIntegration
        .activate_deployed_host_component_registration(
            CONTEXT_MCP,
            &install_context(home.path(), "/new/tracedecay"),
        )
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(&mcp_path).unwrap(),
        "{\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"other\",\n      \"args\": []\n    },\n    \"tracedecay\": {\n      \"args\": [\n        \"serve\"\n      ],\n      \"command\": \"/new/tracedecay\",\n      \"disabled\": false\n    }\n  }\n}\n"
    );
    assert!(KiroIntegration.has_tracedecay(
        home.path(),
        &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path())
    ));

    KiroIntegration
        .deactivate_deployed_host_component_registration(
            CONTEXT_MCP,
            &install_context(home.path(), "/new/tracedecay"),
        )
        .unwrap();

    assert_eq!(std::fs::read_to_string(&mcp_path).unwrap(), original);
    assert!(!ran.exists(), "the lifecycle ran kiro-cli");
}

/// With no `kiro-cli` anywhere, registration creates the documented user-level
/// file from nothing.
#[test]
fn global_registration_creates_mcp_json_without_any_kiro_cli() {
    let home = tempfile::tempdir().unwrap();
    let empty_path = tempfile::tempdir().unwrap();
    let _path = tracedecay_runtime_core::config::HostProgramSearchPathGuard::set(empty_path.path());

    KiroIntegration
        .activate_deployed_host_component_registration(
            CONTEXT_MCP,
            &install_context(home.path(), "/bin/tracedecay"),
        )
        .unwrap();

    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(mcp_config_path(home.path())).unwrap()).unwrap();
    assert_eq!(
        written,
        serde_json::json!({"mcpServers": {"tracedecay": {
            "command": "/bin/tracedecay", "args": ["serve"], "disabled": false
        }}})
    );
}

fn kiro_component_set() -> crate::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1 {
    crate::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin(
        crate::agents::host_bundle::HostKindV1::Kiro,
        &[crate::agents::host_bundle::HostComponentV1::ContextMcp],
        0,
        "/bin/tracedecay",
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .expect("the embedded Kiro component set must verify")
}

fn kiro_component_request(
    operation: crate::agents::host_bundle::HostBundleLifecycleOpV1,
    operation_id: [u8; 16],
) -> crate::agents::host_bundle::HostComponentSetExecutionRequestV1 {
    crate::agents::host_bundle::HostComponentSetExecutionRequestV1 {
        lifecycle: crate::agents::host_bundle::HostComponentSetLifecycleRequestV1 {
            operation,
            expected_host: crate::agents::host_bundle::HostKindV1::Kiro,
            expected_components: vec![crate::agents::host_bundle::HostComponentV1::ContextMcp],
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
            explicit_adoption: false,
        },
        operation_id,
    }
}

#[test]
fn rollback_refuses_a_foreign_registry_write_after_apply() {
    use crate::agents::host_bundle::{
        HostBundleLifecycleOpV1, HostBundleWriterV1, HostComponentSetRegistrationV1,
        HostComponentSetTransactionV1,
    };

    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();

    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let original = br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#;
    std::fs::write(&mcp_path, original).unwrap();

    let component_set = kiro_component_set();
    let request = kiro_component_request(HostBundleLifecycleOpV1::Install, [32; 16]);
    let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
        &tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
        "kiro",
        home.path(),
        request.lifecycle.operation,
        "/bin/tracedecay".to_string(),
    )
    .unwrap();
    let mut writer =
        HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path()).unwrap();
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        )
        .unwrap();

    registration
        .confirm_preview(&component_set.component_set, &request, &preview)
        .unwrap();
    registration
        .declare_artifact_writes(&component_set.component_set, &request, &[])
        .unwrap();
    registration
        .preflight(&component_set.component_set, &request)
        .unwrap();
    registration
        .stage(&component_set.component_set, &request)
        .unwrap();
    registration
        .apply(&component_set.component_set, &request)
        .expect("the registration edit must apply");

    let foreign = br#"{"mcpServers":{"foreign":{"command":"operator"}}}"#;
    std::fs::write(&mcp_path, foreign).unwrap();
    let error = registration
        .rollback(&component_set.component_set, &request)
        .expect_err("rollback must refuse to overwrite a later foreign edit");
    assert!(
        matches!(
            error,
            crate::agents::host_bundle::HostBundleError::StalePreview(_)
        ),
        "foreign drift must be typed stale preview: {error}"
    );
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        foreign,
        "a refused rollback must leave the later foreign bytes untouched"
    );
}

#[test]
fn detected_kiro_without_a_tracedecay_server_is_a_single_optional_warning() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"operator":{"command":"other","args":[]}}}"#,
    )
    .unwrap();

    let mut counters = DoctorCounters::new();
    KiroIntegration.healthcheck(
        &mut counters,
        &HealthcheckContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 0);
    assert_eq!(counters.warnings, 1);
}

#[test]
fn malformed_kiro_mcp_config_remains_a_doctor_failure() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(&mcp_path, "{ not valid JSON").unwrap();

    let mut counters = DoctorCounters::new();
    KiroIntegration.healthcheck(
        &mut counters,
        &HealthcheckContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 1);
    assert_eq!(counters.warnings, 0);
}

#[test]
fn an_empty_kiro_mcp_config_is_a_doctor_failure() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(&mcp_path, b"").unwrap();

    let error = match kiro_doctor_installation_state(home.path()) {
        Err(error) => error,
        Ok(_) => panic!("an existing empty Kiro MCP config is malformed persisted state"),
    };
    let TraceDecayError::Config { message } = error else {
        panic!("an empty Kiro MCP config must not become TraceDecayAbsent: {error}");
    };
    assert!(
        message.contains("empty"),
        "the persisted-config failure must explain the malformed empty file: {message}"
    );

    let mut counters = DoctorCounters::new();
    KiroIntegration.healthcheck(
        &mut counters,
        &HealthcheckContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 1);
    assert_eq!(counters.warnings, 0);
}

#[test]
fn lifecycle_leaves_an_ambient_kiro_home_sentinel_untouched() {
    const AMBIENT_CHILD: &str = "TRACEDECAY_TEST_AMBIENT_KIRO_HOME_CHILD";
    let Some(ambient) = std::env::var_os(AMBIENT_CHILD).map(PathBuf::from) else {
        // The ambient `KIRO_HOME` is process environment; a child isolates
        // it from concurrently running tests.
        let ambient = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agents::kiro::tests::lifecycle_leaves_an_ambient_kiro_home_sentinel_untouched",
            ])
            .env(AMBIENT_CHILD, ambient.path())
            .env("KIRO_HOME", ambient.path())
            .status()
            .unwrap();
        assert!(status.success(), "the ambient KIRO_HOME child failed");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let ambient_mcp = ambient.join("settings/mcp.json");
    std::fs::create_dir_all(ambient_mcp.parent().unwrap()).unwrap();
    let sentinel = br#"{"mcpServers":{"operator-sentinel":{"command":"keep"}}}"#;
    std::fs::write(&ambient_mcp, sentinel).unwrap();
    KiroIntegration
        .activate_deployed_host_component_registration(
            CONTEXT_MCP,
            &install_context(home.path(), "/bin/tracedecay"),
        )
        .expect("the admitted profile must be the one registered");
    assert_eq!(std::fs::read(&ambient_mcp).unwrap(), sentinel);
    assert!(mcp_config_path(home.path()).is_file());
}

/// Kiro's documented hook entry schema is `command` plus an optional
/// `matcher`, an undocumented field (the old `timeout_ms`) is schema noise
/// Kiro never reads and must not be written.
#[test]
fn managed_agent_hook_entries_carry_only_documented_fields() {
    let hooks = managed_agent_hooks("/bin/tracedecay");
    let events = hooks.as_object().expect("hooks is an object");
    assert!(
        !events.is_empty(),
        "at least one managed hook is registered"
    );
    for (event, entries) in events {
        for entry in entries.as_array().expect("event entries are an array") {
            let entry = entry.as_object().expect("hook entry is an object");
            assert!(
                entry.contains_key("command"),
                "hook entry for {event} must carry a command"
            );
            for key in entry.keys() {
                assert!(
                    matches!(key.as_str(), "command" | "matcher"),
                    "hook entry for {event} carries undocumented field {key}"
                );
            }
        }
    }
}

/// Kiro custom agents do not auto-include steering, so the managed agent's
/// `resources` must reference the global steering file explicitly.
#[test]
fn managed_agent_resources_reference_the_steering_file() {
    let project = tempfile::tempdir().unwrap();
    let steering = project.path().join(".kiro/steering/tracedecay.md");
    let config = managed_agent_config("/bin/tracedecay", &steering, None);
    let expected = file_resource_uri(&steering);
    assert!(
        config["resources"]
            .as_array()
            .expect("agent config has resources")
            .iter()
            .any(|value| value.as_str() == Some(expected.as_str())),
        "managed agent must load its steering as an explicit resource"
    );
}
