use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;

use super::super::StorageRuntimeContractErrorV1;
use super::types::{
    SemanticVectorChunkDigest, SemanticVectorChunkId, SemanticVectorChunkManifestDigest,
    SemanticVectorStageChunkOperation,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorChunkManifestMember {
    pub chunk_id: SemanticVectorChunkId,
    pub chunk_digest: SemanticVectorChunkDigest,
    pub operation: SemanticVectorStageChunkOperation,
}

pub struct SemanticVectorChunkManifestAccumulator {
    hasher: Sha256,
    last_chunk_id: Option<SemanticVectorChunkId>,
}

impl SemanticVectorChunkManifestAccumulator {
    pub fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay.semantic-vector-chunk-manifest\0");
        Self {
            hasher,
            last_chunk_id: None,
        }
    }

    pub fn push(
        &mut self,
        member: &SemanticVectorChunkManifestMember,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        if self
            .last_chunk_id
            .as_ref()
            .is_some_and(|prior| prior >= &member.chunk_id)
        {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "semantic vector chunk manifest order",
            });
        }
        let encoded = tracedecay_domain::canonical_sha256(&(
            "tracedecay.semantic-vector-chunk-manifest-member",
            member,
        ))
        .map_err(|_| StorageRuntimeContractErrorV1::NonCanonical {
            field: "semantic vector chunk manifest member",
        })?;
        self.hasher.update(encoded.as_str().as_bytes());
        self.hasher.update([0]);
        self.last_chunk_id = Some(member.chunk_id.clone());
        Ok(())
    }

    pub fn finish(
        self,
    ) -> Result<SemanticVectorChunkManifestDigest, StorageRuntimeContractErrorV1> {
        let digest = self.hasher.finalize();
        SemanticVectorChunkManifestDigest::new(encode_tagged_lowercase_hex("sha256:", &digest))
    }
}

impl Default for SemanticVectorChunkManifestAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[hotpath::measure(label = "store.semantic_vector.manifest_digest")]
pub fn semantic_vector_chunk_manifest_digest(
    sorted_members: &[SemanticVectorChunkManifestMember],
) -> Result<SemanticVectorChunkManifestDigest, StorageRuntimeContractErrorV1> {
    let mut accumulator = SemanticVectorChunkManifestAccumulator::new();
    for member in sorted_members {
        accumulator.push(member)?;
    }
    accumulator.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_digest_streams_beyond_the_legacy_project_size() {
        const LEGACY_PROJECT_CHUNK_LIMIT: u64 = 100_000;
        let digest = SemanticVectorChunkDigest::new(format!("sha256:{}", "a".repeat(64)))
            .expect("canonical chunk digest");
        let mut accumulator = SemanticVectorChunkManifestAccumulator::new();
        for ordinal in 0..=LEGACY_PROJECT_CHUNK_LIMIT {
            accumulator
                .push(&SemanticVectorChunkManifestMember {
                    chunk_id: SemanticVectorChunkId::new(format!("chunk.{ordinal:06}"))
                        .expect("canonical ordered chunk id"),
                    chunk_digest: digest.clone(),
                    operation: SemanticVectorStageChunkOperation::Embed,
                })
                .expect("streaming manifests are bounded per member, not by project size");
        }
        accumulator.finish().expect("streaming manifest digest");
    }
}
