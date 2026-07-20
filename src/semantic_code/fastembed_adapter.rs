//! PR10 quarantined preparation packet `pr10/prep-runtime-adapter`
//! (Plan 31, `docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md`).
//!
//! Root-private embedding runtime port surface. This file owns the typed
//! `EmbeddingRuntime` / `EmbeddingSession` ports: load a verified artifact,
//! create bounded sessions, embed bounded sanitized text batches, and return
//! typed vectors whose dimensions, metric, and normalization are echoed from
//! the verified manifest descriptor. It also owns the deterministic
//! [`FakeEmbeddingRuntime`] used by every offline pool/session test.
//!
//! QUARANTINE STATUS: temporarily unlinked. Registration happens at
//! integration by the Sol coordinator; this file must not be referenced from
//! `src/lib.rs` or `src/semantic_code/mod.rs` in this packet. It is std-only
//! on purpose so it compiles standalone via `#[path]` inclusion.
//!
//! The real FastEmbed-backed implementation is NOT part of this packet. The
//! port is shaped so that implementation drops in later as another
//! `EmbeddingRuntime` impl: at integration this becomes the only module that
//! imports `fastembed` (Plan 31: "Only one root-private adapter depends on
//! `fastembed`").
//!
//! Design decisions recorded for the coordinator (plan-level ambiguities,
//! flagged rather than guessed):
//!
//! - ESCALATION-1 (sync vs async): Plan 31 does not state whether the runtime
//!   port is async. FastEmbed/ORT inference is blocking CPU work, and PR9's
//!   `src/query/retrieval/ports.rs` sets the precedent that ports are
//!   synchronous contracts with scheduling/cancellation above them. This port
//!   is therefore synchronous; async wrapping is an integration concern.
//! - ESCALATION-2 (metric enumeration): Plan 31 pins "metric" in the manifest
//!   but never enumerates values. [`EmbeddingMetricV1`] is a closed enum of
//!   cosine/dot/L2; adding a variant later is additive.
//! - ESCALATION-3 (projection identity): Plan 31's `EmbeddingProjectionKeyV1`
//!   lives in the domain packet (crates/tracedecay-domain), not here. The
//!   pool key in `session_pool.rs` therefore carries the projection profile
//!   digest as an opaque string plus privacy domain/key epoch; the integrator
//!   maps `EmbeddingProjectionKeyV1.profile_digest` into it.
//! - ESCALATION-4 (budget type): Plan 31 says deadline/cancellation limits are
//!   fields of the shared PR9 `RetrievalBudget` and PR10 introduces no
//!   semantic-only budget type. That domain type is outside this quarantined
//!   packet, so deadlines are modelled here as a `Duration` against the
//!   injected pool clock and cancellation as the [`CancellationSignal`] trait;
//!   the integrator adapts `RetrievalBudget` onto both.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Upper bound on a declared embedding dimension (manifest sanity limit; the
/// largest maintained FastEmbed text models are far below this).
pub const MAX_EMBEDDING_DIMENSIONS: u32 = 8192;

/// Distance metric pinned by the verified manifest (Plan 31: the manifest
/// pins dimension, metric, and normalization; vectors are only comparable
/// under the same pinned triple).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EmbeddingMetricV1 {
    Cosine,
    DotProduct,
    EuclideanL2,
}

impl fmt::Display for EmbeddingMetricV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Cosine => "cosine",
            Self::DotProduct => "dot_product",
            Self::EuclideanL2 => "euclidean_l2",
        };
        f.write_str(s)
    }
}

/// Output-vector normalization pinned by the verified manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EmbeddingNormalizationV1 {
    None,
    L2,
}

impl fmt::Display for EmbeddingNormalizationV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::None => "none",
            Self::L2 => "l2",
        };
        f.write_str(s)
    }
}

/// Precision/quantization pinned by the verified manifest (Plan 31:
/// precision/quantization is a vector-affecting input and must be echoed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EmbeddingPrecisionV1 {
    F32,
    F16,
    QuantizedI8,
}

impl fmt::Display for EmbeddingPrecisionV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::F32 => "f32",
            Self::F16 => "f16",
            Self::QuantizedI8 => "quantized_i8",
        };
        f.write_str(s)
    }
}

/// Typed failure of one embedding operation or runtime admission (Plan 31:
/// load failure, OOM, corruption, revocation, or incompatible pins disables
/// the affected semantic stage; nothing silently substitutes another model).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbedError {
    /// The caller's cancellation signal fired before or during the batch.
    Cancelled,
    /// A batch with zero texts invokes no inference.
    EmptyBatch,
    /// The batch exceeded the manifest's bounded text count.
    TooManyTexts { presented: usize, max: usize },
    /// The batch exceeded the manifest's bounded total sanitized bytes.
    BatchBytesExceeded { presented: usize, max: usize },
    /// A produced vector does not match the manifest's declared dimension.
    DimensionMismatch { expected: u32, actual: usize },
    /// A produced vector contains NaN or infinite values.
    NonFiniteVectorValue,
    /// The verified-artifact descriptor failed validation.
    InvalidArtifactDescriptor(String),
    /// Runtime-level failure (load, OOM, corruption, revocation,
    /// incompatibility, inference failure).
    Runtime(RuntimeFailureV1),
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "the embedding operation was cancelled"),
            Self::EmptyBatch => write!(f, "an empty batch invokes no inference"),
            Self::TooManyTexts { presented, max } => write!(
                f,
                "batch of {presented} texts exceeds the manifest bound of {max}"
            ),
            Self::BatchBytesExceeded { presented, max } => write!(
                f,
                "batch of {presented} bytes exceeds the manifest bound of {max}"
            ),
            Self::DimensionMismatch { expected, actual } => write!(
                f,
                "vector dimension {actual} does not match the manifest dimension {expected}"
            ),
            Self::NonFiniteVectorValue => {
                write!(f, "vector contains NaN or infinite values")
            }
            Self::InvalidArtifactDescriptor(detail) => {
                write!(f, "invalid verified-artifact descriptor: {detail}")
            }
            Self::Runtime(failure) => write!(f, "{failure}"),
        }
    }
}

impl Error for EmbedError {}

/// Kind of runtime-level failure (Plan 31: each disables the affected
/// semantic stage with a typed reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeFailureKindV1 {
    LoadFailed,
    OutOfMemory,
    CorruptArtifact,
    RevokedArtifact,
    IncompatibleRuntime,
    EmbedFailed,
}

impl fmt::Display for RuntimeFailureKindV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::LoadFailed => "load_failed",
            Self::OutOfMemory => "out_of_memory",
            Self::CorruptArtifact => "corrupt_artifact",
            Self::RevokedArtifact => "revoked_artifact",
            Self::IncompatibleRuntime => "incompatible_runtime",
            Self::EmbedFailed => "embed_failed",
        };
        f.write_str(s)
    }
}

/// One typed runtime failure with an operator-facing detail string (no raw
/// model bytes, query text, or private paths — Plan 31 privacy boundary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeFailureV1 {
    pub kind: RuntimeFailureKindV1,
    pub detail: String,
}

impl fmt::Display for RuntimeFailureV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime failure ({}): {}", self.kind, self.detail)
    }
}

/// The manifest echo the runtime is allowed to see (Plan 31: the canonical
/// manifest lists model/tokenizer/config digests, runtime compatibility,
/// projection inputs, and the complete resource ceiling; the runtime path
/// performs no download, import, extraction, or trust decision).
///
/// The integrator builds this value from the verified manifest produced by
/// the artifact packet (`artifacts.rs`/`manifest.rs`); this quarantined
/// packet does not own the manifest schema and performs no verification
/// itself — verification is the artifact verifier's job upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedEmbeddingArtifactV1 {
    /// Signed artifact digest (opaque hex); keys the installed artifact in
    /// the Plan-02 user store. Also seeds deterministic fake vectors so the
    /// same model identity always produces the same pseudo-embeddings.
    pub artifact_digest: String,
    /// Installed location inside the Plan-02-owned user store. Never an
    /// ambient Hugging Face/ORT/FastEmbed cache directory.
    pub model_root: PathBuf,
    /// Manifest-declared member paths, relative to `model_root`.
    pub model_file: String,
    pub tokenizer_file: String,
    pub config_file: String,
    /// Declared embedding dimension.
    pub dimensions: u32,
    pub metric: EmbeddingMetricV1,
    pub normalization: EmbeddingNormalizationV1,
    pub precision: EmbeddingPrecisionV1,
    /// Manifest-pinned query/document instructions (Plan 31: the manifest
    /// pins query and document instructions/prefixes). The real adapter
    /// applies them as prefixes; the fake ignores them but echoes them so
    /// descriptor identity stays complete.
    pub query_instruction: Option<String>,
    pub document_instruction: Option<String>,
    /// Resource ceiling: maximum texts per batch.
    pub max_batch_texts: u32,
    /// Resource ceiling: maximum total sanitized bytes per batch.
    pub max_batch_bytes: u32,
    /// Resource ceiling: declared resident bytes per warmed session, used by
    /// the session pool's memory-ceiling enforcement.
    pub resident_byte_ceiling: u64,
    /// Runtime/backend/build revision echo (Plan 31: manifests pin
    /// runtime/build identity).
    pub runtime_build_revision: String,
}

impl VerifiedEmbeddingArtifactV1 {
    /// Structural validation of the descriptor itself. This is not artifact
    /// verification; the descriptor is already trusted input from the
    /// artifact verifier.
    pub fn validate(&self) -> Result<(), EmbedError> {
        if self.artifact_digest.is_empty() {
            return Err(EmbedError::InvalidArtifactDescriptor(
                "artifact_digest is empty".to_string(),
            ));
        }
        if self.dimensions == 0 || self.dimensions > MAX_EMBEDDING_DIMENSIONS {
            return Err(EmbedError::InvalidArtifactDescriptor(format!(
                "dimensions {} outside 1..={MAX_EMBEDDING_DIMENSIONS}",
                self.dimensions
            )));
        }
        for (field, value) in [
            ("model_file", &self.model_file),
            ("tokenizer_file", &self.tokenizer_file),
            ("config_file", &self.config_file),
        ] {
            if value.is_empty() {
                return Err(EmbedError::InvalidArtifactDescriptor(format!(
                    "{field} is empty"
                )));
            }
        }
        if self.max_batch_texts == 0 {
            return Err(EmbedError::InvalidArtifactDescriptor(
                "max_batch_texts is zero".to_string(),
            ));
        }
        if self.max_batch_bytes == 0 {
            return Err(EmbedError::InvalidArtifactDescriptor(
                "max_batch_bytes is zero".to_string(),
            ));
        }
        if self.resident_byte_ceiling == 0 {
            return Err(EmbedError::InvalidArtifactDescriptor(
                "resident_byte_ceiling is zero".to_string(),
            ));
        }
        if self.runtime_build_revision.is_empty() {
            return Err(EmbedError::InvalidArtifactDescriptor(
                "runtime_build_revision is empty".to_string(),
            ));
        }
        Ok(())
    }
}

/// One typed embedding vector. Dimensions, metric, and normalization are
/// echoed from the verified manifest descriptor, so a consumer can prove the
/// vector belongs to the declared projection without trusting the runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingVectorV1 {
    pub values: Vec<f32>,
    pub dimensions: u32,
    pub metric: EmbeddingMetricV1,
    pub normalization: EmbeddingNormalizationV1,
}

impl EmbeddingVectorV1 {
    /// Echo validation: declared dimension matches the payload and every
    /// value is finite (Plan 31: publication verifies dimensions and finite
    /// values; the runtime port fails fast on the same invariants).
    pub fn validate(&self) -> Result<(), EmbedError> {
        if self.values.len() != self.dimensions as usize {
            return Err(EmbedError::DimensionMismatch {
                expected: self.dimensions,
                actual: self.values.len(),
            });
        }
        if self.values.iter().any(|v| !v.is_finite()) {
            return Err(EmbedError::NonFiniteVectorValue);
        }
        Ok(())
    }

    /// Squared L2 norm of the payload (test and diagnostic aid).
    pub fn squared_l2_norm(&self) -> f32 {
        self.values.iter().map(|v| v * v).sum()
    }
}

/// One bounded batch of sanitized text ready for inference (Plan 31: a raw
/// query/source is sanitized into a bounded ephemeral view before model
/// inference; the batch bound comes from the manifest's resource ceiling).
///
/// Construction enforces the bound, so a value of this type is proof the
/// batch is within *some* declared ceiling. A session re-checks the batch
/// against its own descriptor's ceiling, which may be tighter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedSanitizedTextBatchV1 {
    texts: Vec<String>,
    total_bytes: usize,
}

impl BoundedSanitizedTextBatchV1 {
    pub fn try_new(
        texts: Vec<String>,
        max_texts: usize,
        max_bytes: usize,
    ) -> Result<Self, EmbedError> {
        if texts.is_empty() {
            return Err(EmbedError::EmptyBatch);
        }
        if texts.len() > max_texts {
            return Err(EmbedError::TooManyTexts {
                presented: texts.len(),
                max: max_texts,
            });
        }
        let total_bytes: usize = texts.iter().map(|t| t.len()).sum();
        if total_bytes > max_bytes {
            return Err(EmbedError::BatchBytesExceeded {
                presented: total_bytes,
                max: max_bytes,
            });
        }
        Ok(Self { texts, total_bytes })
    }

    pub fn texts(&self) -> &[String] {
        &self.texts
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn len(&self) -> usize {
        self.texts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }
}

/// Cancellation surface shared by the runtime and the session pool (Plan 31:
/// sessions are pooled under a bounded cancellation policy; cancellation can
/// never broaden scope or leave a partial batch visible).
pub trait CancellationSignal: Send + Sync {
    fn cancelled(&self) -> bool;
}

/// A manually flipped cancellation flag.
#[derive(Debug, Default)]
pub struct ManualCancellation {
    flag: AtomicBool,
}

impl ManualCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

impl CancellationSignal for ManualCancellation {
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// A deterministic scripted signal that reports cancelled once it has been
/// polled more than `cancel_after` times. `cancel_after = 0` cancels on the
/// first poll. Used to test mid-batch cancellation deterministically.
#[derive(Debug)]
pub struct ScriptedCancellation {
    checks: AtomicUsize,
    cancel_after: usize,
}

impl ScriptedCancellation {
    pub fn new(cancel_after: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            cancel_after,
        }
    }
}

impl CancellationSignal for ScriptedCancellation {
    fn cancelled(&self) -> bool {
        let poll = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        poll > self.cancel_after
    }
}

/// One warmed embedding session bound to one verified artifact descriptor
/// (Plan 31: compatible warmed sessions are pooled under bounded memory,
/// concurrency, idle, and cancellation policy).
pub trait EmbeddingSession: Send {
    /// The descriptor this session was opened from (echo surface).
    fn descriptor(&self) -> &VerifiedEmbeddingArtifactV1;
    /// Estimated resident bytes, used by the pool's memory-ceiling
    /// enforcement. Must be <= the descriptor's `resident_byte_ceiling`.
    fn resident_bytes_estimate(&self) -> u64;
    /// Embed one bounded sanitized batch. Returns one typed vector per input
    /// text, in input order. Implementations must honor the cancellation
    /// signal between texts and must return an error rather than a partial
    /// batch.
    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        cancel: &dyn CancellationSignal,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError>;
}

/// The root-private embedding runtime port (Plan 31: load verified artifact
/// → create session → embed bounded sanitized batches). The only production
/// implementation will be the FastEmbed adapter in this module; every other
/// crate depends on this trait surface, never on FastEmbed runtime types.
pub trait EmbeddingRuntime {
    type Session: EmbeddingSession;

    /// Cheap admission-time compatibility check (Plan 31: activation verifies
    /// runtime/platform compatibility before publishing). Performs no model
    /// load and no I/O beyond descriptor/platform inspection.
    fn verify_artifact_compatibility(
        &self,
        artifact: &VerifiedEmbeddingArtifactV1,
    ) -> Result<(), EmbedError>;

    /// Load the verified artifact and create one warmed session. The
    /// artifact bytes are already installed and verified by the artifact
    /// packet; this performs no download, import, extraction, cache
    /// discovery, or trust decision.
    fn open_session(
        &self,
        artifact: &VerifiedEmbeddingArtifactV1,
    ) -> Result<Self::Session, EmbedError>;
}

/// Test-observable counters for the deterministic fake runtime.
#[derive(Debug, Default)]
pub struct FakeRuntimeCounters {
    pub compatibility_checks: AtomicUsize,
    pub sessions_opened: AtomicUsize,
    pub sessions_closed: AtomicUsize,
    pub embed_calls: AtomicUsize,
    pub texts_embedded: AtomicUsize,
}

/// Deterministic offline implementation of [`EmbeddingRuntime`] (Plan 31:
/// "fake runtime ports" are part of this preparation packet). It loads no
/// model, performs no I/O and no network access, and produces hash-based
/// pseudo-embeddings with the descriptor's declared dimensions, metric, and
/// normalization. Same descriptor digest + same text always yields the same
/// vector, so all pool/session behavior is testable offline.
#[derive(Debug)]
pub struct FakeEmbeddingRuntime {
    resident_bytes_per_session: u64,
    open_failure: Option<RuntimeFailureKindV1>,
    compatibility_failure: Option<RuntimeFailureKindV1>,
    counters: Arc<FakeRuntimeCounters>,
}

impl Default for FakeEmbeddingRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEmbeddingRuntime {
    pub fn new() -> Self {
        Self {
            resident_bytes_per_session: 1024 * 1024,
            open_failure: None,
            compatibility_failure: None,
            counters: Arc::new(FakeRuntimeCounters::default()),
        }
    }

    pub fn with_resident_bytes_per_session(mut self, bytes: u64) -> Self {
        self.resident_bytes_per_session = bytes;
        self
    }

    pub fn with_open_failure(mut self, kind: RuntimeFailureKindV1) -> Self {
        self.open_failure = Some(kind);
        self
    }

    pub fn with_compatibility_failure(mut self, kind: RuntimeFailureKindV1) -> Self {
        self.compatibility_failure = Some(kind);
        self
    }

    pub fn counters(&self) -> Arc<FakeRuntimeCounters> {
        Arc::clone(&self.counters)
    }
}

impl EmbeddingRuntime for FakeEmbeddingRuntime {
    type Session = FakeEmbeddingSession;

    fn verify_artifact_compatibility(
        &self,
        artifact: &VerifiedEmbeddingArtifactV1,
    ) -> Result<(), EmbedError> {
        self.counters
            .compatibility_checks
            .fetch_add(1, Ordering::SeqCst);
        artifact.validate()?;
        if let Some(kind) = self.compatibility_failure {
            return Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind,
                detail: "scripted fake compatibility failure".to_string(),
            }));
        }
        Ok(())
    }

    fn open_session(
        &self,
        artifact: &VerifiedEmbeddingArtifactV1,
    ) -> Result<Self::Session, EmbedError> {
        artifact.validate()?;
        if let Some(kind) = self.open_failure {
            return Err(EmbedError::Runtime(RuntimeFailureV1 {
                kind,
                detail: "scripted fake open failure".to_string(),
            }));
        }
        self.counters.sessions_opened.fetch_add(1, Ordering::SeqCst);
        Ok(FakeEmbeddingSession {
            descriptor: artifact.clone(),
            vector_seed: fnv1a64(artifact.artifact_digest.as_bytes(), FNV_OFFSET_BASIS),
            resident_bytes: self.resident_bytes_per_session,
            counters: Arc::clone(&self.counters),
        })
    }
}

/// One deterministic fake warmed session.
#[derive(Debug)]
pub struct FakeEmbeddingSession {
    descriptor: VerifiedEmbeddingArtifactV1,
    vector_seed: u64,
    resident_bytes: u64,
    counters: Arc<FakeRuntimeCounters>,
}

impl EmbeddingSession for FakeEmbeddingSession {
    fn descriptor(&self) -> &VerifiedEmbeddingArtifactV1 {
        &self.descriptor
    }

    fn resident_bytes_estimate(&self) -> u64 {
        self.resident_bytes
    }

    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        cancel: &dyn CancellationSignal,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        if batch.is_empty() {
            return Err(EmbedError::EmptyBatch);
        }
        if batch.len() > self.descriptor.max_batch_texts as usize {
            return Err(EmbedError::TooManyTexts {
                presented: batch.len(),
                max: self.descriptor.max_batch_texts as usize,
            });
        }
        if batch.total_bytes() > self.descriptor.max_batch_bytes as usize {
            return Err(EmbedError::BatchBytesExceeded {
                presented: batch.total_bytes(),
                max: self.descriptor.max_batch_bytes as usize,
            });
        }
        let mut out = Vec::with_capacity(batch.len());
        for text in batch.texts() {
            // Honor cancellation between texts; a cancelled batch returns no
            // partial results.
            if cancel.cancelled() {
                return Err(EmbedError::Cancelled);
            }
            let vector = EmbeddingVectorV1 {
                values: pseudo_embedding(
                    self.vector_seed,
                    text,
                    self.descriptor.dimensions,
                    self.descriptor.normalization,
                ),
                dimensions: self.descriptor.dimensions,
                metric: self.descriptor.metric,
                normalization: self.descriptor.normalization,
            };
            vector.validate()?;
            self.counters.texts_embedded.fetch_add(1, Ordering::SeqCst);
            out.push(vector);
        }
        self.counters.embed_calls.fetch_add(1, Ordering::SeqCst);
        Ok(out)
    }
}

impl Drop for FakeEmbeddingSession {
    fn drop(&mut self) {
        self.counters.sessions_closed.fetch_add(1, Ordering::SeqCst);
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn xorshift64star(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// Deterministic hash-based pseudo-embedding: FNV-1a seeds a xorshift64*
/// stream from (model identity, text); each output maps into [-1, 1). L2
/// normalization is applied when the descriptor pins it. Pure arithmetic —
/// no HashMap iteration, no clock, no randomness — so results are stable
/// across runs on one platform.
fn pseudo_embedding(
    seed: u64,
    text: &str,
    dimensions: u32,
    normalization: EmbeddingNormalizationV1,
) -> Vec<f32> {
    let mut state = fnv1a64(text.as_bytes(), seed);
    if state == 0 {
        state = 0x9e37_79b9_7f4a_7c15;
    }
    let mut values: Vec<f32> = (0..dimensions)
        .map(|_| {
            let bits = xorshift64star(&mut state);
            // 24 high bits -> [0, 1) -> [-1, 1).
            ((bits >> 40) as f32 / 16_777_216.0_f32) * 2.0 - 1.0
        })
        .collect();
    if normalization == EmbeddingNormalizationV1::L2 {
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for value in &mut values {
                *value /= norm;
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(dimensions: u32) -> VerifiedEmbeddingArtifactV1 {
        VerifiedEmbeddingArtifactV1 {
            artifact_digest: "aa55aa55aa55aa55".to_string(),
            model_root: PathBuf::from("/plan02-store/artifacts/aa55"),
            model_file: "model.onnx".to_string(),
            tokenizer_file: "tokenizer.json".to_string(),
            config_file: "config.json".to_string(),
            dimensions,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::F32,
            query_instruction: Some("query:".to_string()),
            document_instruction: Some("passage:".to_string()),
            max_batch_texts: 8,
            max_batch_bytes: 16 * 1024,
            resident_byte_ceiling: 64 * 1024 * 1024,
            runtime_build_revision: "fastembed-test-rev-1".to_string(),
        }
    }

    fn batch(texts: &[&str]) -> BoundedSanitizedTextBatchV1 {
        BoundedSanitizedTextBatchV1::try_new(
            texts.iter().map(|t| (*t).to_string()).collect(),
            64,
            1 << 20,
        )
        .expect("batch within bounds")
    }

    fn never_cancelled() -> ManualCancellation {
        ManualCancellation::new()
    }

    #[test]
    fn descriptor_validation_accepts_valid_descriptor() {
        descriptor(384).validate().expect("valid descriptor");
    }

    #[test]
    fn descriptor_validation_rejects_zero_and_oversized_dimensions() {
        let mut d = descriptor(0);
        assert!(matches!(
            d.validate(),
            Err(EmbedError::InvalidArtifactDescriptor(_))
        ));
        d = descriptor(MAX_EMBEDDING_DIMENSIONS + 1);
        assert!(matches!(
            d.validate(),
            Err(EmbedError::InvalidArtifactDescriptor(_))
        ));
    }

    #[test]
    fn descriptor_validation_rejects_empty_identity_and_zero_ceilings() {
        for mutate in [
            (|d: &mut VerifiedEmbeddingArtifactV1| d.artifact_digest.clear())
                as fn(&mut VerifiedEmbeddingArtifactV1),
            |d| d.model_file.clear(),
            |d| d.tokenizer_file.clear(),
            |d| d.config_file.clear(),
            |d| d.max_batch_texts = 0,
            |d| d.max_batch_bytes = 0,
            |d| d.resident_byte_ceiling = 0,
            |d| d.runtime_build_revision.clear(),
        ] {
            let mut d = descriptor(384);
            mutate(&mut d);
            assert!(
                matches!(d.validate(), Err(EmbedError::InvalidArtifactDescriptor(_))),
                "mutation must be rejected"
            );
        }
    }

    #[test]
    fn batch_constructor_enforces_bounds() {
        assert!(matches!(
            BoundedSanitizedTextBatchV1::try_new(vec![], 4, 16),
            Err(EmbedError::EmptyBatch)
        ));
        assert!(matches!(
            BoundedSanitizedTextBatchV1::try_new(vec!["a".to_string(), "b".to_string()], 1, 16),
            Err(EmbedError::TooManyTexts {
                presented: 2,
                max: 1
            })
        ));
        assert!(matches!(
            BoundedSanitizedTextBatchV1::try_new(vec!["abcdef".to_string()], 4, 3),
            Err(EmbedError::BatchBytesExceeded {
                presented: 6,
                max: 3
            })
        ));
    }

    #[test]
    fn fake_embed_is_deterministic_across_sessions() {
        let runtime = FakeEmbeddingRuntime::new();
        let d = descriptor(16);
        let mut s1 = runtime.open_session(&d).expect("session 1");
        let mut s2 = runtime.open_session(&d).expect("session 2");
        let texts = batch(&["fn reserve_stock()", "impl Display for Error"]);
        let cancel = never_cancelled();
        let v1 = s1.embed_batch(&texts, &cancel).expect("embed 1");
        let v2 = s2.embed_batch(&texts, &cancel).expect("embed 2");
        assert_eq!(v1, v2, "same model identity + same text => same vector");
    }

    #[test]
    fn fake_embed_distinguishes_inputs_and_model_identities() {
        let runtime = FakeEmbeddingRuntime::new();
        let d = descriptor(16);
        let mut session = runtime.open_session(&d).expect("session");
        let cancel = never_cancelled();
        let pair = session
            .embed_batch(&batch(&["alpha", "beta"]), &cancel)
            .expect("embed");
        assert_ne!(pair[0].values, pair[1].values, "distinct texts differ");

        let mut other = d.clone();
        other.artifact_digest = "bb66bb66bb66bb66".to_string();
        let mut other_session = runtime.open_session(&other).expect("other session");
        let other_vec = other_session
            .embed_batch(&batch(&["alpha"]), &cancel)
            .expect("embed other");
        assert_ne!(
            pair[0].values, other_vec[0].values,
            "distinct model identities differ"
        );
    }

    #[test]
    fn echo_dimensions_metric_and_normalization_are_exact() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut d = descriptor(24);
        d.metric = EmbeddingMetricV1::DotProduct;
        d.normalization = EmbeddingNormalizationV1::L2;
        let mut session = runtime.open_session(&d).expect("session");
        let vectors = session
            .embed_batch(&batch(&["echo me"]), &never_cancelled())
            .expect("embed");
        assert_eq!(vectors.len(), 1);
        let v = &vectors[0];
        assert_eq!(v.values.len(), 24);
        assert_eq!(v.dimensions, 24);
        assert_eq!(v.metric, EmbeddingMetricV1::DotProduct);
        assert_eq!(v.normalization, EmbeddingNormalizationV1::L2);
        let norm = v.squared_l2_norm().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "L2-normalized vector has unit norm, got {norm}"
        );
    }

    #[test]
    fn unnormalized_echo_stays_raw() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut d = descriptor(24);
        d.normalization = EmbeddingNormalizationV1::None;
        let mut session = runtime.open_session(&d).expect("session");
        let vectors = session
            .embed_batch(&batch(&["raw values"]), &never_cancelled())
            .expect("embed");
        assert_eq!(vectors[0].normalization, EmbeddingNormalizationV1::None);
        assert!(
            vectors[0].values.iter().all(|v| (-1.0..1.0).contains(v)),
            "fake raw values stay in [-1, 1)"
        );
    }

    #[test]
    fn cancellation_before_embed_aborts() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut session = runtime.open_session(&descriptor(8)).expect("session");
        let cancel = ManualCancellation::new();
        cancel.cancel();
        let result = session.embed_batch(&batch(&["a", "b"]), &cancel);
        assert!(matches!(result, Err(EmbedError::Cancelled)));
        assert_eq!(
            runtime.counters().texts_embedded.load(Ordering::SeqCst),
            0,
            "no text embedded after pre-cancel"
        );
    }

    #[test]
    fn cancellation_mid_embed_discards_partial_batch() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut session = runtime.open_session(&descriptor(8)).expect("session");
        // First poll (before text 1) passes, second poll cancels.
        let cancel = ScriptedCancellation::new(1);
        let result = session.embed_batch(&batch(&["a", "b", "c", "d"]), &cancel);
        assert!(matches!(result, Err(EmbedError::Cancelled)));
        assert_eq!(
            runtime.counters().texts_embedded.load(Ordering::SeqCst),
            1,
            "exactly one text embedded before cancellation; no partial batch returned"
        );
    }

    #[test]
    fn session_enforces_its_own_manifest_batch_ceiling() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut d = descriptor(8);
        d.max_batch_texts = 1;
        let mut session = runtime.open_session(&d).expect("session");
        let result = session.embed_batch(&batch(&["a", "b"]), &never_cancelled());
        assert!(matches!(
            result,
            Err(EmbedError::TooManyTexts {
                presented: 2,
                max: 1
            })
        ));
    }

    #[test]
    fn vector_validation_rejects_bad_shape_and_nonfinite_values() {
        let mut v = EmbeddingVectorV1 {
            values: vec![0.0; 3],
            dimensions: 4,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
        };
        assert!(matches!(
            v.validate(),
            Err(EmbedError::DimensionMismatch {
                expected: 4,
                actual: 3
            })
        ));
        v.dimensions = 3;
        v.values[1] = f32::NAN;
        assert!(matches!(
            v.validate(),
            Err(EmbedError::NonFiniteVectorValue)
        ));
        v.values[1] = f32::INFINITY;
        assert!(matches!(
            v.validate(),
            Err(EmbedError::NonFiniteVectorValue)
        ));
    }

    #[test]
    fn open_failure_is_typed_and_disables_nothing_silently() {
        for kind in [
            RuntimeFailureKindV1::OutOfMemory,
            RuntimeFailureKindV1::CorruptArtifact,
            RuntimeFailureKindV1::RevokedArtifact,
            RuntimeFailureKindV1::IncompatibleRuntime,
            RuntimeFailureKindV1::LoadFailed,
            RuntimeFailureKindV1::EmbedFailed,
        ] {
            let runtime = FakeEmbeddingRuntime::new().with_open_failure(kind);
            let result = runtime.open_session(&descriptor(8));
            match result {
                Err(EmbedError::Runtime(failure)) => assert_eq!(failure.kind, kind),
                other => panic!("expected typed runtime failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn compatibility_failure_is_typed() {
        let runtime = FakeEmbeddingRuntime::new()
            .with_compatibility_failure(RuntimeFailureKindV1::IncompatibleRuntime);
        let result = runtime.verify_artifact_compatibility(&descriptor(8));
        match result {
            Err(EmbedError::Runtime(failure)) => {
                assert_eq!(failure.kind, RuntimeFailureKindV1::IncompatibleRuntime)
            }
            other => panic!("expected typed compatibility failure, got {other:?}"),
        }
    }

    #[test]
    fn compatibility_check_validates_descriptor_first() {
        let runtime = FakeEmbeddingRuntime::new();
        let mut d = descriptor(8);
        d.dimensions = 0;
        assert!(matches!(
            runtime.verify_artifact_compatibility(&d),
            Err(EmbedError::InvalidArtifactDescriptor(_))
        ));
        assert_eq!(
            runtime
                .counters()
                .compatibility_checks
                .load(Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn metric_normalization_precision_enumerations_render_stably() {
        // The closed enumerations are the manifest echo vocabulary (see
        // ESCALATION-2); every variant stays constructible and renders.
        for metric in [
            EmbeddingMetricV1::Cosine,
            EmbeddingMetricV1::DotProduct,
            EmbeddingMetricV1::EuclideanL2,
        ] {
            assert!(!metric.to_string().is_empty());
        }
        for normalization in [EmbeddingNormalizationV1::None, EmbeddingNormalizationV1::L2] {
            assert!(!normalization.to_string().is_empty());
        }
        for precision in [
            EmbeddingPrecisionV1::F32,
            EmbeddingPrecisionV1::F16,
            EmbeddingPrecisionV1::QuantizedI8,
        ] {
            assert!(!precision.to_string().is_empty());
        }
    }

    #[test]
    fn fake_reports_resident_bytes_and_close_counters() {
        let runtime = FakeEmbeddingRuntime::new().with_resident_bytes_per_session(4096);
        let counters = runtime.counters();
        {
            let session = runtime.open_session(&descriptor(8)).expect("session");
            assert_eq!(session.resident_bytes_estimate(), 4096);
            assert_eq!(counters.sessions_opened.load(Ordering::SeqCst), 1);
            assert_eq!(counters.sessions_closed.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            counters.sessions_closed.load(Ordering::SeqCst),
            1,
            "closing a session is observable"
        );
    }
}
