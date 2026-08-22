//! The one sanctioned test-only observability hook: thread-local counters
//! over the domain's canonicalize-then-SHA256 boundaries.
//!
//! Store-level tests use these counters to prove a terminally refused
//! observation is never re-decoded, re-derived, or re-hashed. Counting here —
//! at the functions that perform the work — rather than at a store dispatch
//! seam means the proof cannot regress silently when digest work moves
//! earlier than the dispatch (e.g. idempotency-identity hashing computed
//! before a submit that then early-exits), and it requires no test-only port
//! in any store crate.
//!
//! Three boundaries are counted:
//!
//! * **identity** — [`crate::observation`]'s `domain_digest`: every fresh
//!   canonical observation-identity derivation and every stored-row
//!   decode-time verification (`accepted_identity_digests`) funnels through
//!   it, so a zero delta proves no identity material was re-canonicalized or
//!   re-hashed on the observed thread.
//! * **payload** — `sha256_digest` under
//!   [`crate::observation::PayloadReferenceV1::for_payload`]: the only
//!   payload-content hash, so a zero delta proves no payload was
//!   re-canonicalized or re-hashed.
//! * **canonical** — [`crate::research::canonical_sha256`]: every runtime
//!   read and write command digest is computed through it on the dispatching
//!   thread *before* the request crosses into the store runtime, so the exact
//!   delta bounds the record work a call dispatched — a stored-row read that
//!   would be decoded off-thread still costs its command digest here first.
//!
//! Counters are thread-local so parallel tests cannot bleed counts into each
//! other. Never enable the `identity-digest-probe` feature in production
//! builds.

use std::cell::Cell;

thread_local! {
    static IDENTITY_DIGESTS: Cell<u64> = const { Cell::new(0) };
    static PAYLOAD_DIGESTS: Cell<u64> = const { Cell::new(0) };
    static CANONICAL_DIGESTS: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn record_identity() {
    IDENTITY_DIGESTS.with(|digests| digests.set(digests.get() + 1));
}

pub(crate) fn record_payload() {
    PAYLOAD_DIGESTS.with(|digests| digests.set(digests.get() + 1));
}

pub(crate) fn record_canonical() {
    CANONICAL_DIGESTS.with(|digests| digests.set(digests.get() + 1));
}

/// Canonical observation-identity digests computed on the current thread so
/// far (fresh derivations and decode-time verifications alike).
pub fn identity_digests() -> u64 {
    IDENTITY_DIGESTS.with(Cell::get)
}

/// Canonical payload-content digests computed on the current thread so far.
pub fn payload_digests() -> u64 {
    PAYLOAD_DIGESTS.with(Cell::get)
}

/// Canonical manifest/command digests ([`crate::research::canonical_sha256`])
/// computed on the current thread so far.
pub fn canonical_digests() -> u64 {
    CANONICAL_DIGESTS.with(Cell::get)
}
