mod canonical;

use tracedecay_domain::canonical_text::canonical_framed_sha256;

pub use canonical::{ClaudeSpawnParent, normalize, normalize_spawned, stable_record_id};

const CURSOR_KEY_PREFIX: &str = "tracedecay-claude-cursor-v1";
const SOURCE_ID_PREFIX: &str = "tracedecay-claude-source-v1";
const OBSERVATION_SOURCE_ID_PREFIX: &str = "tracedecay-claude-observation-source-v1-sha256";
const OBSERVATION_SOURCE_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation-source.v1\0";
const OBSERVATION_PATH_SOURCE_ID_PREFIX: &str = "tracedecay-claude-observation-source-v2-sha256";
const OBSERVATION_PATH_SOURCE_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation-source.v2\0";

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

/// Source key of one transcript file, from its native path relative to the
/// Claude projects directory. Unlike [`observation_source_id`], two files that
/// share a stem in different project directories get distinct keys.
pub fn observation_path_source_id(native_relative_path: &[u8]) -> String {
    format!(
        "{OBSERVATION_PATH_SOURCE_ID_PREFIX}-{}",
        canonical_framed_sha256(OBSERVATION_PATH_SOURCE_ID_DOMAIN, &[native_relative_path])
    )
}
