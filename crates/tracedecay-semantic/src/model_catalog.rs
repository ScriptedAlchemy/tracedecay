//! Immutable `FastEmbed` model catalog for `TraceDecay` semantic selection.
//!
//! Catalog entries pin source revision, license, member lengths, and SHA-256
//! digests. There are no signatures or trust roots — integrity is the
//! declared length + digest identity, matching the distribution fixture.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::DEFAULT_FASTEMBED_MODEL_ID;

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

/// Provenance for a cataloged `FastEmbed` model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSourceV1 {
    pub upstream: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub provenance: String,
}

/// One supported `FastEmbed` model that settings may select.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogedFastEmbedModelV1 {
    pub model_id: String,
    pub fastembed_enum: String,
    pub model_code: String,
    pub source: CatalogSourceV1,
    pub expected_dimensions: u32,
    pub max_length: u32,
    pub members: BTreeMap<String, CatalogMemberPinV1>,
}

/// Versioned catalog of supported `FastEmbed` models.
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
            models: vec![jina_embeddings_v2_base_code()],
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
    #[error("fastembed model catalog schema is invalid")]
    InvalidSchema,
    #[error("fastembed model catalog is empty")]
    Empty,
    #[error("fastembed model catalog has a duplicate model id")]
    DuplicateModelId,
    #[error("fastembed model catalog omits the default model")]
    MissingDefault,
    #[error("fastembed model catalog entry is invalid")]
    InvalidEntry,
    #[error("selected fastembed model is not in the catalog")]
    UnknownModel,
}

fn validate_model(model: &CatalogedFastEmbedModelV1) -> Result<(), CatalogErrorV1> {
    if model.model_id.trim().is_empty()
        || model.fastembed_enum.trim().is_empty()
        || model.model_code.trim().is_empty()
        || model.expected_dimensions == 0
        || model.max_length == 0
        || model.members.is_empty()
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
        fastembed_enum: "JinaEmbeddingsV2BaseCode".to_owned(),
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

/// Package digest identity over catalog pins (model + revision + members).
pub fn catalog_package_digest(model: &CatalogedFastEmbedModelV1) -> String {
    use sha2::{Digest, Sha256};
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
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
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
            catalog_package_digest(model),
            catalog_package_digest(&jina_embeddings_v2_base_code())
        );
    }

    #[test]
    fn unknown_model_is_rejected_by_lookup() {
        let catalog = FastEmbedModelCatalogV1::production();
        assert!(catalog.get("NotARealModel").is_none());
    }
}
