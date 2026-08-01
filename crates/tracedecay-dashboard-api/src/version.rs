use std::sync::OnceLock;

type BuildVersion = fn() -> &'static str;

static BUILD_VERSION: OnceLock<BuildVersion> = OnceLock::new();

/// Installs the owning product binary's build identity.
///
/// The dashboard crate's package version is an implementation detail; product
/// surfaces must report the version baked into the root executable.
pub fn install_build_version(build_version: fn() -> &'static str) {
    let _ = BUILD_VERSION.set(build_version);
}

pub fn build_version() -> &'static str {
    BUILD_VERSION
        .get()
        .map_or(env!("CARGO_PKG_VERSION"), |build_version| build_version())
}
