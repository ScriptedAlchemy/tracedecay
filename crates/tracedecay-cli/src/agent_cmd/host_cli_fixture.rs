//! Test-only installer for the compiled host-CLI fixture.
#[path = "../../build-support/provision_host_cli_fixture.rs"]
mod provision_host_cli_fixture;

pub(super) use provision_host_cli_fixture::{
    install_compiled_host_cli_fixture, looks_like_native_executable,
};
