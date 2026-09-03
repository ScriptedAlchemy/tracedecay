//! Immutable semantic embedding model catalog for `TraceDecay` selection.
//!
//! Catalog entries pin source revision, license, member lengths, SHA-256
//! digests, and the embedding backend that serves the model. There are no
//! signatures or trust roots — integrity is the declared length + digest
//! identity, matching the distribution fixture. The backend declaration is
//! the single production selection authority: lifecycle projection identity
//! and the runtime dispatcher both derive from it, so a model can never be
//! served by a runtime other than the one its entry names.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::EmbeddingPrecisionV1;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_semantic_contracts::{
    ArtifactMemberRoleV1, DEFAULT_FASTEMBED_MODEL_ID, MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID,
};

use super::embedding_backend::EmbeddingRuntimeFamilyV1;

const CATALOG_SCHEMA_V1: &str = "tracedecay.fastembed.model-catalog.v1";

/// One immutable package member pin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogMemberPinV1 {
    pub path: String,
    pub upstream_path: String,
    pub length: u64,
    pub sha256: String,
}

/// Provenance for a cataloged model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSourceV1 {
    pub upstream: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub provenance: String,
}

/// The embedding backend a catalog entry is served by. Each variant carries
/// exactly the backend-specific facts the runtime needs before it has read a
/// single member byte, so projection identity is fixed by the catalog alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "runtime", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogedEmbeddingBackendV1 {
    /// `FastEmbed` over ONNX Runtime; `fastembed_enum` names the upstream
    /// `EmbeddingModel` variant the pinned package corresponds to.
    FastEmbedOrt { fastembed_enum: String },
    /// Model2Vec static token-embedding table: the `embeddings` tensor of
    /// `model.safetensors`, mean-pooled and L2-normalized in-process.
    /// `table_precision` is the tensor's stored dtype, verified at load.
    Model2VecStatic {
        table_precision: EmbeddingPrecisionV1,
    },
}

impl CatalogedEmbeddingBackendV1 {
    pub fn runtime_family(&self) -> EmbeddingRuntimeFamilyV1 {
        match self {
            Self::FastEmbedOrt { .. } => EmbeddingRuntimeFamilyV1::FastEmbedOrt,
            Self::Model2VecStatic { .. } => EmbeddingRuntimeFamilyV1::Model2VecStatic,
        }
    }

    /// Vector precision recorded in the projection identity. `FastEmbed`
    /// always runs the ONNX graph in fp32; Model2Vec vectors carry the
    /// stored table precision because the table is the whole model.
    pub fn precision(&self) -> EmbeddingPrecisionV1 {
        match self {
            Self::FastEmbedOrt { .. } => EmbeddingPrecisionV1::Fp32,
            Self::Model2VecStatic { table_precision } => *table_precision,
        }
    }

    /// Catalog member roles the backend must find in an install before it
    /// can open a session.
    pub fn required_member_roles(&self) -> &'static [&'static str] {
        match self {
            Self::FastEmbedOrt { .. } => &[
                "model",
                "tokenizer",
                "config",
                "special_tokens_map",
                "tokenizer_config",
            ],
            Self::Model2VecStatic { .. } => &["model", "tokenizer", "config"],
        }
    }
}

/// One supported embedding model that settings may select.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogedFastEmbedModelV1 {
    pub model_id: String,
    pub backend: CatalogedEmbeddingBackendV1,
    pub model_code: String,
    pub source: CatalogSourceV1,
    pub expected_dimensions: u32,
    pub max_length: u32,
    pub members: BTreeMap<String, CatalogMemberPinV1>,
}

/// Versioned catalog of supported embedding models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FastEmbedModelCatalogV1 {
    pub schema: String,
    pub models: Vec<CatalogedFastEmbedModelV1>,
}

/// Production catalog used by settings validation and daemon acquisition.
#[cfg(any(test, feature = "test-helpers"))]
pub fn production_fastembed_catalog() -> FastEmbedModelCatalogV1 {
    FastEmbedModelCatalogV1::production()
}

impl FastEmbedModelCatalogV1 {
    pub fn production() -> Self {
        Self {
            schema: CATALOG_SCHEMA_V1.to_owned(),
            models: vec![jina_embeddings_v2_base_code(), potion_code_16m_v2()],
        }
    }

    pub fn get(&self, model_id: &str) -> Option<&CatalogedFastEmbedModelV1> {
        self.models.iter().find(|model| model.model_id == model_id)
    }

    pub fn model_ids(&self) -> impl Iterator<Item = &str> {
        self.models.iter().map(|model| model.model_id.as_str())
    }

    pub fn validate(&self) -> Result<(), CatalogErrorV1> {
        if self.schema != CATALOG_SCHEMA_V1 {
            return Err(CatalogErrorV1::InvalidSchema);
        }
        if self.models.is_empty() {
            return Err(CatalogErrorV1::Empty);
        }
        let mut seen = BTreeSet::new();
        for model in &self.models {
            validate_model(model)?;
            if !seen.insert(model.model_id.as_str()) {
                return Err(CatalogErrorV1::DuplicateModelId);
            }
        }
        if self.get(DEFAULT_FASTEMBED_MODEL_ID).is_none() {
            return Err(CatalogErrorV1::MissingDefault);
        }
        Ok(())
    }
}

/// Catalog construction / lookup failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CatalogErrorV1 {
    #[error("semantic model catalog schema is invalid")]
    InvalidSchema,
    #[error("semantic model catalog is empty")]
    Empty,
    #[error("semantic model catalog has a duplicate model id")]
    DuplicateModelId,
    #[error("semantic model catalog omits the default model")]
    MissingDefault,
    #[error("semantic model catalog entry is invalid")]
    InvalidEntry,
    #[error("selected semantic model is not in the catalog")]
    UnknownModel,
}

fn validate_model(model: &CatalogedFastEmbedModelV1) -> Result<(), CatalogErrorV1> {
    if model.model_id.trim().is_empty()
        || model.model_code.trim().is_empty()
        || model.expected_dimensions == 0
        || model.max_length == 0
        || model.members.is_empty()
    {
        return Err(CatalogErrorV1::InvalidEntry);
    }
    match &model.backend {
        CatalogedEmbeddingBackendV1::FastEmbedOrt { fastembed_enum } => {
            if fastembed_enum.trim().is_empty() {
                return Err(CatalogErrorV1::InvalidEntry);
            }
        }
        CatalogedEmbeddingBackendV1::Model2VecStatic { table_precision } => {
            // The static runtime decodes exactly the two safetensors dtypes
            // `half`/`f32` represent; a catalog entry must not promise a
            // table precision the runtime cannot verify at load.
            if !matches!(
                table_precision,
                EmbeddingPrecisionV1::Fp16 | EmbeddingPrecisionV1::Fp32
            ) {
                return Err(CatalogErrorV1::InvalidEntry);
            }
        }
    }
    if model
        .backend
        .required_member_roles()
        .iter()
        .any(|role| !model.members.contains_key(*role))
    {
        return Err(CatalogErrorV1::InvalidEntry);
    }
    if model.source.revision.len() != 40
        || !model
            .source
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CatalogErrorV1::InvalidEntry);
    }
    for member in model.members.values() {
        if member.path.trim().is_empty()
            || member.upstream_path.trim().is_empty()
            || member.upstream_path.contains("..")
            || member.length == 0
            || member.sha256.len() != 64
            || !member
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CatalogErrorV1::InvalidEntry);
        }
    }
    Ok(())
}

/// Production pin for `FastEmbed` `EmbeddingModel::JinaEmbeddingsV2BaseCode`.
///
/// Digests and lengths match `tests/distribution/fastembed/fixture.json`.
fn jina_embeddings_v2_base_code() -> CatalogedFastEmbedModelV1 {
    let mut members = BTreeMap::new();
    members.insert(
        "model".to_owned(),
        CatalogMemberPinV1 {
            path: "model.onnx".to_owned(),
            upstream_path: "onnx/model.onnx".to_owned(),
            length: 641_517_466,
            sha256: "63363fc178428b74620c6f3780cbc7191883fa5c7f84c0945c45eb5c4256733b".to_owned(),
        },
    );
    members.insert(
        "tokenizer".to_owned(),
        CatalogMemberPinV1 {
            path: "tokenizer.json".to_owned(),
            upstream_path: "tokenizer.json".to_owned(),
            length: 2_561_316,
            sha256: "b01c78a902aa4facb2f47f95449f48e2f7bbfea5d2472ee2f6ce92323c6f86e5".to_owned(),
        },
    );
    members.insert(
        "config".to_owned(),
        CatalogMemberPinV1 {
            path: "config.json".to_owned(),
            upstream_path: "config.json".to_owned(),
            length: 1_216,
            sha256: "e426aa684c7f9a95c5f020aa855faf93a24f065f5fad0c9e17b124670cabdea6".to_owned(),
        },
    );
    members.insert(
        "special_tokens_map".to_owned(),
        CatalogMemberPinV1 {
            path: "special_tokens_map.json".to_owned(),
            upstream_path: "special_tokens_map.json".to_owned(),
            length: 280,
            sha256: "06e405a36dfe4b9604f484f6a1e619af1a7f7d09e34a8555eb0b77b66318067f".to_owned(),
        },
    );
    members.insert(
        "tokenizer_config".to_owned(),
        CatalogMemberPinV1 {
            path: "tokenizer_config.json".to_owned(),
            upstream_path: "tokenizer_config.json".to_owned(),
            length: 493,
            sha256: "f477aeb15ff9f78d3c1ddf2361d2b0b8b20cf55220f839f29a37f3a18efddd89".to_owned(),
        },
    );
    CatalogedFastEmbedModelV1 {
        model_id: DEFAULT_FASTEMBED_MODEL_ID.to_owned(),
        backend: CatalogedEmbeddingBackendV1::FastEmbedOrt {
            fastembed_enum: "JinaEmbeddingsV2BaseCode".to_owned(),
        },
        model_code: "jinaai/jina-embeddings-v2-base-code".to_owned(),
        source: CatalogSourceV1 {
            upstream: "https://huggingface.co/jinaai/jina-embeddings-v2-base-code".to_owned(),
            revision: "516f4baf13dec4ddddda8631e019b5737c8bc250".to_owned(),
            license: "Apache-2.0".to_owned(),
            license_url: "https://www.apache.org/licenses/LICENSE-2.0".to_owned(),
            provenance:
                "https://huggingface.co/jinaai/jina-embeddings-v2-base-code/tree/516f4baf13dec4ddddda8631e019b5737c8bc250"
                    .to_owned(),
        },
        expected_dimensions: 768,
        max_length: 8192,
        members,
    }
}

/// Production pin for the Model2Vec static code model
/// `minishlab/potion-code-16M-v2` (MIT, 256-dimensional fp16 table).
///
/// Lengths and digests were taken from the immutable revision's files as
/// served by `https://huggingface.co/minishlab/potion-code-16M-v2/resolve/<revision>/`;
/// `config.json` is `{"normalize": true, "embedding_dtype": "float16"}` and
/// the runtime re-reads it at load to confirm the L2 normalization pin.
fn potion_code_16m_v2() -> CatalogedFastEmbedModelV1 {
    let mut members = BTreeMap::new();
    members.insert(
        "model".to_owned(),
        CatalogMemberPinV1 {
            path: "model.safetensors".to_owned(),
            upstream_path: "model.safetensors".to_owned(),
            length: 32_490_072,
            sha256: "75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c".to_owned(),
        },
    );
    members.insert(
        "tokenizer".to_owned(),
        CatalogMemberPinV1 {
            path: "tokenizer.json".to_owned(),
            upstream_path: "tokenizer.json".to_owned(),
            length: 1_024_340,
            sha256: "107bbdcbad4bff1d299b7a4c3a2fb17c52890688b7dd0e4c9deab79d3c4f3d45".to_owned(),
        },
    );
    members.insert(
        "config".to_owned(),
        CatalogMemberPinV1 {
            path: "config.json".to_owned(),
            upstream_path: "config.json".to_owned(),
            length: 59,
            sha256: "148e5691a6fcc553437156859701fba017a1ba5d340b170f17e0f3668fb861a7".to_owned(),
        },
    );
    CatalogedFastEmbedModelV1 {
        model_id: MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID.to_owned(),
        backend: CatalogedEmbeddingBackendV1::Model2VecStatic {
            table_precision: EmbeddingPrecisionV1::Fp16,
        },
        model_code: "minishlab/potion-code-16M-v2".to_owned(),
        source: CatalogSourceV1 {
            upstream: "https://huggingface.co/minishlab/potion-code-16M-v2".to_owned(),
            revision: "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b".to_owned(),
            license: "MIT".to_owned(),
            license_url: "https://opensource.org/license/mit".to_owned(),
            provenance:
                "https://huggingface.co/minishlab/potion-code-16M-v2/tree/e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b"
                    .to_owned(),
        },
        expected_dimensions: 256,
        max_length: 1024,
        members,
    }
}

/// Map a catalog member role name onto the manifest member vocabulary.
pub fn catalog_member_role(name: &str) -> Option<ArtifactMemberRoleV1> {
    match name {
        "model" => Some(ArtifactMemberRoleV1::Model),
        "tokenizer" => Some(ArtifactMemberRoleV1::Tokenizer),
        "config" => Some(ArtifactMemberRoleV1::Config),
        "special_tokens_map" => Some(ArtifactMemberRoleV1::SpecialTokensMap),
        "tokenizer_config" => Some(ArtifactMemberRoleV1::TokenizerConfig),
        _ => None,
    }
}

/// Package digest identity over catalog pins (model + revision + members).
pub fn catalog_package_digest(model: &CatalogedFastEmbedModelV1) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.fastembed.catalog-package.v1\0");
    hasher.update(model.model_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(model.source.revision.as_bytes());
    hasher.update(b"\0");
    for (role, member) in &model.members {
        hasher.update(role.as_bytes());
        hasher.update(b"\0");
        hasher.update(member.upstream_path.as_bytes());
        hasher.update(b"\0");
        hasher.update(member.length.to_le_bytes());
        hasher.update(member.sha256.as_bytes());
        hasher.update(b"\0");
    }
    encode_lowercase_hex(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use tracedecay_semantic_contracts::SemanticConfig;

    use super::*;

    #[test]
    fn production_catalog_pins_default_jina_code_model() {
        let catalog = FastEmbedModelCatalogV1::production();
        catalog.validate().expect("production catalog");
        let model = catalog
            .get(DEFAULT_FASTEMBED_MODEL_ID)
            .expect("default model");
        assert_eq!(model.expected_dimensions, 768);
        assert_eq!(model.members.len(), 5);
        assert_eq!(
            model.backend.runtime_family(),
            EmbeddingRuntimeFamilyV1::FastEmbedOrt
        );
        assert_eq!(model.backend.precision(), EmbeddingPrecisionV1::Fp32);
        assert_eq!(
            catalog_package_digest(model),
            catalog_package_digest(&jina_embeddings_v2_base_code())
        );
    }

    #[test]
    fn production_catalog_pins_model2vec_potion_code_model() {
        let catalog = FastEmbedModelCatalogV1::production();
        catalog.validate().expect("production catalog");
        let model = catalog
            .get(MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID)
            .expect("model2vec model");
        assert_eq!(model.expected_dimensions, 256);
        assert_eq!(model.max_length, 1024);
        assert_eq!(
            model.backend,
            CatalogedEmbeddingBackendV1::Model2VecStatic {
                table_precision: EmbeddingPrecisionV1::Fp16,
            }
        );
        assert_eq!(
            model.backend.runtime_family(),
            EmbeddingRuntimeFamilyV1::Model2VecStatic
        );
        assert_eq!(model.source.license, "MIT");
        assert_eq!(
            model.members.keys().map(String::as_str).collect::<Vec<_>>(),
            ["config", "model", "tokenizer"]
        );
        assert_eq!(model.members["model"].path, "model.safetensors");
        assert_ne!(
            catalog_package_digest(model),
            catalog_package_digest(&jina_embeddings_v2_base_code())
        );
    }

    #[test]
    fn every_production_model_id_is_a_valid_settings_selection() {
        let catalog = FastEmbedModelCatalogV1::production();
        for model_id in catalog.model_ids() {
            let config = SemanticConfig {
                selected_model: Some(model_id.to_owned()),
                ..SemanticConfig::default()
            };
            config
                .validate()
                .unwrap_or_else(|error| panic!("{model_id} must be selectable: {error}"));
        }
        let unknown = SemanticConfig {
            selected_model: Some("NotARealModel".to_owned()),
            ..SemanticConfig::default()
        };
        assert!(unknown.validate().is_err());
    }

    #[test]
    fn model2vec_entry_requires_its_members_and_a_decodable_precision() {
        let mut catalog = FastEmbedModelCatalogV1::production();
        let model = catalog
            .models
            .iter_mut()
            .find(|model| model.model_id == MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID)
            .expect("model2vec model");
        model.backend = CatalogedEmbeddingBackendV1::Model2VecStatic {
            table_precision: EmbeddingPrecisionV1::Int8,
        };
        assert_eq!(catalog.validate(), Err(CatalogErrorV1::InvalidEntry));

        let mut catalog = FastEmbedModelCatalogV1::production();
        let model = catalog
            .models
            .iter_mut()
            .find(|model| model.model_id == MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID)
            .expect("model2vec model");
        model.members.remove("config");
        assert_eq!(catalog.validate(), Err(CatalogErrorV1::InvalidEntry));

        let mut catalog = FastEmbedModelCatalogV1::production();
        let model = catalog
            .models
            .iter_mut()
            .find(|model| model.model_id == DEFAULT_FASTEMBED_MODEL_ID)
            .expect("default model");
        model.members.remove("special_tokens_map");
        assert_eq!(catalog.validate(), Err(CatalogErrorV1::InvalidEntry));
    }

    #[test]
    fn unknown_model_is_rejected_by_lookup() {
        let catalog = FastEmbedModelCatalogV1::production();
        assert!(catalog.get("NotARealModel").is_none());
    }
}
