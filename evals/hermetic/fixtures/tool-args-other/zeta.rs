/// Lives in the *second* registered project so project-selector scenarios
/// have a symbol that does not exist in the primary fixture.
fn zeta() -> u8 {
    7
}

fn zeta_caller() -> u8 {
    zeta() + 1
}
