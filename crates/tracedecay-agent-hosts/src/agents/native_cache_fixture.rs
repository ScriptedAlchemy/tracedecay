//! Byte-level fixture for a host's native plugin cache.
//!
//! Claude Code and Codex each copy the rendered plugin source into their own
//! versioned cache, and TraceDecay's activation probes decide whether the host
//! loads the plugin by comparing those cached bytes against the rendered
//! bundle. Fixtures have to reproduce the host's copy as a plain byte copy.

use std::path::Path;

use tracedecay_domain::canonical_text::sha256_hex;

/// Materialize the rendered `relatives` from `source` into the host's native
/// `cache` root.
///
/// Deliberately read and write source bytes for cache fidelity: activation
/// probes compare content digests. Same-length cache-byte divergence has been
/// observed, but its underlying cause has not been established.
pub(crate) fn materialize_cached_bundle<'a>(
    source: &Path,
    cache: &Path,
    relatives: impl IntoIterator<Item = &'a str>,
) {
    for relative in relatives {
        let target = cache.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let contents = std::fs::read(source.join(relative)).unwrap();
        std::fs::write(&target, &contents).unwrap();
    }
}

/// Assert `source` and `cache` both hold exactly the bytes `rendered` declares.
///
/// A host-cache refresh is a content contract: the fixture must not leave a
/// same-length file with different bytes behind, and a divergence has to name
/// the file and both digests instead of only failing the activation probe.
pub(crate) fn assert_rendered_bundle_bytes(
    source: &Path,
    cache: &Path,
    rendered: &[(&str, String)],
) {
    for (relative, expected) in rendered {
        for (label, root) in [("source", source), ("cache", cache)] {
            let observed = std::fs::read(root.join(relative)).unwrap();
            assert!(
                observed == expected.as_bytes(),
                "{label}/{relative}: refreshed bundle is not byte-exact; \
                 actual(len={} sha256={}), expected(len={} sha256={})",
                observed.len(),
                sha256_hex(&observed),
                expected.len(),
                sha256_hex(expected.as_bytes()),
            );
        }
    }
}
