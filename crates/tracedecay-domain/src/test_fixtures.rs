//! Shared fixture constructors for tests in every crate that already depends
//! on this one.
//!
//! The values are not hashes of anything. Suites used to each re-roll the same
//! `sha256:` spelling and the same `TryFrom<String>` identity parse.

use std::fmt::Debug;

use crate::ManifestDigest;

/// `sha256:` plus `digit` repeated 64 times.
///
/// Not a hash. One lowercase hex digit parses as a manifest digest and stays
/// distinct from its siblings.
pub fn repeated_sha256_text(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

/// [`repeated_sha256_text`] parsed as a manifest digest.
///
/// An illegal digit is a broken fixture, so this fails immediately rather than
/// handing each suite its own `unwrap`.
pub fn digest(digit: char) -> ManifestDigest {
    ManifestDigest::new(repeated_sha256_text(digit)).expect("fixture digest is canonical")
}

/// Absolute fixture root. Work and registry contracts require
/// `Path::is_absolute`, and a bare `/...` literal is not absolute on Windows.
pub fn fixture_abs_root(posix: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", posix.replace('/', "\\"))
    } else {
        posix.to_owned()
    }
}

/// Parses a fixture identity. The value must already be canonical for `T`.
pub fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

static TRACING_CALLSITE_KEEPALIVE: std::sync::OnceLock<[tracing::Dispatch; 2]> =
    std::sync::OnceLock::new();

/// Pins every `tracing` callsite in this test process to `Interest::sometimes`
/// so a per-test thread-local `Dispatch` census stays isolated to that thread.
///
/// `tracing_core` caches callsite interest process-globally and computes it
/// from `Dispatchers::rebuilder()`. While at most one `Dispatch` is registered,
/// that rebuilder takes the `Rebuilder::JustOne` fast path, which asks the
/// current thread's default subscriber. Callsites register lazily on first
/// execution, so under `--test-threads=N` an unrelated test that reaches a
/// callsite first, on a thread with no scoped dispatcher (`NoSubscriber`),
/// permanently caches `Interest::never()` for that callsite. A later census
/// then observes zero events for work that did happen.
///
/// Registering two permanently-live dispatchers makes `has_just_one` false for
/// the life of the process, so interest is folded over the real registry
/// instead of one arbitrary thread's default. Both keepalives claim
/// `Interest::sometimes()` for every callsite, which means enablement is
/// decided per event by the calling thread's dispatcher. Constructing them
/// also rebuilds the interest cache, repairing any callsite already poisoned
/// before the census ran.
fn install_tracing_callsite_keepalive() {
    struct AlwaysConsultThreadDispatch;

    impl tracing::Subscriber for AlwaysConsultThreadDispatch {
        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::sometimes()
        }

        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            false
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    TRACING_CALLSITE_KEEPALIVE.get_or_init(|| {
        [
            tracing::Dispatch::new(AlwaysConsultThreadDispatch),
            tracing::Dispatch::new(AlwaysConsultThreadDispatch),
        ]
    });
}

/// Installs the callsite keepalive, then sets `subscriber` as this thread's
/// default until the guard drops.
///
/// While only one dispatcher is registered, `tracing_core` caches interest from
/// the thread that first reaches a callsite. A thread with no subscriber stores
/// `Interest::never()` for the process. The keepalive keeps two dispatchers
/// registered so a later capture on this thread still observes the event.
#[must_use = "dropping the guard unregisters the capturing subscriber"]
pub fn set_tracing_capture<S>(subscriber: S) -> tracing::subscriber::DefaultGuard
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    install_tracing_callsite_keepalive();
    tracing::subscriber::set_default(subscriber)
}

#[cfg(test)]
mod tests {
    use super::{digest, id, repeated_sha256_text};

    #[test]
    fn repeated_hex_digest_matches_the_shared_spelling() {
        let spelled = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(repeated_sha256_text('a'), spelled);
        assert_eq!(digest('a').as_str(), spelled);
        assert_eq!(
            repeated_sha256_text('0'),
            crate::ManifestDigest::zero().unwrap().as_str()
        );
    }

    #[test]
    fn fixture_id_accepts_a_canonical_project_id() {
        let project: crate::ProjectId = id("project.fixture");
        assert_eq!(project.as_str(), "project.fixture");
    }

    #[test]
    fn fixture_abs_root_is_absolute_on_this_host() {
        let root = super::fixture_abs_root("/workspace/fixture");
        assert!(std::path::Path::new(&root).is_absolute(), "{root}");
    }
}
