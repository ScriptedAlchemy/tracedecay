use tracedecay_session_memory::user_config::UserConfig;

#[test]
fn missing_fields_use_defaults() {
    let toml_str = "upload_enabled = false\n";
    let parsed: UserConfig = toml::from_str(toml_str).unwrap();
    assert!(!parsed.upload_enabled);
    assert_eq!(parsed.pending_upload, 0);
    assert_eq!(parsed.last_upload_at, 0);
}

#[test]
fn unknown_fields_ignored() {
    let toml_str = "upload_enabled = true\nsome_future_field = 42\n";
    let parsed: UserConfig = toml::from_str(toml_str).unwrap();
    assert!(parsed.upload_enabled);
}

#[test]
fn old_daemon_debounce_field_still_deserializes() {
    let toml = r#"daemon_debounce = "30s""#;
    let cfg: tracedecay_session_memory::user_config::UserConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.watcher_debounce, "30s");
}

#[test]
fn new_watcher_debounce_field_works() {
    let toml = r#"watcher_debounce = "45s""#;
    let cfg: tracedecay_session_memory::user_config::UserConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.watcher_debounce, "45s");
}
