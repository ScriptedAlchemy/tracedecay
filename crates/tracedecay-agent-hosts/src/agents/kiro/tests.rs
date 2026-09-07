//! Host-CLI-driven Kiro MCP registry lifecycle.
//!
//! Kiro owns `~/.kiro/settings/mcp.json` through `kiro-cli mcp`, so TraceDecay
//! drives that CLI rather than merging the file. These tests stand a fake
//! `kiro-cli` in an isolated HOME, assert the exact argv TraceDecay issues,
//! and assert that an absent binary refuses instead of falling back to config
//! surgery. The fake host preserves a known peer server so the lifecycle's
//! preservation guard is exercised on both add and remove.
//!
//! The fake CLI also emulates the registry's own effect (add writes the
//! server entry, remove drops it) so removal can be shown to reverse
//! installation rather than merely being spelled correctly.

use super::*;

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

/// The heading shipped releases through v0.1.0-beta.37 wrote as the block's
/// identity, closed by the end sentinel those releases already emitted.
const SHIPPED_HEADING: &str = "## TraceDecay: mandatory tool routing";
/// The heading the release before that used for the same block.
const OLDEST_HEADING: &str = "## Prefer tracedecay MCP tools";

fn shipped_block(heading: &str, body: &str) -> String {
    format!("{heading}\n\n{body}\n\n{}", STEERING_SENTINELS.end)
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
        (
            "shipped-heading refresh",
            Some(
                format!(
                    "operator rules\n\n{}\n",
                    shipped_block(SHIPPED_HEADING, "stale rules")
                )
                .into_bytes(),
            ),
        ),
        (
            "heading fallback",
            Some(format!("operator rules\n\n{SHIPPED_HEADING}\n\nstale rules\n").into_bytes()),
        ),
        ("existing append", Some(b"operator rules\n".to_vec())),
        ("missing create", None),
    ]
}

#[test]
fn every_historical_steering_shape_converges_on_update_and_preserves_peers() {
    let block = steering_block_text();
    let historical_shapes = [
        (
            "shipped heading with end sentinel",
            shipped_block(
                SHIPPED_HEADING,
                "You MUST use it. 1% chance. No rationalizing.",
            ),
        ),
        (
            "oldest heading with end sentinel",
            shipped_block(
                OLDEST_HEADING,
                "Before reading source files, use tracedecay.",
            ),
        ),
        (
            "oldest heading without end sentinel",
            format!("{OLDEST_HEADING}\n\nBefore reading source files, use tracedecay."),
        ),
    ];
    for (shape, stale) in historical_shapes {
        let root = tempfile::tempdir().unwrap();
        let steering = root.path().join("tracedecay.md");
        let original =
            format!("# Team steering\n\nkeep me\n\n{stale}\n\n## Operator section\n\nand me\n");
        std::fs::write(&steering, &original).unwrap();

        install_steering_rules(&steering).unwrap();

        let updated = std::fs::read_to_string(&steering).unwrap();
        assert_eq!(
            updated,
            format!("# Team steering\n\nkeep me\n\n{block}\n\n## Operator section\n\nand me\n"),
            "{shape}: update must replace the whole owned block in place and keep both peers"
        );
        assert!(
            !updated.contains("MUST") && !updated.contains("rationaliz"),
            "{shape}: no historical forcing may survive the migration"
        );

        install_steering_rules(&steering).unwrap();
        assert_eq!(
            std::fs::read_to_string(&steering).unwrap(),
            updated,
            "{shape}: a current reinstall is idempotent"
        );
    }
}

#[test]
fn every_historical_steering_shape_is_removed_on_uninstall() {
    for stale in [
        shipped_block(SHIPPED_HEADING, "stale mandate"),
        shipped_block(OLDEST_HEADING, "stale mandate"),
        format!("{OLDEST_HEADING}\n\nstale mandate"),
        steering_block_text(),
    ] {
        let root = tempfile::tempdir().unwrap();
        let steering = root.path().join("tracedecay.md");
        std::fs::write(
            &steering,
            format!("keep me\n\n{stale}\n\n## Operator section\n\nand me\n"),
        )
        .unwrap();

        remove_steering_rules(&steering).unwrap();

        assert_eq!(
            std::fs::read_to_string(&steering).unwrap(),
            "keep me\n\n## Operator section\n\nand me\n",
            "uninstall must remove the owned block and only that block"
        );
    }
}

#[test]
fn duplicate_and_mixed_steering_blocks_converge_deterministically() {
    let block = steering_block_text();
    let mixed = format!(
        "keep me\n\n{}\n\n## Operator section\n\nand me\n\n{}\n\n{block}\n\ntail peer\n",
        shipped_block(SHIPPED_HEADING, "stale mandate"),
        shipped_block(OLDEST_HEADING, "older mandate"),
    );
    let root = tempfile::tempdir().unwrap();
    let steering = root.path().join("tracedecay.md");
    std::fs::write(&steering, &mixed).unwrap();

    install_steering_rules(&steering).unwrap();

    let converged = std::fs::read_to_string(&steering).unwrap();
    assert_eq!(
        converged,
        format!("keep me\n\n{block}\n\n## Operator section\n\nand me\n\ntail peer\n"),
        "mixed markers must collapse onto one current block at the first owned position"
    );
    assert_eq!(owned_steering_ranges(&converged).len(), 1);

    std::fs::write(&steering, &mixed).unwrap();
    remove_steering_rules(&steering).unwrap();
    assert_eq!(
        std::fs::read_to_string(&steering).unwrap(),
        "keep me\n\n## Operator section\n\nand me\n\ntail peer\n",
        "uninstall must remove every owned block, historical and current"
    );
}

#[test]
fn steering_doctor_judges_sentinels_and_bytes_not_prose() {
    fn doctor(home: &Path) -> DoctorCounters {
        let mut counters = DoctorCounters::new();
        doctor_check_steering(&mut counters, home);
        counters
    }
    let home = tempfile::tempdir().unwrap();
    let steering = steering_path(home.path());
    std::fs::create_dir_all(steering.parent().unwrap()).unwrap();

    std::fs::write(
        &steering,
        "tracedecay MCP tools are great, use tracedecay_grep\n",
    )
    .unwrap();
    assert_eq!(
        doctor(home.path()).issues,
        1,
        "prose mentioning tracedecay without the ownership sentinel is not an install"
    );

    std::fs::write(&steering, shipped_block(SHIPPED_HEADING, "stale mandate")).unwrap();
    assert_eq!(
        doctor(home.path()).issues,
        1,
        "a shipped historical block is outdated until update converges it"
    );

    install_steering_rules(&steering).unwrap();
    let healthy = doctor(home.path());
    assert_eq!((healthy.issues, healthy.warnings), (0, 0));

    let edited = std::fs::read_to_string(&steering)
        .unwrap()
        .replace("tracedecay_grep", "rg");
    std::fs::write(&steering, edited).unwrap();
    assert_eq!(
        doctor(home.path()).issues,
        1,
        "an edited owned block is stale even though its sentinels are intact"
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
    install_steering_rules(&steering).unwrap();
    let original = std::fs::read(&steering).unwrap();
    let blocked_intent_root = root.path().join("blocked-intent-root");
    std::fs::write(&blocked_intent_root, b"not a directory").unwrap();

    let error = crate::agents::with_host_config_write_intents(blocked_intent_root, || {
        remove_steering_rules(&steering)
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
    install_steering_rules(&empty).unwrap();

    remove_steering_rules(&empty).unwrap();

    assert!(!empty.exists());
}

/// Install a fake `kiro-cli` that appends each invocation's argv to `log` and
/// then performs `body`.
#[cfg(unix)]
fn fake_kiro_cli(bin: &Path, log: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n{body}\n",
        log = shell_single_quote(&log.to_string_lossy()),
    );
    std::fs::write(bin, script).unwrap();
    let mut permissions = std::fs::metadata(bin).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(bin, permissions).unwrap();
}

/// Body for a fake `kiro-cli` that emulates the registry's own writes, so a
/// test can observe that TraceDecay's removal really reverses its install.
#[cfg(unix)]
const FAKE_REGISTRY_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    [ "${11-}" = "--force" ] || { echo 'missing --force' >&2; exit 64; }
    command="$6"
    /bin/mkdir -p "$HOME/.kiro/settings"
    if [ -f "$HOME/.kiro/settings/mcp.json" ] && /usr/bin/grep -q '"other"' "$HOME/.kiro/settings/mcp.json"; then
      printf '{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"%s","args":["serve"],"disabled":false}}}\n' "$command" > "$HOME/.kiro/settings/mcp.json"
    else
      printf '{"mcpServers":{"tracedecay":{"command":"%s","args":["serve"],"disabled":false}}}\n' "$command" > "$HOME/.kiro/settings/mcp.json"
    fi
    ;;
  "mcp remove")
    if [ -f "$HOME/.kiro/settings/mcp.json" ] && /usr/bin/grep -q '"other"' "$HOME/.kiro/settings/mcp.json"; then
      printf '%s\n' '{"mcpServers":{"other":{"command":"other","args":[]}}}' > "$HOME/.kiro/settings/mcp.json"
    else
      /bin/rm -f "$HOME/.kiro/settings/mcp.json"
    fi
    ;;
esac
exit 0"#;

/// A host command can mutate its registry and still return a failure (for
/// example after a post-write validation error). The component-set transaction
/// must restore the exact pre-command bytes in that case.
#[cfg(unix)]
const FAKE_FAIL_AFTER_WRITE_BODY: &str = r#"case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.kiro/settings"
    printf '%s\n' '{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"/bin/tracedecay","args":["serve"],"disabled":false}}}' > "$HOME/.kiro/settings/mcp.json"
    ;;
esac
echo 'Kiro rejected the registry after writing it' >&2
exit 7"#;

#[cfg(unix)]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(unix)]
fn recorded_invocations(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
#[test]
fn activation_drives_the_hosts_own_mcp_add_with_the_registered_server_contract() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);

    kiro_mcp_add_with(&kiro_cli, home.path(), "/bin/tracedecay")
        .expect("a clean host CLI run is a completed registration");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            "mcp add --name tracedecay --command /bin/tracedecay --args serve --scope global --force"
                .to_string(),
        ],
        "activation must add the server through Kiro's own registry, naming it and \
         passing each launch argument as Kiro's raw `--args` value at global scope"
    );
    assert!(
        mcp_config_path(home.path()).exists(),
        "the host's own registry write must be what lands the entry"
    );
}

#[cfg(unix)]
#[test]
fn removal_drives_the_hosts_own_mcp_remove_and_reverses_the_registration() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let mcp_path = mcp_config_path(home.path());
    assert!(!mcp_path.exists(), "precondition: nothing registered yet");

    kiro_mcp_add_with(&kiro_cli, home.path(), "/bin/tracedecay").unwrap();
    kiro_mcp_remove_with(&kiro_cli, home.path())
        .expect("a clean host CLI run is a completed removal");

    assert_eq!(
        recorded_invocations(&log),
        vec![
            "mcp add --name tracedecay --command /bin/tracedecay --args serve --scope global --force"
                .to_string(),
            "mcp remove --name tracedecay --scope global".to_string(),
        ],
        "removal must address the server by the same registry name the add used"
    );
    assert!(
        !mcp_path.exists(),
        "removal must fully reverse installation, leaving no tracedecay entry behind"
    );
}

#[cfg(unix)]
#[test]
fn add_and_remove_preserve_an_operator_owned_peer_server() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    std::fs::write(
        &mcp_path,
        br#"{"mcpServers":{"other":{"command":"other","args":[]},"tracedecay":{"command":"/old/tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();

    kiro_mcp_add_with(&kiro_cli, home.path(), "/new/tracedecay")
        .expect("host add must force-update tracedecay while preserving the peer");
    let added: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(added["mcpServers"]["other"]["command"], "other");
    assert_eq!(
        added["mcpServers"]["tracedecay"]["command"],
        "/new/tracedecay"
    );

    kiro_mcp_remove_with(&kiro_cli, home.path())
        .expect("host remove must preserve the peer while dropping tracedecay");
    let removed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(removed["mcpServers"]["other"]["command"], "other");
    assert!(removed["mcpServers"].get("tracedecay").is_none());
}

#[cfg(unix)]
fn kiro_component_set() -> crate::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1 {
    crate::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin(
        crate::agents::host_bundle_v2::HostKindV1::Kiro,
        &[crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp],
        0,
        "/bin/tracedecay",
        crate::agents::TEST_GENERATOR_COMMIT,
    )
    .expect("the embedded Kiro component set must verify")
}

#[cfg(unix)]
fn kiro_component_request(
    operation: crate::agents::host_bundle_v2::HostBundleLifecycleOpV1,
    operation_id: [u8; 16],
) -> crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1 {
    crate::agents::host_bundle_v2::HostComponentSetExecutionRequestV1 {
        lifecycle: crate::agents::host_bundle_v2::HostComponentSetLifecycleRequestV1 {
            operation,
            expected_host: crate::agents::host_bundle_v2::HostKindV1::Kiro,
            expected_components: vec![
                crate::agents::host_bundle_v2::HostBundleComponentV1::ContextMcp,
            ],
            explicit_confirmation: true,
            hermes_profile_bindings: 0,
            explicit_adoption: false,
        },
        operation_id,
    }
}

#[cfg(unix)]
#[test]
fn failed_kiro_cli_effect_rolls_back_the_peer_containing_registry() {
    use crate::agents::host_bundle_v2::{
        HostBundleLifecycleOpV1, HostBundleWriterV1, HostComponentSetTransactionV1,
    };

    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let kiro_cli = bin_dir.path().join("kiro-cli");
    let log = bin_dir.path().join("invocations.log");
    fake_kiro_cli(&kiro_cli, &log, FAKE_FAIL_AFTER_WRITE_BODY);
    let _path = crate::config::AmbientPathGuard::set(bin_dir.path());

    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let original = br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#;
    std::fs::write(&mcp_path, original).unwrap();

    let component_set = kiro_component_set();
    let request = kiro_component_request(HostBundleLifecycleOpV1::Install, [31; 16]);
    let mut writer = HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path())
        .expect("host bundle writer must open for an isolated profile");
    let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
        "kiro",
        home.path(),
        lifecycle.path(),
        request.lifecycle.operation,
        "/bin/tracedecay".to_string(),
    )
    .unwrap();
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let preview = transaction
        .preview(
            &component_set.component_set,
            &request,
            &component_set,
            &mut registration,
        )
        .expect("the isolated peer-containing registry must preview");
    let _error = transaction
        .execute_confirmed(
            &component_set.component_set,
            &request,
            &preview,
            &component_set,
            &mut registration,
        )
        .expect_err("a failing native command must fail the lifecycle");
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        original,
        "registration rollback must restore the exact peer-containing document"
    );
}

#[cfg(unix)]
#[test]
fn rollback_refuses_a_foreign_registry_write_after_cli_apply() {
    use crate::agents::host_bundle_v2::{
        HostBundleLifecycleOpV1, HostBundleWriterV1, HostComponentSetRegistrationV1,
        HostComponentSetTransactionV1,
    };

    let home = tempfile::tempdir().unwrap();
    let lifecycle = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let kiro_cli = bin_dir.path().join("kiro-cli");
    let log = bin_dir.path().join("invocations.log");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let _path = crate::config::AmbientPathGuard::set(bin_dir.path());

    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let original = br#"{"mcpServers":{"other":{"command":"other","args":[]}}}"#;
    std::fs::write(&mcp_path, original).unwrap();

    let component_set = kiro_component_set();
    let request = kiro_component_request(HostBundleLifecycleOpV1::Install, [32; 16]);
    let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
        "kiro",
        home.path(),
        lifecycle.path(),
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
        .expect("the fake native add must apply");

    let foreign = br#"{"mcpServers":{"foreign":{"command":"operator"}}}"#;
    std::fs::write(&mcp_path, foreign).unwrap();
    let error = registration
        .rollback(&component_set.component_set, &request)
        .expect_err("rollback must refuse to overwrite a later foreign edit");
    assert!(
        matches!(
            error,
            crate::agents::host_bundle_v2::HostBundleError::StalePreview(_)
        ),
        "foreign drift must be typed stale preview: {error}"
    );
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        foreign,
        "a refused rollback must leave the later foreign bytes untouched"
    );
}

#[cfg(unix)]
#[test]
fn a_failing_kiro_registry_command_reports_the_hosts_own_diagnosis() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(
        &kiro_cli,
        &log,
        "echo 'mcp server tracedecay is not configured' >&2\nexit 7",
    );

    let error = kiro_mcp_remove_with(&kiro_cli, home.path())
        .expect_err("a non-zero host CLI exit must fail the lifecycle");

    let TraceDecayError::Config { message } = error else {
        panic!("a failed host command must surface as a config error");
    };
    assert!(
        message.contains("mcp server tracedecay is not configured")
            && message.contains("exit code 7"),
        "the host's own stderr and status must reach the operator: {message}"
    );
}

#[test]
fn a_missing_kiro_binary_refuses_instead_of_editing_host_owned_state() {
    let home = tempfile::tempdir().unwrap();
    let mcp_path = mcp_config_path(home.path());
    std::fs::create_dir_all(mcp_path.parent().unwrap()).unwrap();
    let operator_owned = br#"{"mcpServers":{"someone-elses":{"command":"other"}}}"#;
    std::fs::write(&mcp_path, operator_owned).unwrap();

    let error =
        crate::agents::host_cli::require_host_cli("kiro-cli-definitely-absent", KIRO_CLI_LIFECYCLE)
            .expect_err("an absent host binary is a hard requirement failure");

    let TraceDecayError::HostCliUnavailable { program, lifecycle } = error else {
        panic!("host CLI absence must surface as a typed requirement");
    };
    assert_eq!(program, "kiro-cli-definitely-absent");
    assert_eq!(lifecycle, KIRO_CLI_LIFECYCLE);
    assert_eq!(
        std::fs::read(&mcp_path).unwrap(),
        operator_owned,
        "a refused lifecycle must not have touched host-owned registry state"
    );
    assert!(
        !config_backup_path(&mcp_path).exists(),
        "a refused lifecycle must not have staged a backup of host-owned registry state"
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
            home: home.path().to_path_buf(),
            project_path: home.path().to_path_buf(),
        },
    );

    assert_eq!(counters.issues, 1);
    assert_eq!(counters.warnings, 0);
}

#[test]
fn an_ambient_kiro_home_never_redirects_an_admitted_profile() {
    struct AmbientKiroHomeGuard {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl AmbientKiroHomeGuard {
        fn set(value: &Path) -> Self {
            let lock = crate::config::lock_user_data_dir_test_env();
            let previous = std::env::var_os("KIRO_HOME");
            // SAFETY: the shared profile-discovery lock is held for the
            // guard's lifetime, so no sibling profile test observes this
            // temporary ambient value.
            unsafe {
                std::env::set_var("KIRO_HOME", value);
            }
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for AmbientKiroHomeGuard {
        fn drop(&mut self) {
            // SAFETY: see `AmbientKiroHomeGuard::set`.
            unsafe {
                match self.previous.take() {
                    Some(previous) => std::env::set_var("KIRO_HOME", previous),
                    None => std::env::remove_var("KIRO_HOME"),
                }
            }
        }
    }

    let home = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let _ambient = AmbientKiroHomeGuard::set(ambient.path());
    assert_eq!(
        mcp_config_path(home.path()),
        home.path().join(".kiro/settings/mcp.json")
    );
    assert_ne!(
        mcp_config_path(home.path()),
        ambient.path().join("settings/mcp.json")
    );
}

#[cfg(unix)]
#[test]
fn cli_lifecycle_leaves_an_ambient_kiro_home_sentinel_untouched() {
    let home = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let log = bin_dir.path().join("invocations.log");
    let kiro_cli = bin_dir.path().join("kiro-cli");
    fake_kiro_cli(&kiro_cli, &log, FAKE_REGISTRY_BODY);
    let ambient_mcp = ambient.path().join("settings/mcp.json");
    std::fs::create_dir_all(ambient_mcp.parent().unwrap()).unwrap();
    let sentinel = br#"{"mcpServers":{"operator-sentinel":{"command":"keep"}}}"#;
    std::fs::write(&ambient_mcp, sentinel).unwrap();
    let _ambient = {
        struct AmbientKiroHomeGuard {
            previous: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }

        impl AmbientKiroHomeGuard {
            fn set(value: &Path) -> Self {
                let lock = crate::config::lock_user_data_dir_test_env();
                let previous = std::env::var_os("KIRO_HOME");
                // SAFETY: the shared profile-discovery lock serializes this
                // process-global test environment mutation.
                unsafe { std::env::set_var("KIRO_HOME", value) };
                Self {
                    previous,
                    _lock: lock,
                }
            }
        }

        impl Drop for AmbientKiroHomeGuard {
            fn drop(&mut self) {
                // SAFETY: see `AmbientKiroHomeGuard::set`.
                unsafe {
                    match self.previous.take() {
                        Some(previous) => std::env::set_var("KIRO_HOME", previous),
                        None => std::env::remove_var("KIRO_HOME"),
                    }
                }
            }
        }

        AmbientKiroHomeGuard::set(ambient.path())
    };
    kiro_mcp_add_with(&kiro_cli, home.path(), "/bin/tracedecay")
        .expect("the admitted profile must drive the native CLI");
    assert_eq!(std::fs::read(&ambient_mcp).unwrap(), sentinel);
    assert!(mcp_config_path(home.path()).is_file());
}

#[test]
fn the_cli_raw_args_match_the_config_writers_launch_arguments() {
    let entry = mcp_server_entry("/bin/tracedecay");
    let expected = serde_json::to_value(MCP_SERVER_ARGS).unwrap();
    assert_eq!(
        &expected,
        entry.get("args").unwrap(),
        "the CLI-driven global registration's raw --args values and the workspace-local config \
         writer must launch the same server with the same arguments"
    );
}

/// Kiro's documented hook entry schema is `command` plus an optional
/// `matcher` — an undocumented field (the old `timeout_ms`) is schema noise
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
fn managed_agent_resources_reference_the_global_steering_file() {
    let home = tempfile::tempdir().unwrap();
    let steering = steering_path(home.path());
    let config = managed_agent_config("/bin/tracedecay", &steering, None);
    let expected = file_resource_uri(&steering);
    assert!(
        config["resources"]
            .as_array()
            .expect("agent config has resources")
            .iter()
            .any(|value| value.as_str() == Some(expected.as_str())),
        "managed agent must load global steering as an explicit resource"
    );
}
