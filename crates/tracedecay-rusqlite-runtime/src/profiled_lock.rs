//! Lock type aliases that change shape with the `hotpath` feature.
//!
//! `hotpath::mutex!` returns an instrumented drop-in wrapper when profiling is
//! on and its own argument when it is off, so the *type* of an instrumented
//! lock differs between the two builds. Naming that difference once, here,
//! keeps it out of every struct that holds a lock: fields spell the alias,
//! construction sites spell the macro, and call sites are identical either
//! way because the wrapper mirrors the std API.
//!
//! Instrument at the construction site rather than through a helper — the
//! macros capture `file!()`/`line!()`, so wrapping them in a function would
//! collapse every lock in the crate onto one source location. Pass a `label`
//! as well, since that is what the `mutexes` report keys on.

// Unconditional on purpose: `hotpath::mutex!` expands by the *hotpath
// crate's* feature state, which Cargo unifies workspace-wide - a sibling
// crate enabling profiling flips the macro's return type even when this
// crate's own `hotpath` feature is off. Aliasing through the same crate
// (`hotpath::mutexes::Mutex` is `std::sync::Mutex` in the no-op build)
// keeps the alias and the macro in lockstep under any feature unification.
pub(crate) type ProfiledMutex<T> = hotpath::mutexes::Mutex<T>;
