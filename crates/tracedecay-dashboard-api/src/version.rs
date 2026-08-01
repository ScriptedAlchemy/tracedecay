pub const fn build_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
