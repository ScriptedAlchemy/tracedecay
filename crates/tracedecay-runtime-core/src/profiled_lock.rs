//! Lock type aliases that change shape with the `hotpath` feature.
//!
//! `hotpath::mutex!` / `hotpath::rw_lock!` return an instrumented drop-in
//! wrapper when profiling is on and their own argument when it is off, so the
//! *type* of an instrumented lock differs between the two builds. Naming that
//! difference once, here, keeps it out of every struct that holds a lock:
//! fields spell the alias, construction sites spell the macro, and call sites
//! are identical either way because the wrapper mirrors the std API.
//!
//! Instrument at the construction site rather than through a helper — the
//! macros capture `file!()`/`line!()`, so wrapping them in a function would
//! collapse every lock in the crate onto one source location. Pass a `label`
//! as well, since that is what the `mutexes` and `rw_locks` reports key on.
//!
//! The instrumented wrappers do not implement `Debug`, so a struct that holds
//! one cannot `#[derive(Debug)]`; those few types spell out a manual `Debug`
//! that prints the lock's identity rather than its contents.
//!
//! A mutex paired with a [`std::sync::Condvar`] cannot be instrumented at all:
//! `Condvar::wait` demands a real [`std::sync::MutexGuard`] and the wrapper
//! hands back its own guard type. Leave those locks alone.
//!
//! The aliases are unconditional on purpose: the `hotpath::mutex!` /
//! `rw_lock!` macros expand by the *hotpath crate's* feature state, which
//! Cargo unifies workspace-wide - a sibling crate enabling profiling flips
//! the macros' return types even when this crate's own `hotpath` feature is
//! off. Aliasing through the same crate (each wrapper is its std counterpart
//! in the no-op build) keeps alias and macro in lockstep under any feature
//! unification.

pub(crate) type ProfiledMutex<T> = hotpath::mutexes::Mutex<T>;

pub(crate) type ProfiledMutexGuard<'a, T> = hotpath::mutexes::MutexGuard<'a, T>;

pub(crate) type ProfiledRwLock<T> = hotpath::rw_locks::RwLock<T>;

pub(crate) type ProfiledRwLockReadGuard<'a, T> = hotpath::rw_locks::RwLockReadGuard<'a, T>;

pub(crate) type ProfiledRwLockWriteGuard<'a, T> = hotpath::rw_locks::RwLockWriteGuard<'a, T>;
