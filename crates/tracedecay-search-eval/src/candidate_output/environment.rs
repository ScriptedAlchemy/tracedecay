//! Process-local environment facts retained with a direct evaluator output.

pub(super) fn toolchain_fingerprint() -> String {
    format!(
        "rustc:{}",
        option_env!("RUSTC_COMMIT_HASH").unwrap_or("unknown")
    )
}

pub(super) fn hardware_fingerprint() -> String {
    std::env::consts::ARCH.to_owned()
}
