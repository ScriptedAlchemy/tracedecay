#[test]
fn worker_response_deserializes() {
    #[derive(serde::Deserialize)]
    struct WorkerResponse {
        total: u64,
    }
    let json = r#"{"total": 2847561}"#;
    let parsed: WorkerResponse = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.total, 2847561);
}

#[test]
fn increment_request_body_format() {
    let amount: u64 = 4823;
    let body = serde_json::json!({ "amount": amount });
    assert_eq!(body["amount"], 4823);
}

#[test]
fn is_newer_version_stable_comparisons() {
    assert!(tracedecay::cloud::is_newer_version("2.3.0", "2.4.0"));
    assert!(tracedecay::cloud::is_newer_version("2.4.0", "3.0.0"));
    assert!(!tracedecay::cloud::is_newer_version("2.4.0", "2.4.0"));
    assert!(!tracedecay::cloud::is_newer_version("2.4.0", "2.3.0"));
}

#[test]
fn is_newer_version_beta_comparisons() {
    // Cross-channel comparisons always return false (separate update channels)
    assert!(!tracedecay::cloud::is_newer_version(
        "2.5.0-beta.1",
        "2.5.0"
    ));
    assert!(!tracedecay::cloud::is_newer_version(
        "2.5.0",
        "2.5.0-beta.1"
    ));
    assert!(!tracedecay::cloud::is_newer_version(
        "2.5.0-beta.1",
        "2.6.0"
    ));
    assert!(!tracedecay::cloud::is_newer_version(
        "2.6.0",
        "2.5.0-beta.1"
    ));
    // Same-channel beta comparisons still work
    assert!(tracedecay::cloud::is_newer_version(
        "2.5.0-beta.1",
        "2.5.0-beta.2"
    ));
    assert!(!tracedecay::cloud::is_newer_version(
        "2.5.0-beta.2",
        "2.5.0-beta.1"
    ));
    assert!(tracedecay::cloud::is_newer_version(
        "2.5.0-beta.1",
        "2.6.0-beta.1"
    ));
}

#[test]
fn is_newer_minor_version_ignores_patch_bumps() {
    // Patch-only bump → not a minor update
    assert!(!tracedecay::cloud::is_newer_minor_version("3.2.0", "3.2.1"));
    assert!(!tracedecay::cloud::is_newer_minor_version("3.2.1", "3.2.2"));
    // Minor bump → yes
    assert!(tracedecay::cloud::is_newer_minor_version("3.2.1", "3.3.0"));
    assert!(tracedecay::cloud::is_newer_minor_version("3.2.0", "3.3.0"));
    // Major bump → yes
    assert!(tracedecay::cloud::is_newer_minor_version("3.2.1", "4.0.0"));
    // Same version → no
    assert!(!tracedecay::cloud::is_newer_minor_version("3.2.0", "3.2.0"));
    // Older version → no
    assert!(!tracedecay::cloud::is_newer_minor_version("3.3.0", "3.2.1"));
}

#[test]
fn is_newer_minor_version_beta() {
    // Cross-channel: always false regardless of version distance
    assert!(!tracedecay::cloud::is_newer_minor_version(
        "3.2.0-beta.1",
        "3.2.0"
    ));
    assert!(!tracedecay::cloud::is_newer_minor_version(
        "3.2.0-beta.1",
        "3.3.0"
    ));
    assert!(!tracedecay::cloud::is_newer_minor_version(
        "3.2.0",
        "3.3.0-beta.1"
    ));
    // Same-channel beta: minor bump detected
    assert!(tracedecay::cloud::is_newer_minor_version(
        "3.2.0-beta.1",
        "3.3.0-beta.1"
    ));
    assert!(!tracedecay::cloud::is_newer_minor_version(
        "3.2.0-beta.1",
        "3.2.0-beta.2"
    ));
}

#[test]
fn is_newer_version_same_version() {
    assert!(!tracedecay::cloud::is_newer_version("3.2.1", "3.2.1"));
}

#[test]
fn is_newer_version_all_components() {
    // Latest is newer in each component
    assert!(tracedecay::cloud::is_newer_version("3.2.1", "3.3.0"));
    assert!(tracedecay::cloud::is_newer_version("3.2.1", "4.0.0"));
    assert!(tracedecay::cloud::is_newer_version("3.2.1", "3.2.2"));
    // Latest is older
    assert!(!tracedecay::cloud::is_newer_version("3.3.0", "3.2.1"));
}

#[test]
fn is_newer_version_cross_channel_blocked() {
    // Beta vs stable (cross-channel = false)
    assert!(!tracedecay::cloud::is_newer_version(
        "3.2.1",
        "3.3.0-beta.1"
    ));
    assert!(!tracedecay::cloud::is_newer_version(
        "3.2.1-beta.1",
        "3.3.0"
    ));
}

#[test]
fn is_newer_version_beta_ordering() {
    assert!(tracedecay::cloud::is_newer_version(
        "3.2.1-beta.1",
        "3.2.1-beta.2"
    ));
    assert!(!tracedecay::cloud::is_newer_version(
        "3.2.1-beta.2",
        "3.2.1-beta.1"
    ));
}

#[test]
fn is_newer_version_invalid_versions() {
    assert!(!tracedecay::cloud::is_newer_version("invalid", "3.2.1"));
    assert!(!tracedecay::cloud::is_newer_version("3.2.1", "invalid"));
}

#[test]
fn is_newer_minor_version_patch_only() {
    // Patch-only bump returns false
    assert!(!tracedecay::cloud::is_newer_minor_version("3.2.1", "3.2.2"));
}

#[test]
fn is_newer_minor_version_minor_bump() {
    assert!(tracedecay::cloud::is_newer_minor_version("3.2.1", "3.3.0"));
}

#[test]
fn is_newer_minor_version_major_bump() {
    assert!(tracedecay::cloud::is_newer_minor_version("3.2.1", "4.0.0"));
}

#[test]
fn is_newer_minor_version_same() {
    assert!(!tracedecay::cloud::is_newer_minor_version("3.2.1", "3.2.1"));
}

#[test]
fn is_beta_returns_bool() {
    // Just verify it returns a bool and doesn't panic
    let _ = tracedecay::cloud::is_beta();
}

#[test]
fn upgrade_command_always_suggests_tracedecay_upgrade() {
    use tracedecay::cloud::{upgrade_command, InstallMethod};
    for method in &[
        InstallMethod::Cargo,
        InstallMethod::Brew,
        InstallMethod::Scoop,
        InstallMethod::Unknown,
    ] {
        assert_eq!(upgrade_command(method), "tracedecay upgrade");
    }
}

#[test]
fn detect_install_method_no_panic() {
    // Just verify it returns without panic
    let _ = tracedecay::cloud::detect_install_method();
}
