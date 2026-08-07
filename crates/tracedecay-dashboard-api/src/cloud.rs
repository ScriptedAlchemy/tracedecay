pub fn is_beta() -> bool {
    env!("CARGO_PKG_VERSION").contains('-')
}
