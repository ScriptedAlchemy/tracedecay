/// Whether `release_version` names a prerelease (beta-channel) build.
///
/// Takes the product release version because `env!` expands against the
/// crate that writes it: evaluating `CARGO_PKG_VERSION` here would bake this
/// crate's own version and report "stable" for every beta product build.
/// Only the version core is inspected — semver build metadata after `+`
/// (source SHA, dirty marker) never selects a channel.
pub fn is_beta(release_version: &str) -> bool {
    release_version
        .split('+')
        .next()
        .is_some_and(|core| core.contains('-'))
}
