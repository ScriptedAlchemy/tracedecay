use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::super::StorageRuntimeContractErrorV1;
use super::types::{
    MAX_SEMANTIC_VECTOR_STAGE_CHUNKS, SemanticVectorChunkDigest, SemanticVectorChunkId,
    SemanticVectorChunkManifestDigest, SemanticVectorStageChunkOperation,
};

pub const MAX_SEMANTIC_VECTOR_CHUNK_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_SEMANTIC_VECTOR_CHUNK_MANIFEST_BYTES_U64: u64 = 64 * 1024 * 1024;

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
    members: u64,
    bytes: usize,
}

impl SemanticVectorChunkManifestAccumulator {
    pub fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay.semantic-vector-chunk-manifest\0");
        Self {
            hasher,
            last_chunk_id: None,
            members: 0,
            bytes: 0,
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
        if self.members >= MAX_SEMANTIC_VECTOR_STAGE_CHUNKS {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector chunk manifest members",
                actual: self.members + 1,
                max: MAX_SEMANTIC_VECTOR_STAGE_CHUNKS,
            });
        }
        let encoded = tracedecay_domain::canonical_sha256(&(
            "tracedecay.semantic-vector-chunk-manifest-member",
            member,
        ))
        .map_err(|_| StorageRuntimeContractErrorV1::NonCanonical {
            field: "semantic vector chunk manifest member",
        })?;
        let next_bytes = self
            .bytes
            .checked_add(member.chunk_id.as_str().len())
            .and_then(|value| value.checked_add(member.chunk_digest.as_str().len()))
            .and_then(|value| value.checked_add(encoded.as_str().len()))
            .ok_or(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector chunk manifest bytes",
                actual: u64::MAX,
                max: MAX_SEMANTIC_VECTOR_CHUNK_MANIFEST_BYTES_U64,
            })?;
        if next_bytes > MAX_SEMANTIC_VECTOR_CHUNK_MANIFEST_BYTES {
            let actual = u64::try_from(next_bytes).map_err(|_| {
                StorageRuntimeContractErrorV1::LimitExceeded {
                    field: "semantic vector chunk manifest bytes",
                    actual: u64::MAX,
                    max: MAX_SEMANTIC_VECTOR_CHUNK_MANIFEST_BYTES_U64,
                }
            })?;
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector chunk manifest bytes",
                actual,
                max: MAX_SEMANTIC_VECTOR_CHUNK_MANIFEST_BYTES_U64,
            });
        }
        self.hasher.update(encoded.as_str().as_bytes());
        self.hasher.update([0]);
        self.last_chunk_id = Some(member.chunk_id.clone());
        self.members += 1;
        self.bytes = next_bytes;
        Ok(())
    }

    pub fn finish(
        self,
    ) -> Result<SemanticVectorChunkManifestDigest, StorageRuntimeContractErrorV1> {
        let digest = self.hasher.finalize();
        SemanticVectorChunkManifestDigest::new(format!("sha256:{}", hex::encode(digest)))
    }
}

impl Default for SemanticVectorChunkManifestAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

pub fn semantic_vector_chunk_manifest_digest(
    sorted_members: &[SemanticVectorChunkManifestMember],
) -> Result<SemanticVectorChunkManifestDigest, StorageRuntimeContractErrorV1> {
    let mut accumulator = SemanticVectorChunkManifestAccumulator::new();
    for member in sorted_members {
        accumulator.push(member)?;
    }
    accumulator.finish()
}
