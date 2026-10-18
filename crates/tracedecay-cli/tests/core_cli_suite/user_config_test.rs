use tracedecay_session_memory::user_config::UserConfig;

#[test]
fn missing_fields_use_defaults() {
    let toml_str = "pending_upload = 5\n";
    let parsed: UserConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(parsed.pending_upload, 5);
    assert_eq!(parsed.last_upload_at, 0);
    assert!(parsed.installed_agents.is_empty());
}
