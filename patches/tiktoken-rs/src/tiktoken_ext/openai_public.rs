pub const ENDOFTEXT: &str = "<|endoftext|>";
pub const FIM_PREFIX: &str = "<|fim_prefix|>";
pub const FIM_MIDDLE: &str = "<|fim_middle|>";
pub const FIM_SUFFIX: &str = "<|fim_suffix|>";
pub const ENDOFPROMPT: &str = "<|endofprompt|>";

/// Adaptation of the tiktoken crate for use in Rust projects
use std::io::Read;

use anyhow::{Result, bail};
use base64::{Engine as _, engine::general_purpose};
use flate2::read::GzDecoder;
use rustc_hash::FxHashMap as HashMap;

use crate::{CoreBPE, Rank};

/// TraceDecay ships only the gzip-compressed `o200k_base` vocabulary. Legacy
/// vocabularies are omitted from the patch crate so they cannot land in the
/// binary even if a dead call site reappears.
fn unsupported_vocabulary(name: &str) -> Result<CoreBPE> {
    bail!("tiktoken-rs vocabulary {name} is not shipped in this TraceDecay build")
}

fn o200k_base_vocabulary() -> Result<String> {
    let compressed = include_bytes!("../../assets/o200k_base.tiktoken.gz");
    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut inflated = String::new();
    decoder.read_to_string(&mut inflated)?;
    Ok(inflated)
}

/// Use for GPT-3 models like `davinci`
/// Initializes and returns a new instance of the r50k_base tokenizer (also known as `gpt2`)
pub fn r50k_base() -> Result<CoreBPE> {
    unsupported_vocabulary("r50k_base")
}

/// Use for Code models, `text-davinci-002`, `text-davinci-003`
/// Initializes and returns a new instance of the p50k_base tokenizer.
pub fn p50k_base() -> Result<CoreBPE> {
    unsupported_vocabulary("p50k_base")
}

/// Use for edit models like `text-davinci-edit-001`, `code-davinci-edit-001`
/// Initializes and returns a new instance of the p50k_base tokenizer.
pub fn p50k_edit() -> Result<CoreBPE> {
    unsupported_vocabulary("p50k_edit")
}

/// Use for ChatGPT models, `text-embedding-ada-002`
/// Initializes and returns a new instance of the cl100k_base tokenizer.
pub fn cl100k_base() -> Result<CoreBPE> {
    unsupported_vocabulary("cl100k_base")
}

pub const O200K_BASE_PAT_STR: &str = concat!(
    r#"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?"#,
    "|",
    r#"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?"#,
    "|",
    r#"\p{N}{1,3}"#,
    "|",
    r#" ?[^\s\p{L}\p{N}]+[\r\n/]*"#,
    "|",
    r#"\s*[\r\n]+"#,
    "|",
    r#"\s+(?!\S)"#,
    "|",
    r#"\s+"#
);

/// Use for GPT-5, GPT-4.1, GPT-4o, and other `o` series models like `o1`, `o3`, and `o4`.
/// Initializes and returns a new instance of the o200k_base tokenizer.
pub fn o200k_base() -> Result<CoreBPE> {
    let o200k_base = o200k_base_vocabulary()?;

    let mut encoder = HashMap::default();
    for line in o200k_base.lines() {
        let mut parts = line.split(' ');
        let raw = parts.next().unwrap();
        let token = &general_purpose::STANDARD.decode(raw)?;
        let rank: Rank = parts.next().unwrap().parse().unwrap();
        encoder.insert(token.clone(), rank);
    }

    let mut special_tokens = HashMap::default();
    special_tokens.insert(String::from(ENDOFTEXT), 199999);
    special_tokens.insert(String::from(ENDOFPROMPT), 200018);

    let bpe = CoreBPE::new(encoder, special_tokens, O200K_BASE_PAT_STR)?;
    Ok(bpe)
}

/// Use for gpt-oss models like `gpt-oss-20b`, `gpt-oss-120b`.
/// Initializes and returns a new instance of the o200k_harmony tokenizer.
pub fn o200k_harmony() -> Result<CoreBPE> {
    let o200k_harmony = o200k_base_vocabulary()?;

    let mut encoder = HashMap::default();
    for line in o200k_harmony.lines() {
        let mut parts = line.split(' ');
        let raw = parts.next().unwrap();
        let token = &general_purpose::STANDARD.decode(raw)?;
        let rank: Rank = parts.next().unwrap().parse().unwrap();
        encoder.insert(token.clone(), rank);
    }

    let mut special_tokens = HashMap::default();

    special_tokens.insert(String::from("<|startoftext|>"), 199998);
    special_tokens.insert(String::from("<|endoftext|>"), 199999);
    special_tokens.insert(String::from("<|reserved_200000|>"), 200000);
    special_tokens.insert(String::from("<|reserved_200001|>"), 200001);
    special_tokens.insert(String::from("<|return|>"), 200002);
    special_tokens.insert(String::from("<|constrain|>"), 200003);
    special_tokens.insert(String::from("<|reserved_200004|>"), 200004);
    special_tokens.insert(String::from("<|channel|>"), 200005);
    special_tokens.insert(String::from("<|start|>"), 200006);
    special_tokens.insert(String::from("<|end|>"), 200007);
    special_tokens.insert(String::from("<|message|>"), 200008);
    special_tokens.insert(String::from("<|reserved_200009|>"), 200009);
    special_tokens.insert(String::from("<|reserved_200010|>"), 200010);
    special_tokens.insert(String::from("<|reserved_200011|>"), 200011);
    special_tokens.insert(String::from("<|call|>"), 200012);
    for i in 200013..=201087 {
        // reserved tokens from 200013 to 201087
        special_tokens.insert(format!("<|reserved_{}|>", i), i);
    }

    let bpe = CoreBPE::new(encoder, special_tokens, O200K_BASE_PAT_STR)?;
    Ok(bpe)
}
