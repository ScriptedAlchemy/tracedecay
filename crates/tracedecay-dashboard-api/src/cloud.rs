use std::time::Duration;

pub fn agent_with_timeout(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

pub fn is_beta() -> bool {
    env!("CARGO_PKG_VERSION").contains('-')
}
