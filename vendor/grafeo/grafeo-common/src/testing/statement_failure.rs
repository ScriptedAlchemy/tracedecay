//! Statement / commit failure injection for testing rollback paths.
//!
//! When the `testing-statement-injection` feature is enabled,
//! [`maybe_fail_statement`] counts down a **thread-local** counter and returns
//! [`InjectedFailure`] when it reaches zero. [`maybe_fail_commit`] returns
//! [`InjectedFailure`] once if a commit trigger is armed.
//!
//! Unlike [`crash`](super::crash) (which panics), injection returns an error
//! so the session's normal error path runs. This matches how real runtime
//! errors (constraint violations, parse errors) behave: the transaction stays
//! active and the caller is expected to issue `ROLLBACK` explicitly.
//!
//! Thread-local storage ensures concurrent tests never interfere with each
//! other; only the thread that calls [`enable_statement_failure_after`] /
//! [`enable_commit_failure_once`] is affected.
//!
//! When the feature is **disabled**, all functions compile to no-ops with
//! zero runtime overhead.
//!
//! # Example
//!
//! ```ignore
//! use grafeo_common::testing::statement_failure::with_statement_failure_after;
//!
//! with_statement_failure_after(3, || {
//!     // 1st and 2nd calls to maybe_fail_statement succeed.
//!     // 3rd returns Err(InjectedFailure).
//! });
//! ```

use std::fmt;

/// Error returned by [`maybe_fail_statement`] / [`maybe_fail_commit`] when
/// the injection fires. Tests match on this via the session's error chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectedFailure;

impl fmt::Display for InjectedFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("injected test failure")
    }
}

impl std::error::Error for InjectedFailure {}

#[cfg(feature = "testing-statement-injection")]
mod inner {
    use super::InjectedFailure;
    use std::cell::Cell;

    thread_local! {
        static STATEMENT_COUNTER: Cell<u64> = const { Cell::new(u64::MAX) };
        static STATEMENT_ENABLED: Cell<bool> = const { Cell::new(false) };
        static COMMIT_TRIGGER: Cell<bool> = const { Cell::new(false) };
    }

    /// Conditionally return [`InjectedFailure`] when the statement counter
    /// reaches zero. Insert this at the entry of each statement-execution
    /// boundary. Zero-overhead when injection is disabled.
    ///
    /// Uses thread-local state so concurrent tests don't interfere.
    ///
    /// # Errors
    ///
    /// Returns [`InjectedFailure`] when statement injection is enabled and
    /// the counter reaches zero.
    #[inline]
    pub fn maybe_fail_statement() -> Result<(), InjectedFailure> {
        STATEMENT_ENABLED.with(|enabled| {
            if !enabled.get() {
                return Ok(());
            }
            STATEMENT_COUNTER.with(|counter| {
                let prev = counter.get();
                counter.set(prev.wrapping_sub(1));
                if prev == 1 {
                    Err(InjectedFailure)
                } else {
                    Ok(())
                }
            })
        })
    }

    /// Conditionally return [`InjectedFailure`] once if a commit failure is
    /// armed. The trigger is one-shot: it consumes itself on first call.
    ///
    /// # Errors
    ///
    /// Returns [`InjectedFailure`] once per call to [`enable_commit_failure_once`].
    #[inline]
    pub fn maybe_fail_commit() -> Result<(), InjectedFailure> {
        COMMIT_TRIGGER.with(|trigger| {
            if trigger.get() {
                trigger.set(false);
                Err(InjectedFailure)
            } else {
                Ok(())
            }
        })
    }

    /// Arm statement injection to fire on the `count`-th call to
    /// [`maybe_fail_statement`].
    ///
    /// Only affects the calling thread.
    pub fn enable_statement_failure_after(count: u64) {
        STATEMENT_COUNTER.with(|c| c.set(count));
        STATEMENT_ENABLED.with(|e| e.set(true));
    }

    /// Arm a one-shot commit failure for the next call to [`maybe_fail_commit`].
    ///
    /// Only affects the calling thread.
    pub fn enable_commit_failure_once() {
        COMMIT_TRIGGER.with(|t| t.set(true));
    }

    /// Disable both statement and commit injection (reset to no-op behavior).
    ///
    /// Only affects the calling thread.
    pub fn disable_injection() {
        STATEMENT_ENABLED.with(|e| e.set(false));
        STATEMENT_COUNTER.with(|c| c.set(u64::MAX));
        COMMIT_TRIGGER.with(|t| t.set(false));
    }
}

#[cfg(not(feature = "testing-statement-injection"))]
mod inner {
    use super::InjectedFailure;

    /// No-op when injection is disabled.
    ///
    /// # Errors
    ///
    /// Always returns `Ok(())` when the `testing-statement-injection` feature
    /// is off; the result type is kept so call sites compile uniformly.
    #[inline]
    pub fn maybe_fail_statement() -> Result<(), InjectedFailure> {
        Ok(())
    }

    /// No-op when injection is disabled.
    ///
    /// # Errors
    ///
    /// Always returns `Ok(())` when the `testing-statement-injection` feature
    /// is off; the result type is kept so call sites compile uniformly.
    #[inline]
    pub fn maybe_fail_commit() -> Result<(), InjectedFailure> {
        Ok(())
    }

    /// No-op when injection is disabled.
    pub fn enable_statement_failure_after(_count: u64) {}

    /// No-op when injection is disabled.
    pub fn enable_commit_failure_once() {}

    /// No-op when injection is disabled.
    pub fn disable_injection() {}
}

pub use inner::*;

/// Run `f` with statement injection armed to fire on the `fail_after`-th call
/// to [`maybe_fail_statement`]. Injection is automatically disabled after the
/// closure returns.
pub fn with_statement_failure_after<F, T>(fail_after: u64, f: F) -> T
where
    F: FnOnce() -> T,
{
    enable_statement_failure_after(fail_after);
    let result = f();
    disable_injection();
    result
}

/// Run `f` with a one-shot commit failure armed. Injection is automatically
/// disabled after the closure returns.
pub fn with_commit_failure<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    enable_commit_failure_once();
    let result = f();
    disable_injection();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "testing-statement-injection")]
    fn statement_fires_at_exact_count() {
        let result = with_statement_failure_after(3, || {
            let a = maybe_fail_statement();
            let b = maybe_fail_statement();
            let c = maybe_fail_statement();
            (a, b, c)
        });
        assert_eq!(result.0, Ok(()));
        assert_eq!(result.1, Ok(()));
        assert_eq!(result.2, Err(InjectedFailure));
    }

    #[test]
    #[cfg(feature = "testing-statement-injection")]
    fn commit_fires_once() {
        let result = with_commit_failure(|| {
            let a = maybe_fail_commit();
            let b = maybe_fail_commit();
            (a, b)
        });
        assert_eq!(result.0, Err(InjectedFailure));
        assert_eq!(result.1, Ok(()), "commit trigger is one-shot");
    }

    #[test]
    fn completes_when_count_exceeds_calls() {
        let result = with_statement_failure_after(100, || {
            let a = maybe_fail_statement();
            let b = maybe_fail_statement();
            (a, b)
        });
        assert_eq!(result.0, Ok(()));
        assert_eq!(result.1, Ok(()));
    }

    #[test]
    fn disabled_by_default() {
        assert_eq!(maybe_fail_statement(), Ok(()));
        assert_eq!(maybe_fail_commit(), Ok(()));
    }

    #[test]
    fn disable_resets_state() {
        enable_statement_failure_after(2);
        enable_commit_failure_once();
        disable_injection();
        assert_eq!(maybe_fail_statement(), Ok(()));
        assert_eq!(maybe_fail_statement(), Ok(()));
        assert_eq!(maybe_fail_commit(), Ok(()));
    }
}
