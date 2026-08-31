//! Process-wide product runtime: release version, exact source provenance,
//! and the embedded dashboard bundle.
//!
//! The composition library does not generate these values. The shipping
//! binary crate is the sole generator of the dashboard bundle and of the
//! commit identity it was compiled from, and it hands them to this crate
//! through one strict, validated, set-once registration at process start.
//! A build with no provenance no longer exists: the generating binary fails
//! to build without it, and a process that never registered answers every
//! read with the typed [`ProductRuntimeError::MissingProvider`] state.

use std::sync::OnceLock;

#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_api::StaticDashboardAsset;
use tracedecay_api::StaticDashboardAssets;

use crate::version::PACKAGE_VERSION;

/// Exact source identity of the checkout the running product was built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProductSourceProvenance {
    /// Full 40-character lowercase-hex commit SHA.
    pub full_sha: &'static str,
    /// Whether the worktree carried uncommitted or untracked changes.
    pub dirty: bool,
}

/// Everything the generating binary supplies to the composition library.
#[derive(Clone, Copy)]
pub struct ProductRuntimeProvider {
    /// Must equal this crate's `CARGO_PKG_VERSION`: the binary and the
    /// library it registers into are always built from one workspace.
    pub release_version: &'static str,
    pub source: ProductSourceProvenance,
    pub dashboard: StaticDashboardAssets,
}

/// Typed states of the product runtime registration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProductRuntimeError {
    #[error(
        "no product runtime provider is registered; the generating binary must register one at process start"
    )]
    MissingProvider,
    #[error(
        "a product runtime provider is already registered; registration is set-once per process"
    )]
    ConflictingProvider,
    #[error("product runtime provider rejected: {reason}")]
    InvalidProvider { reason: String },
}

impl From<ProductRuntimeError> for tracedecay_domain::errors::TraceDecayError {
    fn from(error: ProductRuntimeError) -> Self {
        Self::Config {
            message: error.to_string(),
        }
    }
}

/// A provider that passed registration validation, with the self-identifying
/// build version precomposed so every read is allocation-free.
pub struct RegisteredProductRuntime {
    provider: ProductRuntimeProvider,
    build_version: String,
}

impl RegisteredProductRuntime {
    pub fn provider(&self) -> &ProductRuntimeProvider {
        &self.provider
    }

    pub fn release_version(&self) -> &'static str {
        self.provider.release_version
    }

    pub fn source(&self) -> ProductSourceProvenance {
        self.provider.source
    }

    pub fn dashboard(&self) -> StaticDashboardAssets {
        self.provider.dashboard
    }

    /// `"{release_version}+{full_sha}"`, plus `".dirty"` when the source
    /// worktree was dirty. `SemVer` build metadata never changes precedence,
    /// so this always compares equal to [`PACKAGE_VERSION`] for ordering.
    pub fn build_version(&self) -> &str {
        &self.build_version
    }
}

/// Joins the released version with the build-metadata commit identity.
fn compose_build_version(release_version: &str, full_sha: &str, dirty: bool) -> String {
    let dirty = if dirty { ".dirty" } else { "" };
    format!("{release_version}+{full_sha}{dirty}")
}

fn is_full_lowercase_hex_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn validated(
    provider: ProductRuntimeProvider,
) -> Result<RegisteredProductRuntime, ProductRuntimeError> {
    let invalid = |reason: String| ProductRuntimeError::InvalidProvider { reason };
    if provider.release_version != PACKAGE_VERSION {
        return Err(invalid(format!(
            "release_version {:?} must equal the composition crate's package version {PACKAGE_VERSION:?}",
            provider.release_version
        )));
    }
    if !is_full_lowercase_hex_sha(provider.source.full_sha) {
        return Err(invalid(format!(
            "full_sha {:?} must be exactly 40 ASCII lowercase hex characters",
            provider.source.full_sha
        )));
    }
    if provider.dashboard.assets.is_empty() {
        return Err(invalid("dashboard bundle has no assets".to_owned()));
    }
    let mut seen_paths = std::collections::BTreeSet::new();
    for asset in provider.dashboard.assets {
        if asset.path.is_empty() {
            return Err(invalid(
                "dashboard bundle contains an asset with an empty path".to_owned(),
            ));
        }
        if !seen_paths.insert(asset.path) {
            return Err(invalid(format!(
                "dashboard bundle contains duplicate asset path {:?}",
                asset.path
            )));
        }
    }
    if !seen_paths.contains("index.html") {
        return Err(invalid(
            "dashboard bundle has no \"index.html\" asset".to_owned(),
        ));
    }
    if provider.dashboard.cache_tag.is_empty() {
        return Err(invalid("dashboard bundle cache_tag is empty".to_owned()));
    }
    let build_version = compose_build_version(
        provider.release_version,
        provider.source.full_sha,
        provider.source.dirty,
    );
    Ok(RegisteredProductRuntime {
        provider,
        build_version,
    })
}

/// Slot-parameterized core of [`register_product_runtime`], so unit tests
/// exercise registration against local slots with no global-state races.
fn register_in(
    slot: &OnceLock<RegisteredProductRuntime>,
    provider: ProductRuntimeProvider,
) -> Result<(), ProductRuntimeError> {
    let runtime = validated(provider)?;
    slot.set(runtime)
        .map_err(|_| ProductRuntimeError::ConflictingProvider)
}

/// Slot-parameterized core of [`product_runtime`].
fn runtime_in(
    slot: &OnceLock<RegisteredProductRuntime>,
) -> Result<&RegisteredProductRuntime, ProductRuntimeError> {
    slot.get().ok_or(ProductRuntimeError::MissingProvider)
}

static PRODUCT_RUNTIME: OnceLock<RegisteredProductRuntime> = OnceLock::new();

/// Registers the process's product runtime provider. Strictly set-once: a
/// second registration attempt answers [`ProductRuntimeError::ConflictingProvider`]
/// even when it carries an identical provider.
pub fn register_product_runtime(
    provider: ProductRuntimeProvider,
) -> Result<(), ProductRuntimeError> {
    register_in(&PRODUCT_RUNTIME, provider)
}

/// The registered product runtime, or the typed missing state when the
/// generating binary never registered one.
pub fn product_runtime() -> Result<&'static RegisteredProductRuntime, ProductRuntimeError> {
    runtime_in(&PRODUCT_RUNTIME)
}

/// Canonical fixture dashboard bundle for tests and integration harnesses.
#[cfg(any(test, feature = "test-helpers"))]
pub const FIXTURE_DASHBOARD_ASSETS: StaticDashboardAssets = StaticDashboardAssets {
    assets: &[
        StaticDashboardAsset {
            path: "index.html",
            contents: b"<html>TraceDecay fixture dashboard</html>",
            content_type: "text/html; charset=utf-8",
        },
        StaticDashboardAsset {
            path: "static/app.fixture.js",
            contents: b"console.log('tracedecay fixture bundle')",
            content_type: "application/javascript",
        },
    ],
    cache_tag: "fixture-bundle-1",
};

#[cfg(any(test, feature = "test-helpers"))]
const FIXTURE_FULL_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[cfg(any(test, feature = "test-helpers"))]
const FIXTURE_PROVIDER: ProductRuntimeProvider = ProductRuntimeProvider {
    release_version: PACKAGE_VERSION,
    source: ProductSourceProvenance {
        full_sha: FIXTURE_FULL_SHA,
        dirty: false,
    },
    dashboard: FIXTURE_DASHBOARD_ASSETS,
};

/// Get-or-init of the global slot with the canonical fixture provider.
///
/// Invariant: a test process only ever registers this fixture, never a real
/// provider, so every in-process read across a suite observes one identical
/// runtime regardless of test order. The fixture bypasses [`validated`] only
/// because it is a constant; `the_fixture_provider_passes_registration_validation`
/// pins that the constant stays valid.
#[cfg(any(test, feature = "test-helpers"))]
pub fn register_fixture_product_runtime() -> &'static RegisteredProductRuntime {
    PRODUCT_RUNTIME.get_or_init(|| RegisteredProductRuntime {
        provider: FIXTURE_PROVIDER,
        build_version: compose_build_version(PACKAGE_VERSION, FIXTURE_FULL_SHA, false),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use tracedecay_api::{StaticDashboardAsset, StaticDashboardAssets};

    use super::{
        FIXTURE_PROVIDER, ProductRuntimeError, ProductRuntimeProvider, ProductSourceProvenance,
        RegisteredProductRuntime, compose_build_version, register_fixture_product_runtime,
        register_in, runtime_in, validated,
    };
    use crate::version::PACKAGE_VERSION;

    const VALID_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    const VALID_ASSETS: StaticDashboardAssets = StaticDashboardAssets {
        assets: &[
            StaticDashboardAsset {
                path: "index.html",
                contents: b"<html>bundle</html>",
                content_type: "text/html; charset=utf-8",
            },
            StaticDashboardAsset {
                path: "static/app.0123abcd.js",
                contents: b"console.log('bundle')",
                content_type: "application/javascript",
            },
        ],
        cache_tag: "bundle-tag-1",
    };

    fn valid_provider() -> ProductRuntimeProvider {
        ProductRuntimeProvider {
            release_version: PACKAGE_VERSION,
            source: ProductSourceProvenance {
                full_sha: VALID_SHA,
                dirty: false,
            },
            dashboard: VALID_ASSETS,
        }
    }

    fn invalid_reason(result: Result<(), ProductRuntimeError>) -> String {
        match result {
            Err(ProductRuntimeError::InvalidProvider { reason }) => reason,
            other => panic!("expected InvalidProvider, got {other:?}"),
        }
    }

    #[test]
    fn a_read_before_registration_is_the_typed_missing_state() {
        let slot = OnceLock::new();
        assert_eq!(
            runtime_in(&slot).err(),
            Some(ProductRuntimeError::MissingProvider)
        );
    }

    #[test]
    fn a_valid_provider_registers_and_reads_back_exactly() {
        let slot = OnceLock::new();
        register_in(&slot, valid_provider()).expect("valid provider must register");
        let runtime = runtime_in(&slot).expect("registered runtime must read back");
        assert_eq!(runtime.release_version(), PACKAGE_VERSION);
        assert_eq!(runtime.source().full_sha, VALID_SHA);
        assert!(!runtime.source().dirty);
        assert_eq!(runtime.dashboard().cache_tag, VALID_ASSETS.cache_tag);
        assert_eq!(runtime.provider().dashboard.assets.len(), 2);
        assert_eq!(
            runtime.build_version(),
            format!("{PACKAGE_VERSION}+{VALID_SHA}")
        );
    }

    #[test]
    fn a_dirty_source_is_admitted_in_the_build_version() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.source.dirty = true;
        register_in(&slot, provider).expect("dirty provider must register");
        assert_eq!(
            runtime_in(&slot).expect("registered").build_version(),
            format!("{PACKAGE_VERSION}+{VALID_SHA}.dirty")
        );
    }

    #[test]
    fn a_second_registration_conflicts_even_when_identical() {
        let slot = OnceLock::new();
        register_in(&slot, valid_provider()).expect("first registration");
        assert_eq!(
            register_in(&slot, valid_provider()).err(),
            Some(ProductRuntimeError::ConflictingProvider)
        );
        // The first registration survives the rejected second attempt.
        assert_eq!(
            runtime_in(&slot)
                .expect("first registration retained")
                .source()
                .full_sha,
            VALID_SHA
        );
    }

    #[test]
    fn a_release_version_mismatch_is_rejected_with_both_versions_named() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.release_version = "0.0.0-not-this-crate";
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("0.0.0-not-this-crate"), "{reason}");
        assert!(reason.contains(PACKAGE_VERSION), "{reason}");
        assert_eq!(
            runtime_in(&slot).err(),
            Some(ProductRuntimeError::MissingProvider),
            "a rejected provider must not occupy the slot"
        );
    }

    #[test]
    fn a_short_sha_is_rejected() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.source.full_sha = "ab12cd34ef56";
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("40 ASCII lowercase hex"), "{reason}");
    }

    #[test]
    fn a_non_hex_or_uppercase_sha_is_rejected() {
        for sha in [
            "0123456789ABCDEF0123456789abcdef01234567",
            "0123456789abcdefg123456789abcdef01234567",
        ] {
            let slot = OnceLock::new();
            let mut provider = valid_provider();
            provider.source.full_sha = sha;
            let reason = invalid_reason(register_in(&slot, provider));
            assert!(reason.contains("40 ASCII lowercase hex"), "{sha}: {reason}");
        }
    }

    #[test]
    fn an_empty_dashboard_bundle_is_rejected() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.dashboard = StaticDashboardAssets {
            assets: &[],
            cache_tag: "bundle-tag-1",
        };
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("no assets"), "{reason}");
    }

    #[test]
    fn an_empty_asset_path_is_rejected() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.dashboard = StaticDashboardAssets {
            assets: &[
                StaticDashboardAsset {
                    path: "index.html",
                    contents: b"<html></html>",
                    content_type: "text/html; charset=utf-8",
                },
                StaticDashboardAsset {
                    path: "",
                    contents: b"",
                    content_type: "application/octet-stream",
                },
            ],
            cache_tag: "bundle-tag-1",
        };
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("empty path"), "{reason}");
    }

    #[test]
    fn a_duplicate_asset_path_is_rejected_and_named() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.dashboard = StaticDashboardAssets {
            assets: &[
                StaticDashboardAsset {
                    path: "index.html",
                    contents: b"<html>one</html>",
                    content_type: "text/html; charset=utf-8",
                },
                StaticDashboardAsset {
                    path: "index.html",
                    contents: b"<html>two</html>",
                    content_type: "text/html; charset=utf-8",
                },
            ],
            cache_tag: "bundle-tag-1",
        };
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("duplicate asset path"), "{reason}");
        assert!(reason.contains("index.html"), "{reason}");
    }

    #[test]
    fn a_bundle_without_index_html_is_rejected() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.dashboard = StaticDashboardAssets {
            assets: &[StaticDashboardAsset {
                path: "static/app.0123abcd.js",
                contents: b"console.log('bundle')",
                content_type: "application/javascript",
            }],
            cache_tag: "bundle-tag-1",
        };
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("index.html"), "{reason}");
    }

    #[test]
    fn an_empty_cache_tag_is_rejected() {
        let slot = OnceLock::new();
        let mut provider = valid_provider();
        provider.dashboard = StaticDashboardAssets {
            assets: VALID_ASSETS.assets,
            cache_tag: "",
        };
        let reason = invalid_reason(register_in(&slot, provider));
        assert!(reason.contains("cache_tag"), "{reason}");
    }

    #[test]
    fn build_version_composition_covers_clean_and_dirty() {
        assert_eq!(
            compose_build_version("0.0.66", VALID_SHA, false),
            format!("0.0.66+{VALID_SHA}")
        );
        assert_eq!(
            compose_build_version("0.0.66", VALID_SHA, true),
            format!("0.0.66+{VALID_SHA}.dirty")
        );
    }

    /// The fixture skips [`validated`] because it is a constant; this pins
    /// that the constant would still pass a real registration.
    #[test]
    fn the_fixture_provider_passes_registration_validation() {
        let runtime = validated(FIXTURE_PROVIDER).expect("fixture provider must stay valid");
        assert_eq!(
            runtime.build_version(),
            format!("{PACKAGE_VERSION}+{}", super::FIXTURE_FULL_SHA)
        );
    }

    /// The unit-test process registers the fixture into the real global slot,
    /// matching the documented invariant for test processes.
    #[test]
    fn the_global_fixture_registration_is_idempotent_and_readable() {
        let first: &'static RegisteredProductRuntime = register_fixture_product_runtime();
        let second = register_fixture_product_runtime();
        assert!(std::ptr::eq(first, second));
        assert_eq!(first.source().full_sha, super::FIXTURE_FULL_SHA);
        assert_eq!(
            super::product_runtime()
                .expect("fixture registered")
                .build_version(),
            first.build_version()
        );
    }
}
