mod canonical;

use sha2::{Digest, Sha256};

pub use canonical::{normalize, stable_record_id};

const CURSOR_KEY_PREFIX: &str = "tracedecay-claude-cursor-v1";
const SOURCE_ID_PREFIX: &str = "tracedecay-claude-source-v1";
const OBSERVATION_SOURCE_ID_PREFIX: &str = "tracedecay-claude-observation-source-v1-sha256";
const OBSERVATION_SOURCE_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation-source.v1\0";

pub fn encode_cursor_key(platform: &str, native_path: &[u8]) -> String {
    format!(
        "{CURSOR_KEY_PREFIX}-{platform}-{}",
        hex::encode(native_path)
    )
}

pub fn encode_source_id(platform: &str, native_stem: &[u8]) -> String {
    format!("{SOURCE_ID_PREFIX}-{platform}-{}", hex::encode(native_stem))
}

pub fn observation_source_id(native_transcript_id: &[u8]) -> String {
    format!(
        "{OBSERVATION_SOURCE_ID_PREFIX}-{}",
        canonical_framed_sha256(OBSERVATION_SOURCE_ID_DOMAIN, &[native_transcript_id])
    )
}

fn canonical_framed_sha256(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}
