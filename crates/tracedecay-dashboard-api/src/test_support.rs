//! Process-wide environment pinning for this crate's own tests.
//!
//! The dashboard tests used to import `PinnedUserDataDir` from the root
//! crate's `config` module. That type stayed in the root's `src/config.rs` under
//! `#[cfg(test)]`, so it is not reachable across the crate boundary — and
//! reaching for it was never sound in the first place: the guard serializes on
//! a `static` mutex, and a `static` is per-test-binary. The root's lock never
//! serialized this crate's tests. Owning the guard here makes the
//! serialization real for the binary it actually protects.

use std::ffi::OsString;

use tracedecay_runtime_core::config::{TRACEDECAY_DIR, USER_DATA_DIR_ENV};
use tracedecay_runtime_core::storage::PrivateStoreIo;

/// Serializes tests that mutate process-wide storage environment variables
/// (`TRACEDECAY_DATA_DIR` and the HOME/USERPROFILE profile pins).
static USER_DATA_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires [`USER_DATA_DIR_TEST_LOCK`], recovering even when poisoned.
fn lock_user_data_dir_test_env() -> std::sync::MutexGuard<'static, ()> {
    USER_DATA_DIR_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Pins [`USER_DATA_DIR_ENV`] and agent home discovery to an isolated temp
/// profile while holding [`USER_DATA_DIR_TEST_LOCK`], so parallel tests cannot
/// race profile resolution or scan live host transcripts.
///
/// The pin is released, and the previous environment restored, on drop.
pub struct PinnedUserDataDir {
    _lock: std::sync::MutexGuard<'static, ()>,
    _root: tempfile::TempDir,
    previous: Option<OsString>,
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
}

impl PinnedUserDataDir {
    #[must_use]
    pub fn new() -> Self {
        let lock = lock_user_data_dir_test_env();
        let root = tempfile::TempDir::new()
            .unwrap_or_else(|err| panic!("failed to create temp profile dir: {err}"));
        let profile = root.path().join(TRACEDECAY_DIR);
        PrivateStoreIo::create_dir_all(&profile)
            .unwrap_or_else(|err| panic!("failed to create isolated profile root: {err}"));
        let previous = std::env::var_os(USER_DATA_DIR_ENV);
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        // SAFETY: every writer of these variables in this crate's tests goes
        // through this guard, which holds `USER_DATA_DIR_TEST_LOCK` for its
        // whole lifetime, so no other thread in this binary is concurrently
        // reading or writing them.
        unsafe {
            std::env::set_var(USER_DATA_DIR_ENV, &profile);
            std::env::set_var("HOME", root.path());
            std::env::set_var("USERPROFILE", root.path());
        }
        Self {
            _lock: lock,
            _root: root,
            previous,
            previous_home,
            previous_userprofile,
        }
    }
}

impl Default for PinnedUserDataDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PinnedUserDataDir {
    fn drop(&mut self) {
        // SAFETY: as in `new` — the guard still holds the lock here.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(USER_DATA_DIR_ENV, previous),
                None => std::env::remove_var(USER_DATA_DIR_ENV),
            }
            match self.previous_home.take() {
                Some(previous) => std::env::set_var("HOME", previous),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(previous) => std::env::set_var("USERPROFILE", previous),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }
}
