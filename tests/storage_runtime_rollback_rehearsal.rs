#[path = "fixtures/storage_runtime/source_ast.rs"]
mod source_ast;

use serde::Deserialize;

use source_ast::{RustAst, first_call_line, has_call_suffix, has_path_suffix};

const ROLLBACK_REHEARSAL: &str = include_str!("fixtures/storage_runtime/rollback_rehearsal.json");

#[derive(Debug, Deserialize)]
struct RollbackFixture {
    orchestrator: String,
    driver: String,
    validation: String,
    maintenance: String,
    registry: String,
    forbidden_writable_fallbacks: Vec<String>,
}

#[test]
fn restore_aborts_private_staging_before_publication() {
    let fixture: RollbackFixture =
        serde_json::from_str(ROLLBACK_REHEARSAL).expect("decode rollback fixture");
    let orchestrator = RustAst::parse(&fixture.orchestrator);
    let calls = orchestrator.method_calls("BackupRestoreOrchestrator", "restore");
    for required in [
        ".allocate_restore_target",
        ".stage_and_verify_restore",
        ".abandon_restore",
        ".publish_restore",
    ] {
        assert!(
            has_call_suffix(&calls, required),
            "restore rehearsal omitted {required}"
        );
    }
    assert!(
        first_call_line(&calls, ".stage_and_verify_restore")
            < first_call_line(&calls, ".publish_restore"),
        "restore publication must follow complete staging verification"
    );

    let verification =
        orchestrator.method_calls("BackupRestoreOrchestrator", "stage_and_verify_restore");
    for required in [
        ".verify_restore",
        "verify_restore_evidence",
        "check_cancelled",
    ] {
        assert!(
            has_call_suffix(&verification, required),
            "pre-publication verification omitted {required}"
        );
    }
}

#[test]
fn post_publication_restore_requires_newer_incarnation_and_epoch() {
    let fixture: RollbackFixture =
        serde_json::from_str(ROLLBACK_REHEARSAL).expect("decode rollback fixture");
    let validation = RustAst::parse(&fixture.validation);
    let comparisons = validation.function_binary_expressions("restored_watermarks");
    assert!(
        comparisons
            .iter()
            .any(|expression| expression.contains("replacement.incarnation<=watermark.incarnation")),
        "restore validation must reject a non-increasing incarnation"
    );
    assert!(
        comparisons.iter().any(|expression| {
            expression.contains("replacement.authority_epoch<=watermark.authority_epoch")
        }),
        "restore validation must reject a non-increasing authority epoch"
    );

    let driver = RustAst::parse(&fixture.driver);
    let calls = driver.method_calls("SqliteOnlineBackupDriver", "publish_restore");
    for required in [
        "validate_replacement_bindings",
        ".root.publish_restore",
        ".authority.publish_restored",
    ] {
        assert!(
            has_call_suffix(&calls, required),
            "post-publication restore omitted {required}"
        );
    }
    assert!(
        first_call_line(&calls, "validate_replacement_bindings")
            < first_call_line(&calls, ".root.publish_restore")
            && first_call_line(&calls, ".root.publish_restore")
                < first_call_line(&calls, ".authority.publish_restored"),
        "higher-fence validation, atomic filesystem publication, and canonical publication are out of order"
    );
}

#[test]
fn restore_has_no_old_writable_fallback_path() {
    let fixture: RollbackFixture =
        serde_json::from_str(ROLLBACK_REHEARSAL).expect("decode rollback fixture");
    let orchestrator = RustAst::parse(&fixture.orchestrator);
    let driver = RustAst::parse(&fixture.driver);
    let mut calls = orchestrator.method_calls("BackupRestoreOrchestrator", "restore");
    calls.extend(driver.method_calls("SqliteOnlineBackupDriver", "publish_restore"));

    let violations = calls
        .iter()
        .filter(|call| {
            fixture
                .forbidden_writable_fallbacks
                .iter()
                .any(|forbidden| call.callee.ends_with(forbidden))
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "restore publication must preserve the prior database only as recovery input: {violations:?}"
    );
}

#[test]
fn stale_authority_is_fenced_at_registry_and_adapter_boundaries() {
    let fixture: RollbackFixture =
        serde_json::from_str(ROLLBACK_REHEARSAL).expect("decode rollback fixture");
    let registry = RustAst::parse(&fixture.registry);
    assert!(
        has_path_suffix(
            &registry.method_paths("StoreRuntimeRegistry", "lookup"),
            "StoreRuntimeLookup::Fenced"
        ),
        "registry lookup must return a typed stale-authority fence"
    );

    let maintenance = RustAst::parse(&fixture.maintenance);
    assert!(
        has_path_suffix(
            &maintenance.function_paths("require_publication"),
            "MaintenanceError::Fenced"
        ),
        "maintenance must fence stale publication capabilities"
    );
}
