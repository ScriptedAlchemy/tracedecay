//! Model2Vec static-embedding runtime adapter.
//!
//! A Model2Vec model is a token-embedding table (`model.safetensors`, tensor
//! `embeddings`, shape `[rows, dimension]`) plus a `tokenizers` JSON. There
//! is no graph and no ONNX Runtime: a text's vector is the mean of the table
//! rows for its (unknown-filtered, truncated) token ids, L2-normalized.
//!
//! Semantics follow the upstream `model2vec.StaticModel.encode` reference:
//! tokenize with `add_special_tokens = false`, drop every id equal to the
//! tokenizer's unknown-token id, then truncate to the projection's
//! `truncation_length`; an empty id list yields the all-zero vector. The
//! tokenizer's own serialized truncation/padding settings are disabled so
//! the admitted projection identity is the only truncation authority.
//!
//! Like the `FastEmbed` adapter this runtime is feature-gated
//! (`semantic-model2vec`) and initializes only from bytes opened through the
//! digest-verified lifecycle install or artifact-store authority.
#[cfg(feature = "semantic-model2vec")]
use std::fmt;

#[cfg(feature = "semantic-model2vec")]
use half::f16;
#[cfg(feature = "semantic-model2vec")]
use safetensors::{Dtype, SafeTensors};
#[cfg(feature = "semantic-model2vec")]
use serde::Deserialize;
#[cfg(feature = "semantic-model2vec")]
use tokenizers::{ModelWrapper, Tokenizer};
#[cfg(feature = "semantic-model2vec")]
use tracedecay_domain::{EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1};
#[cfg(feature = "semantic-model2vec")]
use tracedecay_semantic_contracts::ArtifactMemberRoleV1;

#[cfg(feature = "semantic-model2vec")]
use super::embedding_backend::EmbeddingRuntimeFamilyV1;
use super::fastembed_adapter::{
    AdmittedProjectionArtifactV1, BoundedSanitizedTextBatchV1, EmbedError, EmbeddingRuntime,
    EmbeddingSession, EmbeddingVectorV1, RuntimeFailureKindV1, RuntimeFailureV1,
    SemanticExecutionAuthority,
};
#[cfg(feature = "semantic-model2vec")]
use super::fastembed_adapter::{check_execution_authority, validate_batch_limits};

pub const MODEL2VEC_RUNTIME_FAMILY_V1: &str = "model2vec-static";
/// Exact runtime identity recorded in Model2Vec projection keys. It names
/// this adapter's algorithm revision and the exact crate versions pinned in
/// Cargo.toml that decode the table and tokenize input; any change to either
/// must bump it so vector generations replay under the new identity.
pub const MODEL2VEC_RUNTIME_BUILD_REVISION_V1: &str =
    "tracedecay-model2vec-1+tokenizers-0.22.2+safetensors-0.8.0+half-2.7.1";

/// Name of the embedding-table tensor inside `model.safetensors`.
#[cfg(feature = "semantic-model2vec")]
const EMBEDDINGS_TENSOR_NAME: &str = "embeddings";

fn model2vec_failure(kind: RuntimeFailureKindV1, detail: &str) -> EmbedError {
    EmbedError::Runtime(RuntimeFailureV1 {
        kind,
        detail: detail.to_owned(),
    })
}

#[cfg(feature = "semantic-model2vec")]
fn model2vec_error(
    kind: RuntimeFailureKindV1,
    detail: &str,
    error: &impl fmt::Display,
) -> EmbedError {
    let message = error.to_string().to_ascii_lowercase();
    let kind = if message.contains("out of memory") || message.contains("allocation") {
        RuntimeFailureKindV1::OutOfMemory
    } else {
        kind
    };
    model2vec_failure(kind, detail)
}

/// The production Model2Vec runtime.
#[cfg(feature = "semantic-model2vec")]
#[derive(Default)]
pub struct Model2VecEmbeddingRuntime;

/// Feature-disabled stand-in: keeps every consumer type-compatible while the
/// `semantic-model2vec` dependencies are compiled out. Every operation fails
/// with a typed runtime failure.
#[cfg(not(feature = "semantic-model2vec"))]
#[derive(Default)]
pub struct Model2VecEmbeddingRuntime;

/// Uninhabited session type for the feature-disabled runtime.
#[cfg(not(feature = "semantic-model2vec"))]
pub enum UnavailableModel2VecSession {}

#[cfg(not(feature = "semantic-model2vec"))]
impl EmbeddingSession for UnavailableModel2VecSession {
    fn authority(&self) -> &AdmittedProjectionArtifactV1 {
        match *self {}
    }

    fn resident_bytes_estimate(&self) -> u64 {
        match *self {}
    }

    fn embed_batch(
        &mut self,
        _batch: &BoundedSanitizedTextBatchV1,
        _authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        match *self {}
    }
}

#[cfg(not(feature = "semantic-model2vec"))]
impl EmbeddingRuntime for Model2VecEmbeddingRuntime {
    type Session = UnavailableModel2VecSession;

    fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64 {
        authority.resident_bytes_estimate()
    }

    fn verify_artifact_compatibility(
        &self,
        _authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError> {
        Err(model2vec_failure(
            RuntimeFailureKindV1::IncompatibleRuntime,
            "the semantic-model2vec feature is compiled out of this build",
        ))
    }

    fn open_session(
        &self,
        _authority: &AdmittedProjectionArtifactV1,
        _interruption: &dyn SemanticExecutionAuthority,
    ) -> Result<Self::Session, EmbedError> {
        Err(model2vec_failure(
            RuntimeFailureKindV1::IncompatibleRuntime,
            "the semantic-model2vec feature is compiled out of this build",
        ))
    }
}

/// Upstream `config.json` facts that pin vector semantics. Other keys
/// (`embedding_dtype`, `max_length`, model-card hints) are deliberately not
/// read: the safetensors dtype and the admitted projection are the
/// authorities for precision and truncation.
#[cfg(feature = "semantic-model2vec")]
#[derive(Deserialize)]
struct Model2VecConfigV1 {
    normalize: bool,
}

/// One text's token ids after unknown-token filtering and truncation.
#[cfg(feature = "semantic-model2vec")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TokenizedTextV1 {
    pub(crate) ids: Vec<u32>,
    pub(crate) truncated: bool,
}

/// The decoded embedding table in its stored precision. Rows are decoded to
/// `f32` only while they are summed, so a session retains exactly the
/// table's declared byte size.
#[cfg(feature = "semantic-model2vec")]
enum StaticEmbeddingTableV1 {
    F16 { dimension: usize, values: Vec<f16> },
    F32 { dimension: usize, values: Vec<f32> },
}

#[cfg(feature = "semantic-model2vec")]
impl StaticEmbeddingTableV1 {
    fn dimension(&self) -> usize {
        match self {
            Self::F16 { dimension, .. } | Self::F32 { dimension, .. } => *dimension,
        }
    }

    fn element_count(&self) -> usize {
        match self {
            Self::F16 { values, .. } => values.len(),
            Self::F32 { values, .. } => values.len(),
        }
    }

    /// Add table row `row` into `accumulator`. A row outside the table is a
    /// corrupt artifact (the tokenizer produced an id the table has no row
    /// for), never an index panic.
    fn accumulate_row(&self, row: usize, accumulator: &mut [f32]) -> Result<(), EmbedError> {
        let dimension = self.dimension();
        let (start, end) = row
            .checked_mul(dimension)
            .and_then(|start| start.checked_add(dimension).map(|end| (start, end)))
            .filter(|(_, end)| *end <= self.element_count())
            .ok_or_else(|| {
                model2vec_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "tokenizer produced a token id outside the embedding table",
                )
            })?;
        match self {
            Self::F16 { values, .. } => {
                for (target, value) in accumulator.iter_mut().zip(&values[start..end]) {
                    *target += value.to_f32();
                }
            }
            Self::F32 { values, .. } => {
                for (target, value) in accumulator.iter_mut().zip(&values[start..end]) {
                    *target += *value;
                }
            }
        }
        Ok(())
    }

    fn decode(
        bytes: &[u8],
        expected_dimension: usize,
        expected_precision: EmbeddingPrecisionV1,
    ) -> Result<Self, EmbedError> {
        let tensors = SafeTensors::deserialize(bytes).map_err(|error| {
            model2vec_error(
                RuntimeFailureKindV1::CorruptArtifact,
                "model member is not a valid safetensors file",
                &error,
            )
        })?;
        let tensor = tensors.tensor(EMBEDDINGS_TENSOR_NAME).map_err(|error| {
            model2vec_error(
                RuntimeFailureKindV1::CorruptArtifact,
                "model member lacks the embeddings tensor",
                &error,
            )
        })?;
        let (rows, dimension) = match tensor.shape() {
            [rows, dimension] => (*rows, *dimension),
            _ => {
                return Err(model2vec_failure(
                    RuntimeFailureKindV1::CorruptArtifact,
                    "embeddings tensor is not a two-dimensional table",
                ));
            }
        };
        if rows == 0 || dimension == 0 || dimension != expected_dimension {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "embeddings tensor shape does not match the cataloged dimensions",
            ));
        }
        let element_count = rows.checked_mul(dimension).ok_or_else(|| {
            model2vec_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "embeddings tensor shape overflows",
            )
        })?;
        let data = tensor.data();
        match (tensor.dtype(), expected_precision) {
            (Dtype::F16, EmbeddingPrecisionV1::Fp16) => {
                if data.len() != element_count.saturating_mul(2) {
                    return Err(model2vec_failure(
                        RuntimeFailureKindV1::CorruptArtifact,
                        "embeddings tensor byte length does not match its shape",
                    ));
                }
                let values = data
                    .chunks_exact(2)
                    .map(|pair| {
                        <[u8; 2]>::try_from(pair)
                            .map(f16::from_le_bytes)
                            .map_err(|_| {
                                model2vec_failure(
                                    RuntimeFailureKindV1::CorruptArtifact,
                                    "embeddings tensor has a torn fp16 element",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, EmbedError>>()?;
                Ok(Self::F16 { dimension, values })
            }
            (Dtype::F32, EmbeddingPrecisionV1::Fp32) => {
                if data.len() != element_count.saturating_mul(4) {
                    return Err(model2vec_failure(
                        RuntimeFailureKindV1::CorruptArtifact,
                        "embeddings tensor byte length does not match its shape",
                    ));
                }
                let values = data
                    .chunks_exact(4)
                    .map(|quad| {
                        <[u8; 4]>::try_from(quad)
                            .map(f32::from_le_bytes)
                            .map_err(|_| {
                                model2vec_failure(
                                    RuntimeFailureKindV1::CorruptArtifact,
                                    "embeddings tensor has a torn fp32 element",
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, EmbedError>>()?;
                Ok(Self::F32 { dimension, values })
            }
            _ => Err(model2vec_failure(
                RuntimeFailureKindV1::CorruptArtifact,
                "embeddings tensor dtype does not match the cataloged table precision",
            )),
        }
    }
}

/// A loaded static model: verified tokenizer + table + the pins that shape
/// every vector it produces.
#[cfg(feature = "semantic-model2vec")]
pub(crate) struct StaticEmbeddingModelV1 {
    tokenizer: Tokenizer,
    unk_id: Option<u32>,
    table: StaticEmbeddingTableV1,
    truncation_length: usize,
}

#[cfg(feature = "semantic-model2vec")]
impl StaticEmbeddingModelV1 {
    pub(crate) fn load(
        config_bytes: &[u8],
        tokenizer_bytes: &[u8],
        table_bytes: &[u8],
        expected_dimension: usize,
        expected_precision: EmbeddingPrecisionV1,
        truncation_length: usize,
    ) -> Result<Self, EmbedError> {
        let config: Model2VecConfigV1 = serde_json::from_slice(config_bytes).map_err(|error| {
            model2vec_error(
                RuntimeFailureKindV1::CorruptArtifact,
                "config member is not a Model2Vec config",
                &error,
            )
        })?;
        // The projection pins L2 normalization; a table published without
        // it would produce vectors the pinned identity does not describe.
        if !config.normalize {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "Model2Vec config disables normalization but the projection pins L2",
            ));
        }
        let mut tokenizer = Tokenizer::from_bytes(tokenizer_bytes).map_err(|error| {
            model2vec_error(
                RuntimeFailureKindV1::CorruptArtifact,
                "tokenizer member is not a valid tokenizers JSON",
                &error,
            )
        })?;
        tokenizer
            .with_truncation(None)
            .map_err(|error| {
                model2vec_error(
                    RuntimeFailureKindV1::LoadFailed,
                    "tokenizer refused to disable serialized truncation",
                    &error,
                )
            })?
            .with_padding(None);
        let unk_id = unknown_token_id(&tokenizer)?;
        let table =
            StaticEmbeddingTableV1::decode(table_bytes, expected_dimension, expected_precision)?;
        Ok(Self {
            tokenizer,
            unk_id,
            table,
            truncation_length,
        })
    }

    pub(crate) fn tokenize(&self, text: &str) -> Result<TokenizedTextV1, EmbedError> {
        let encoding = self.tokenizer.encode_fast(text, false).map_err(|error| {
            model2vec_error(
                RuntimeFailureKindV1::EmbedFailed,
                "Model2Vec tokenization failed",
                &error,
            )
        })?;
        let mut ids: Vec<u32> = encoding
            .get_ids()
            .iter()
            .copied()
            .filter(|id| Some(*id) != self.unk_id)
            .collect();
        let truncated = ids.len() > self.truncation_length;
        ids.truncate(self.truncation_length);
        Ok(TokenizedTextV1 { ids, truncated })
    }

    /// Mean of the table rows for `ids`, L2-normalized. No ids yields the
    /// all-zero vector; the zero norm is never divided through.
    pub(crate) fn pool(&self, ids: &[u32]) -> Result<Vec<f32>, EmbedError> {
        let mut vector = vec![0.0_f32; self.table.dimension()];
        if ids.is_empty() {
            return Ok(vector);
        }
        for id in ids {
            self.table.accumulate_row(*id as usize, &mut vector)?;
        }
        let count = ids.len() as f32;
        for value in &mut vector {
            *value /= count;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 && norm.is_finite() {
            for value in &mut vector {
                *value /= norm;
            }
        }
        // Canonicalize IEEE negative zero before vector hashing and
        // exact-flat comparison; this changes no distance.
        for value in &mut vector {
            if *value == 0.0 {
                *value = 0.0;
            }
        }
        Ok(vector)
    }

    pub(crate) fn embed(&self, text: &str) -> Result<(Vec<f32>, bool), EmbedError> {
        let tokens = self.tokenize(text)?;
        let vector = self.pool(&tokens.ids)?;
        Ok((vector, tokens.truncated))
    }
}

/// Resolve the tokenizer model's unknown token to its id, mirroring the
/// upstream reference: models that declare no unknown token (or whose
/// vocabulary lacks it) filter nothing. Unigram models keep their unknown
/// id private in `tokenizers`, so they are a typed incompatibility rather
/// than a silent no-filter.
#[cfg(feature = "semantic-model2vec")]
fn unknown_token_id(tokenizer: &Tokenizer) -> Result<Option<u32>, EmbedError> {
    let unk_token = match tokenizer.get_model() {
        ModelWrapper::WordPiece(model) => Some(model.unk_token.as_str()),
        ModelWrapper::WordLevel(model) => Some(model.unk_token.as_str()),
        ModelWrapper::BPE(model) => model.unk_token.as_deref(),
        ModelWrapper::Unigram(_) => {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "Model2Vec runtime does not support Unigram tokenizers",
            ));
        }
    };
    Ok(unk_token
        .filter(|token| !token.is_empty())
        .and_then(|token| tokenizer.token_to_id(token)))
}

#[cfg(feature = "semantic-model2vec")]
impl EmbeddingRuntime for Model2VecEmbeddingRuntime {
    type Session = Model2VecEmbeddingSession;

    fn resident_bytes_reservation(&self, authority: &AdmittedProjectionArtifactV1) -> u64 {
        authority.resident_bytes_estimate()
    }

    fn verify_artifact_compatibility(
        &self,
        authority: &AdmittedProjectionArtifactV1,
    ) -> Result<(), EmbedError> {
        if authority.runtime_family() != EmbeddingRuntimeFamilyV1::Model2VecStatic {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "the projection does not select the Model2Vec static backend",
            ));
        }
        let artifact = authority.runtime_artifact();
        let key = artifact.embedding_key();
        if key.runtime_build_revision != MODEL2VEC_RUNTIME_BUILD_REVISION_V1 {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "the projection was produced by a different Model2Vec runtime revision",
            ));
        }
        if key.normalization != EmbeddingNormalizationV1::L2 {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "Model2Vec always returns L2-normalized vectors",
            ));
        }
        if key.pooling != EmbeddingPoolingV1::Mean {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "Model2Vec always mean-pools token rows",
            ));
        }
        if !matches!(
            key.precision,
            EmbeddingPrecisionV1::Fp16 | EmbeddingPrecisionV1::Fp32
        ) {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "Model2Vec tables are decoded only from fp16 or fp32 safetensors",
            ));
        }
        if key.dimensions == 0 || key.truncation_length == 0 {
            return Err(model2vec_failure(
                RuntimeFailureKindV1::IncompatibleRuntime,
                "the projection declares no vector dimensions or token budget",
            ));
        }
        for role in [
            ArtifactMemberRoleV1::Model,
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            if !artifact.declares_member(role) {
                return Err(model2vec_failure(
                    RuntimeFailureKindV1::IncompatibleRuntime,
                    "the artifact lacks a Model2Vec-required member",
                ));
            }
        }
        Ok(())
    }

    #[hotpath::measure(label = "semantic.embedding.open_session")]
    fn open_session(
        &self,
        authority: &AdmittedProjectionArtifactV1,
        interruption: &dyn SemanticExecutionAuthority,
    ) -> Result<Self::Session, EmbedError> {
        check_execution_authority(interruption)?;
        self.verify_artifact_compatibility(authority)?;
        let artifact = authority.runtime_artifact();
        // Each member read is a disk read + digest recheck; an abandoned
        // load stops before the next one.
        let member_bytes = |role: ArtifactMemberRoleV1| {
            check_execution_authority(interruption)?;
            artifact.required_member_bytes(role)
        };
        let (config, tokenizer, table) = hotpath::measure_block!("semantic.model.member_bytes", {
            let config = member_bytes(ArtifactMemberRoleV1::Config)?;
            let tokenizer = member_bytes(ArtifactMemberRoleV1::Tokenizer)?;
            let table = member_bytes(ArtifactMemberRoleV1::Model)?;
            Ok::<_, EmbedError>((config, tokenizer, table))
        })?;
        hotpath::gauge!("semantic_model_member_bytes").set(table.len());
        // Last boundary before decode: drop the buffered bytes instead of
        // building a tokenizer and table nobody will use.
        check_execution_authority(interruption)?;
        let key = artifact.embedding_key();
        let model = hotpath::measure_block!(
            "semantic.model.load",
            StaticEmbeddingModelV1::load(
                &config,
                &tokenizer,
                &table,
                key.dimensions as usize,
                key.precision,
                key.truncation_length as usize,
            )
        )
        .inspect_err(|failure| crate::hotpath_observe::record_embed_error(failure))?;
        check_execution_authority(interruption)?;
        crate::hotpath_observe::record_model_state("ready");
        Ok(Model2VecEmbeddingSession {
            authority: authority.clone(),
            model,
        })
    }
}

#[cfg(feature = "semantic-model2vec")]
pub struct Model2VecEmbeddingSession {
    authority: AdmittedProjectionArtifactV1,
    model: StaticEmbeddingModelV1,
}

#[cfg(feature = "semantic-model2vec")]
impl EmbeddingSession for Model2VecEmbeddingSession {
    fn authority(&self) -> &AdmittedProjectionArtifactV1 {
        &self.authority
    }

    fn resident_bytes_estimate(&self) -> u64 {
        self.authority.resident_bytes_estimate()
    }

    #[hotpath::measure(label = "semantic.embedding.embed_batch")]
    fn embed_batch(
        &mut self,
        batch: &BoundedSanitizedTextBatchV1,
        authority: &dyn SemanticExecutionAuthority,
    ) -> Result<Vec<EmbeddingVectorV1>, EmbedError> {
        let artifact = self.authority.runtime_artifact();
        validate_batch_limits(batch, artifact)?;
        hotpath::gauge!("semantic_embed_batch_size").set(batch.len());
        hotpath::gauge!("semantic_embed_batch_bytes").set(batch.total_bytes());
        let mut vectors = Vec::with_capacity(batch.len());
        let mut truncated_texts = 0_usize;
        for text in batch.texts() {
            // Static embedding is per text, so the interruption authority is
            // honored between every text rather than once per tensor.
            check_execution_authority(authority)?;
            let (values, truncated) = self
                .model
                .embed(text)
                .inspect_err(|failure| crate::hotpath_observe::record_embed_error(failure))?;
            truncated_texts += usize::from(truncated);
            let vector = EmbeddingVectorV1 {
                values,
                dimensions: artifact.dimensions(),
                metric: artifact.metric(),
                normalization: artifact.normalization(),
            };
            vector.validate()?;
            vectors.push(vector);
        }
        hotpath::gauge!("semantic_embed_truncated_texts").set(truncated_texts);
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_build_revision_names_the_pinned_decoder_versions() {
        let manifest = include_str!("../Cargo.toml");
        let pinned = |dependency: &str| {
            manifest
                .lines()
                .find_map(|line| {
                    let rest = line.trim().strip_prefix(dependency)?.trim_start();
                    let rest = rest.strip_prefix('=')?.trim_start();
                    let (_, rest) = rest.split_once("version = \"=")?;
                    rest.split_once('"').map(|(version, _)| version.to_owned())
                })
                .unwrap_or_else(|| {
                    panic!("tracedecay-semantic must pin an exact {dependency} version")
                })
        };
        for (dependency, prefix) in [
            ("tokenizers", "tokenizers-"),
            ("safetensors", "safetensors-"),
            ("half", "half-"),
        ] {
            let version = pinned(dependency);
            assert!(
                MODEL2VEC_RUNTIME_BUILD_REVISION_V1.contains(&format!("+{prefix}{version}")),
                "MODEL2VEC_RUNTIME_BUILD_REVISION_V1 ({MODEL2VEC_RUNTIME_BUILD_REVISION_V1}) must \
                 record the exact pinned {dependency} version ({version})"
            );
        }
    }
}

#[cfg(all(test, feature = "semantic-model2vec"))]
#[path = "model2vec_adapter/tests.rs"]
mod inference_tests;
