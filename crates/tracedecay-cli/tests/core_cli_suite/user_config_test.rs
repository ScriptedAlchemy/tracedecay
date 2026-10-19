use tracedecay_session_memory::user_config::UserConfig;

#[test]
fn missing_fields_use_defaults() {
    let toml_str = "last_version_check_at = 5\n";
    let parsed: UserConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(parsed.last_version_check_at, 5);
    assert_eq!(parsed.last_version_warning_at, 0);
    assert!(parsed.installed_agents.is_empty());
}
