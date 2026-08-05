//! Hook memory-injection configuration gate.
//!
//! Hooks no longer query fact or LCM tools or persist their own recall/dedupe
//! sidecars. Authorized memory and session guidance is owned by daemon
//! admission and later delivery surfaces. This module remains only as the
//! registered host-integration configuration gate.

/// Whether daemon-owned memory guidance is enabled: the environment override
/// wins when set, otherwise the user configuration applies.
pub fn memory_injection_enabled() -> bool {
    injection_enabled_from(
        crate::config::brand_env("MEMORY_INJECTION").as_deref(),
        crate::user_config::UserConfig::load().memory_injection_enabled,
    )
}

fn injection_enabled_from(env_value: Option<&str>, config_flag: bool) -> bool {
    match env_value {
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => config_flag,
    }
}

#[cfg(test)]
mod tests {
    use super::injection_enabled_from;

    #[test]
    fn environment_override_precedes_the_configuration_gate() {
        assert!(!injection_enabled_from(Some("false"), true));
        assert!(injection_enabled_from(Some("true"), false));
        assert!(injection_enabled_from(None, true));
        assert!(!injection_enabled_from(None, false));
    }
}
