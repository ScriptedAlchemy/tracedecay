//! The shared o200k vocabulary, compressed at rest and loaded lazily.
//!
//! Encoding stays in the maintained tiktoken library; this crate only owns
//! TraceDecay's embedded asset and its fallible initialization.

use std::io::Read;
use std::sync::OnceLock;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::read::GzDecoder;
use rustc_hash::{FxHashMap, FxHashSet};
use tiktoken_rs::{CoreBPE, ENDOFPROMPT, ENDOFTEXT, O200K_BASE_PAT_STR, Rank};

#[derive(Debug, thiserror::Error)]
pub enum TokenizerError {
    #[error("cannot decompress the embedded vocabulary: {0}")]
    Decompress(#[from] std::io::Error),
    #[error("invalid vocabulary record at line {line}")]
    InvalidRecord { line: usize },
    #[error("invalid token at vocabulary line {line}: {source}")]
    InvalidToken {
        line: usize,
        source: base64::DecodeError,
    },
    #[error("invalid rank at vocabulary line {line}: {source}")]
    InvalidRank {
        line: usize,
        source: std::num::ParseIntError,
    },
    #[error("duplicate token or rank at vocabulary line {line}")]
    DuplicateRecord { line: usize },
    #[error("cannot initialize the tokenizer: {0}")]
    Initialize(String),
}

/// Returns the process-wide tokenizer or its initialization failure.
pub fn o200k_base() -> Result<&'static CoreBPE, &'static TokenizerError> {
    static TOKENIZER: OnceLock<Result<CoreBPE, TokenizerError>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| load_compressed(include_bytes!("../assets/o200k_base.tiktoken.gz")))
        .as_ref()
}

fn parse_vocabulary(text: &str) -> Result<FxHashMap<Vec<u8>, Rank>, TokenizerError> {
    let mut encoder = FxHashMap::default();
    let mut ranks = FxHashSet::default();
    for (index, record) in text.lines().enumerate() {
        let line = index + 1;
        let (raw, rank) = record
            .split_once(' ')
            .ok_or(TokenizerError::InvalidRecord { line })?;
        let token = STANDARD
            .decode(raw)
            .map_err(|source| TokenizerError::InvalidToken { line, source })?;
        let rank = rank
            .parse::<Rank>()
            .map_err(|source| TokenizerError::InvalidRank { line, source })?;
        if token.is_empty() {
            return Err(TokenizerError::InvalidRecord { line });
        }
        // CoreBPE requires unique ranks and asserts that invariant internally.
        if !ranks.insert(rank) || encoder.insert(token, rank).is_some() {
            return Err(TokenizerError::DuplicateRecord { line });
        }
    }
    if encoder.is_empty() {
        return Err(TokenizerError::InvalidRecord { line: 1 });
    }
    Ok(encoder)
}

fn load_compressed(bytes: &[u8]) -> Result<CoreBPE, TokenizerError> {
    let mut text = String::new();
    GzDecoder::new(bytes).read_to_string(&mut text)?;
    let encoder = parse_vocabulary(&text)?;
    let special_tokens = FxHashMap::from_iter([
        (ENDOFTEXT.to_owned(), 199_999),
        (ENDOFPROMPT.to_owned(), 200_018),
    ]);
    CoreBPE::new(encoder, special_tokens, O200K_BASE_PAT_STR)
        .map_err(|error| TokenizerError::Initialize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_vocabulary_matches_upstream_encoding() {
        let actual = o200k_base().unwrap();
        let upstream = tiktoken_rs::o200k_base().unwrap();
        for text in [
            "",
            "Hello, world!",
            "fn main() { println!(\"hello\"); }\n",
            "日本語のトークン · 中文 · 한국어 · العربية",
            "👩🏽‍💻 café e\u{301}\r\n\t  ",
            "1234567890 isn't we're THEY'RE",
            "<|endoftext|>ordinary<|endofprompt|>",
        ] {
            assert_eq!(actual.encode_ordinary(text), upstream.encode_ordinary(text));
            assert_eq!(
                actual.encode_with_special_tokens(text),
                upstream.encode_with_special_tokens(text)
            );
        }
        assert!(std::ptr::eq(actual, o200k_base().unwrap()));
    }

    #[test]
    fn rejects_corrupt_compressed_vocabulary() {
        assert!(matches!(
            load_compressed(b"not gzip"),
            Err(TokenizerError::Decompress(_))
        ));
    }

    #[test]
    fn rejects_invalid_records_and_duplicate_identity() {
        assert!(matches!(
            parse_vocabulary(""),
            Err(TokenizerError::InvalidRecord { .. })
        ));
        assert!(matches!(
            parse_vocabulary("YQ=="),
            Err(TokenizerError::InvalidRecord { .. })
        ));
        assert!(matches!(
            parse_vocabulary(" 0"),
            Err(TokenizerError::InvalidRecord { .. })
        ));
        assert!(matches!(
            parse_vocabulary("! 0"),
            Err(TokenizerError::InvalidToken { .. })
        ));
        assert!(matches!(
            parse_vocabulary("YQ== nope"),
            Err(TokenizerError::InvalidRank { .. })
        ));
        assert!(matches!(
            parse_vocabulary("YQ== 0\nYg== 0"),
            Err(TokenizerError::DuplicateRecord { line: 2 })
        ));
        assert!(matches!(
            parse_vocabulary("YQ== 0\nYQ== 1"),
            Err(TokenizerError::DuplicateRecord { line: 2 })
        ));
    }
}
