use super::*;
use crate::agents::host_bundle_v2::HostBundleError;

/// Gemini's deployed artifacts are an extension *source*: the host carries
/// nothing until `gemini extensions install` adopts them. Classifying the
/// set as artifact-only would let a lifecycle report an activation that
/// never happened, and would let artifact backup/restore claim it can
/// reverse a host registration it never snapshots.
#[test]
fn gemini_component_sets_are_not_artifact_only_lifecycles() {
    let home = tempfile::tempdir().expect("home");
    let lifecycle_root = tempfile::tempdir().expect("lifecycle root");
    let authority = CatalogHostComponentRegistrationAuthority::new(
        "gemini",
        home.path(),
        lifecycle_root.path(),
        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
    )
    .expect("catalog registration authority");
    let component_set =
        crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
            crate::agents::host_bundle_v2::HostKindV1::Gemini,
            0,
        )
        .expect("Gemini has a compiled default set");

    assert!(
        !authority.supports_artifact_only_backup_restore(&component_set.component_set),
        "the Gemini lifecycle drives `gemini extensions install`, so its deployed \
         bytes are not the whole lifecycle"
    );
    // Control: Cursor's component set really is fully represented by its
    // managed artifacts, so the assertion above is about Gemini's
    // classification and not about a predicate that always answers false.
    let cursor = CatalogHostComponentRegistrationAuthority::new(
        "cursor",
        home.path(),
        lifecycle_root.path(),
        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
    )
    .expect("catalog registration authority");
    let cursor_set =
        crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
            crate::agents::host_bundle_v2::HostKindV1::CursorDesktop,
            0,
        )
        .expect("Cursor has a compiled default set");
    assert!(cursor.supports_artifact_only_backup_restore(&cursor_set.component_set));
}

/// Copilot's deployed artifact is a receipt-owned descriptor; the host
/// carries nothing until `copilot mcp add` writes its own registry.
/// Classifying the set as artifact-only would let artifact backup/restore
/// claim it can reverse a host registration it never snapshots — the same
/// truthfulness violation Gemini's case above pins.
#[test]
fn copilot_component_sets_are_not_artifact_only_lifecycles() {
    let home = tempfile::tempdir().expect("home");
    let lifecycle_root = tempfile::tempdir().expect("lifecycle root");
    let authority = CatalogHostComponentRegistrationAuthority::new(
        "copilot",
        home.path(),
        lifecycle_root.path(),
        crate::agents::host_bundle_v2::HostBundleLifecycleOpV1::Install,
    )
    .expect("catalog registration authority");
    let component_set =
        crate::agents::host_bundle_registry::verified_embedded_default_host_component_set(
            crate::agents::host_bundle_v2::HostKindV1::Copilot,
            0,
        )
        .expect("Copilot has a compiled default set");

    assert!(
        !authority.supports_artifact_only_backup_restore(&component_set.component_set),
        "the Copilot lifecycle drives `copilot mcp add`, so its deployed \
         bytes are not the whole lifecycle"
    );
}

#[test]
fn rollback_identity_rejects_other_home_profile_and_integration() {
    let home = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let identity = RegistrationBackupIdentityV1::new("codex", home.path(), profile.path()).unwrap();

    assert_eq!(
        identity.validate("codex", home.path(), profile.path()),
        Ok(())
    );
    for result in [
        identity.validate("codex", other.path(), profile.path()),
        identity.validate("codex", home.path(), other.path()),
        identity.validate("cursor", home.path(), profile.path()),
    ] {
        assert_eq!(result, Err(HostBundleError::WrongTarget));
    }
    let mut future_identity = identity;
    future_identity.schema_version = REGISTRATION_BACKUP_IDENTITY_SCHEMA_VERSION + 1;
    assert_eq!(
        future_identity.validate("codex", home.path(), profile.path()),
        Err(HostBundleError::UnsupportedRecoveryFormat)
    );
}
