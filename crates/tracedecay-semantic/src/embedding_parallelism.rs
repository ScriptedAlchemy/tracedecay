//! Embedding width: race the batch job to idle behind the serving reservation.
//!
//! Embedding a published code generation is batch work with a finish line, so
//! it gets the same treatment as extraction (`docs/SERVING-PATH-PERFORMANCE.md`
//! Principle 2): run at machine width, finish, and get out of the way. The
//! serving reservation — not a throttled embedder — is what keeps interactive
//! reads fast, so the width here is derived from
//! [`tracedecay_code_index::parallelism::indexing_worker_target`] and the work
//! is dispatched onto that same reserved pool.
//!
//! Two knobs make up the width, and they are *not* interchangeable:
//!
//! - **Intra-op threads** are how many CPUs ONNX Runtime uses inside one
//!   tensor invocation. Raising this changes how a GEMM is partitioned, which
//!   can change floating-point reduction order — so it is a *numerics* knob,
//!   pinned by the artifact's declared ceiling and never inferred from the
//!   host.
//! - **Session width** is how many independent batches are in flight at once.
//!   Each batch is a separate invocation of the same graph over the same
//!   tensor shape, so results are bit-identical at any width. This is the
//!   knob that scales with the host, exactly as indexing width does.
//!
//! Everything here is therefore sizing policy only: vector bytes are identical
//! at width 1 and at full width, and that equivalence is a test.

use tracedecay_code_index::parallelism::indexing_worker_target;

/// Operator override for concurrently embedding sessions, for hosts where
/// memory rather than CPU binds. Values below 1 are ignored.
const EMBED_SESSIONS_ENV: &str = "TRACEDECAY_EMBED_SESSIONS";

/// Operator override for how many chunks are packed into one ONNX
/// invocation. This changes the padded tensor shape, so it is an explicit
/// override rather than a host-derived value.
const EMBED_BATCH_CHUNKS_ENV: &str = "TRACEDECAY_EMBED_BATCH_CHUNKS";

/// Never open more concurrent sessions than this regardless of host width:
/// each session is a full resident copy of the model graph.
const MAX_EMBEDDING_SESSIONS: usize = 16;

/// Chunks packed into one ONNX invocation when the artifact permits it. The
/// prior value (8) left tokenizer and per-invocation setup cost dominating
/// wall clock.
const DEFAULT_EMBED_BATCH_CHUNKS: usize = 32;

/// Intra-op threads a shipped default configuration requests.
///
/// Held at the historical value on purpose: this is a numerics knob, so it
/// must move deliberately together with a re-embed, never as a side effect of
/// running on a wider host.
pub const DEFAULT_INTRA_THREADS: u32 = 4;

fn detected_cores() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn env_width(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width >= 1)
}

/// Chunks to pack into one ONNX invocation, clamped to what the admitted
/// artifact permits.
///
/// The artifact ceiling always wins: a wider request can never widen a tensor
/// beyond the shape the manifest admitted.
#[must_use]
pub fn embedding_batch_chunks(artifact_max_batch_texts: u32) -> usize {
    let ceiling = (artifact_max_batch_texts as usize).max(1);
    env_width(EMBED_BATCH_CHUNKS_ENV)
        .unwrap_or(DEFAULT_EMBED_BATCH_CHUNKS)
        .min(ceiling)
        .max(1)
}

/// Concurrent embedding sessions for a host with `total_cores` logical CPUs,
/// given the intra-op thread count the artifact pinned.
///
/// `sessions * intra_threads` is held to the indexing width so embedding lives
/// inside the same reservation as extraction rather than stacking a second
/// full-machine pool beside it.
#[must_use]
pub fn embedding_session_width_for(
    total_cores: usize,
    intra_threads: u32,
    configured_max_sessions: u32,
) -> usize {
    let intra = (intra_threads as usize).max(1);
    let configured = (configured_max_sessions as usize).max(1);
    (indexing_worker_target(total_cores) / intra)
        .max(1)
        .min(MAX_EMBEDDING_SESSIONS)
        .min(configured)
}

/// Concurrent embedding sessions on this host, honouring the operator
/// override. Always at least 1.
#[must_use]
pub fn embedding_session_width(intra_threads: u32, configured_max_sessions: u32) -> usize {
    let configured = (configured_max_sessions as usize).max(1);
    match env_width(EMBED_SESSIONS_ENV) {
        Some(forced) => forced.min(MAX_EMBEDDING_SESSIONS).min(configured),
        None => embedding_session_width_for(detected_cores(), intra_threads, configured_max_sessions),
    }
}

/// Session-pool sizing that lets the derived concurrency actually be used.
///
/// The pool's own memory ceiling still applies; this only stops the pool from
/// becoming the binding constraint before the reservation is. The extra slot
/// keeps an interactive query session warm while a rebuild holds the
/// projection sessions.
#[must_use]
pub fn embedding_pool_sessions(intra_threads: u32, configured_max_sessions: u32) -> usize {
    embedding_session_width(intra_threads, configured_max_sessions).saturating_add(1)
}

/// Host-derived default for the configuration's concurrent-session ceiling.
///
/// Configuration stays authoritative — an operator who pins a lower value
/// keeps it. This only changes what "unset" means, from "one session on every
/// host" to "as many as the serving reservation leaves room for".
#[must_use]
pub fn default_max_concurrent_sessions() -> u32 {
    default_max_concurrent_sessions_for(detected_cores())
}

#[must_use]
pub fn default_max_concurrent_sessions_for(total_cores: usize) -> u32 {
    let width = embedding_session_width_for(
        total_cores,
        DEFAULT_INTRA_THREADS,
        MAX_EMBEDDING_SESSIONS as u32,
    );
    u32::try_from(width.max(1)).unwrap_or(1)
}

/// Run `operation` on the reserved-width indexing pool.
///
/// Embedding shares extraction's pool deliberately: two pools each sized to
/// the reservation would together oversubscribe the machine and consume the
/// reservation they were meant to respect.
pub fn install<R, F>(operation: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    tracedecay_code_index::parallelism::install(operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_width_stays_inside_the_indexing_reservation() {
        // 96 cores => indexing width 90; 4 intra threads => 22, capped at 16.
        assert_eq!(embedding_session_width_for(96, 4, 64), 16);
        // 16 cores => indexing width 14; 4 intra threads => 3.
        assert_eq!(embedding_session_width_for(16, 4, 64), 3);
        // A narrow host still embeds, just without concurrency.
        assert_eq!(embedding_session_width_for(4, 4, 64), 1);
        assert_eq!(embedding_session_width_for(1, 4, 64), 1);
    }

    #[test]
    fn configured_ceiling_is_never_exceeded() {
        assert_eq!(embedding_session_width_for(96, 4, 1), 1);
        assert_eq!(embedding_session_width_for(96, 4, 2), 2);
    }

    #[test]
    fn wider_intra_threads_narrow_the_session_width() {
        assert_eq!(embedding_session_width_for(96, 1, 64), 16);
        assert_eq!(embedding_session_width_for(96, 32, 64), 2);
        assert_eq!(embedding_session_width_for(96, 128, 64), 1);
    }

    #[test]
    fn every_host_keeps_at_least_one_session_and_one_batch_chunk() {
        for cores in 1..=256usize {
            assert!(embedding_session_width_for(cores, 4, 64) >= 1);
            assert!(default_max_concurrent_sessions_for(cores) >= 1);
        }
        assert_eq!(embedding_batch_chunks(0), 1);
        assert_eq!(embedding_batch_chunks(1), 1);
    }

    #[test]
    fn batch_chunks_never_exceed_the_artifact_ceiling() {
        assert_eq!(embedding_batch_chunks(4), 4);
    }
}
