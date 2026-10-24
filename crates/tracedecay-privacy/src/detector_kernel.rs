use std::collections::BTreeSet;
use std::ops::Range;

use serde_json::Value;

// Credential rules, the vendored community catalogue and TraceDecay's local
// supplement, live in `super::rules`, which owns their schema, provenance and
// compilation. They are re-exported here because this is the module every
// consumer already imports the detector kernel from, and moving the rules out
// of hand-written code should not move every caller with them.
pub use super::rules::{
    CredentialPattern, CredentialPatternKind, CredentialPatternProfile, CredentialRuleSetError,
    compile_credential_patterns,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedSensitiveKey {
    ascii_compact: String,
    separated: String,
    compact: String,
}

impl NormalizedSensitiveKey {
    pub fn new(key: &str) -> Self {
        let ascii_compact = key
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|character| character.to_ascii_lowercase())
            .collect();

        let characters: Vec<_> = key.chars().collect();
        let mut separated = String::with_capacity(key.len());
        for (index, &character) in characters.iter().enumerate() {
            let previous = index.checked_sub(1).and_then(|index| characters.get(index));
            let next = characters.get(index + 1);
            let word_boundary = character.is_ascii_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_ascii_lowercase()
                        || previous.is_ascii_digit()
                        || (previous.is_ascii_uppercase()
                            && next.is_some_and(char::is_ascii_lowercase))
                });
            if word_boundary && !separated.ends_with('_') {
                separated.push('_');
            }
            if character.is_ascii_alphanumeric() {
                separated.push(character.to_ascii_lowercase());
            } else if !separated.ends_with('_') {
                separated.push('_');
            }
        }
        let separated = separated.trim_matches('_').to_string();
        let compact = separated.replace('_', "");

        Self {
            ascii_compact,
            separated,
            compact,
        }
    }

    pub(crate) fn ascii_compact(&self) -> &str {
        &self.ascii_compact
    }

    pub fn separated(&self) -> &str {
        &self.separated
    }

    pub fn compact(&self) -> &str {
        &self.compact
    }
}

pub trait SensitiveKeyPolicy {
    type Match: Copy;

    fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonPathSegment {
    Field(usize),
    Index(usize),
}

pub enum JsonVisitMut<'a, M> {
    SensitiveValue(&'a mut Value, M),
    String(&'a mut String),
}

pub(crate) fn visit_json_object_keys<P, V>(value: &Value, policy: &P, mut visit: V) -> bool
where
    P: SensitiveKeyPolicy,
    V: FnMut(&str, &[JsonPathSegment]) -> bool,
{
    fn walk<P, V>(value: &Value, policy: &P, path: &mut Vec<JsonPathSegment>, visit: &mut V) -> bool
    where
        P: SensitiveKeyPolicy,
        V: FnMut(&str, &[JsonPathSegment]) -> bool,
    {
        match value {
            Value::Object(fields) => {
                let mut matched = false;
                for (index, (key, child)) in fields.iter().enumerate() {
                    path.push(JsonPathSegment::Field(index));
                    matched |= visit(key, path);
                    if policy.classify(&NormalizedSensitiveKey::new(key)).is_none() {
                        matched |= walk(child, policy, path, visit);
                    }
                    path.pop();
                }
                matched
            }
            Value::Array(items) => {
                let mut matched = false;
                for (index, child) in items.iter().enumerate() {
                    path.push(JsonPathSegment::Index(index));
                    matched |= walk(child, policy, path, visit);
                    path.pop();
                }
                matched
            }
            _ => false,
        }
    }

    walk(value, policy, &mut Vec::new(), &mut visit)
}

pub fn visit_sensitive_json_mut<P, V>(value: &mut Value, policy: &P, mut visit: V) -> bool
where
    P: SensitiveKeyPolicy,
    V: FnMut(JsonVisitMut<'_, P::Match>, &[JsonPathSegment]) -> bool,
{
    fn walk<P, V>(
        value: &mut Value,
        policy: &P,
        path: &mut Vec<JsonPathSegment>,
        visit: &mut V,
    ) -> bool
    where
        P: SensitiveKeyPolicy,
        V: FnMut(JsonVisitMut<'_, P::Match>, &[JsonPathSegment]) -> bool,
    {
        match value {
            Value::Object(fields) => {
                let mut changed = false;
                for (index, (key, child)) in fields.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Field(index));
                    let normalized = NormalizedSensitiveKey::new(key);
                    let redacted = policy.classify(&normalized).is_some_and(|matched| {
                        visit(JsonVisitMut::SensitiveValue(child, matched), path)
                    });
                    changed |= redacted;
                    if !redacted {
                        changed |= walk(child, policy, path, visit);
                    }
                    path.pop();
                }
                changed
            }
            Value::Array(items) => {
                let mut changed = false;
                for (index, child) in items.iter_mut().enumerate() {
                    path.push(JsonPathSegment::Index(index));
                    changed |= walk(child, policy, path, visit);
                    path.pop();
                }
                changed
            }
            Value::String(text) => visit(JsonVisitMut::String(text), path),
            _ => false,
        }
    }

    walk(value, policy, &mut Vec::new(), &mut visit)
}

/// Where a scanned text sits in its document, which decides whether a
/// compound identifier in it is a name or a value.
///
/// Entropy alone cannot tell `WorkDuplicateAdjudicationAppendOutcomeV1` from
/// `RocketButtonStapleBridgeYellowOrange90`: both are words joined together.
/// Only position can. A token the document uses as a name is exempt; the same
/// token as a value or in free text is withheld, because passphrases are
/// exactly such compounds.
#[derive(Clone, Copy)]
pub(crate) enum EntropyScope<'a> {
    /// A value or free text. Nothing is a name.
    Value,
    /// The text is an object key.
    Key,
    /// The raw text of a parsed document. Tokens that make up one of its
    /// parsed object keys are names.
    Document(&'a BTreeSet<String>),
    /// Source code. Tokens in a declaration, path, call, or type position are
    /// names.
    Code,
}

impl EntropyScope<'_> {
    fn names(self, text: &str, range: Range<usize>) -> bool {
        let token = &text[range.clone()];
        match self {
            Self::Value => false,
            Self::Key => is_compound_identifier(token),
            // A reference to keys (`#/$defs/Name`) is a path of key names.
            Self::Document(keys) => {
                token
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .all(|part| keys.contains(part))
                    && is_compound_identifier(token)
            }
            Self::Code => is_code_name_position(text, range) && is_compound_identifier(token),
        }
    }
}

/// The `/`-separated name parts of a parsed object key, for
/// [`EntropyScope::Document`].
pub(crate) fn key_tokens(key: &str) -> impl Iterator<Item = &str> {
    key.split(|c: char| c == '/' || !(c.is_ascii() && token_byte(c as u8)))
        .filter(|token| !token.is_empty())
}

const DECLARATION_KEYWORDS: [&str; 15] = [
    "struct",
    "enum",
    "union",
    "fn",
    "type",
    "trait",
    "impl",
    "class",
    "interface",
    "const",
    "let",
    "static",
    "mod",
    "use",
    "extends",
];

/// Whether `previous` is the word that introduces a declared name.
pub fn introduces_declared_name(previous: &str) -> bool {
    DECLARATION_KEYWORDS.contains(&previous)
}

/// Whether the token at `range` stands where a grammar puts a name, not a
/// value: after a declaration keyword, beside `::`, after `->`, a call, a
/// generic or a tuple payload (`Name(`, `<Name`, `Name<`), or a member
/// (`a.Name`). A quote, a bare space, or sentence punctuation around it is a
/// value or prose.
fn is_code_name_position(text: &str, range: Range<usize>) -> bool {
    let before = &text[..range.start];
    let after = &text[range.end..];
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let spaced = before.trim_end();
    if spaced.len() < before.len() {
        let keyword = spaced
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .next();
        if keyword.is_some_and(introduces_declared_name) || spaced.ends_with("->") {
            return true;
        }
    }
    let before_bytes = before.as_bytes();
    let attached_after_word = |marker: u8| {
        before_bytes.last() == Some(&marker)
            && before_bytes.len() >= 2
            && matches!(before_bytes[before_bytes.len() - 2], byte if is_word(byte) || byte == b'>')
    };
    let after_bytes = after.as_bytes();
    before.ends_with("::")
        || after.starts_with("::")
        || before.ends_with("->")
        || attached_after_word(b'(')
        || attached_after_word(b'<')
        || attached_after_word(b'.')
        || matches!(after_bytes, [b'(' | b'<', ..])
        || matches!(after_bytes, [b'.', next, ..] if is_word(*next))
}

pub(crate) fn high_entropy_ranges(text: &str, scope: EntropyScope<'_>) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        if !token_byte(bytes[start]) {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len() && token_byte(bytes[end]) {
            end += 1;
        }
        let candidate = &text[start..end];
        let macos_temp_path = candidate.starts_with("/private/var/folders/")
            || candidate.starts_with("/var/folders/");
        let uuid_path = !macos_temp_path
            && candidate.starts_with('/')
            && candidate.split('/').any(is_uuid_path_component);
        if uuid_path {
            extend_uuid_path_entropy_ranges(candidate, start, &mut ranges);
        } else if macos_temp_path {
            let mut component_start = 0usize;
            for component in candidate.split('/') {
                let component_end = component_start + component.len();
                if looks_high_entropy_token(component) {
                    ranges.push(start + component_start..start + component_end);
                }
                component_start = component_end + 1;
            }
        } else if looks_high_entropy_token(candidate)
            && !is_lcm_payload_ref(candidate)
            && !scope.names(text, start..end)
        {
            ranges.push(start..end);
        }
        start = end;
    }
    ranges
}

// UUIDs contribute identity, not credential entropy. Dense components and their
// neighbors can form one token. Sparse pairs need a standard Base64-shaped
// alphabet so filename punctuation cannot lend credential evidence.
// Inspect original, disjoint slices so ordinary prefixes cannot dilute secrets.
fn extend_uuid_path_entropy_ranges(text: &str, offset: usize, ranges: &mut Vec<Range<usize>>) {
    let mut component_start = 0usize;
    let mut previous: Option<Range<usize>> = None;
    let mut run: Option<Range<usize>> = None;
    let mut run_prefix = None;
    for component in text.split('/').chain(std::iter::once("")) {
        let component_end = component_start + component.len();
        let eligible = !component.is_empty() && !is_uuid_path_component(component);
        if eligible
            && entropy_bits_per_mille(component)
                .is_none_or(|score| score >= MIN_TOKEN_ENTROPY_PER_MILLE)
        {
            match &mut run {
                Some(run) => run.end = component_end,
                None => {
                    run_prefix = previous.take().map(|range| range.start);
                    run = Some(component_start..component_end);
                }
            }
        } else {
            let mut consumed = false;
            if let Some(run) = run.take() {
                let matched = if looks_high_entropy_token(&text[run.clone()]) {
                    Some(run.clone())
                } else if let Some(prefix) = run_prefix
                    && looks_high_entropy_token(&text[prefix..run.end])
                {
                    Some(prefix..run.end)
                } else if eligible && looks_high_entropy_token(&text[run.start..component_end]) {
                    consumed = true;
                    Some(run.start..component_end)
                } else {
                    None
                };
                if let Some(matched) = matched {
                    ranges.push(offset + matched.start..offset + matched.end);
                }
                run_prefix = None;
            }
            if !consumed
                && eligible
                && let Some(previous) = previous.take()
                && text[previous.start..component_end]
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
                && looks_high_entropy_token(&text[previous.start..component_end])
            {
                ranges.push(offset + previous.start..offset + component_end);
                consumed = true;
            }
            previous = (eligible && !consumed).then_some(component_start..component_end);
        }
        component_start = component_end + 1;
    }
}

fn is_uuid_path_component(component: &str) -> bool {
    component.len() == 36
        && component.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn is_lcm_payload_ref(candidate: &str) -> bool {
    candidate
        .strip_prefix("ref=")
        .unwrap_or(candidate)
        .strip_prefix("payload_")
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Shannon entropy of `token` in bits per character, scaled to per mille.
///
/// This is the same fixed-point sum [`looks_high_entropy_token`] thresholds on,
/// reported rather than compared, so a finding can name the score that produced
/// it on a versioned scale. Integer arithmetic keeps it exactly reproducible.
/// `None` means the fixed-point value could not be represented, so callers
/// retain the redaction but abstain from emitting an invented score.
pub(crate) fn entropy_bits_per_mille(token: &str) -> Option<u32> {
    if token.is_empty() {
        return Some(0);
    }
    let mut counts = [0usize; 256];
    for byte in token.bytes() {
        counts[byte as usize] += 1;
    }
    let len = token.len() as u128;
    let entropy_sum = len * fixed_log2(token.len())
        - counts
            .into_iter()
            .filter(|count| *count != 0)
            .map(|count| count as u128 * fixed_log2(count))
            .sum::<u128>();
    u32::try_from(entropy_sum * 1_000 / (len * ENTROPY_SCALE)).ok()
}

pub fn looks_high_entropy_token(token: &str) -> bool {
    if token.len() < MIN_TOKEN_LENGTH
        || !token.bytes().all(token_byte)
        || token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    if !token.bytes().any(|byte| byte.is_ascii_alphabetic())
        || !token.bytes().any(|byte| byte.is_ascii_digit())
    {
        return false;
    }

    let mut counts = [0usize; 256];
    for byte in token.bytes() {
        counts[byte as usize] += 1;
    }
    let len = token.len() as u128;
    let entropy_sum = len * fixed_log2(token.len())
        - counts
            .into_iter()
            .filter(|count| *count != 0)
            .map(|count| count as u128 * fixed_log2(count))
            .sum::<u128>();
    entropy_sum * 1_000 >= len * u128::from(MIN_TOKEN_ENTROPY_PER_MILLE) * ENTROPY_SCALE
}

/// Whether `token` reads as a compound identifier or path, such as
/// `WorkDuplicateAdjudicationAppendOutcomeV1`,
/// `clone_peak_rss_added_at_most_1_gib_for_39582_symbols`, or
/// `index-bench/clone-envelope-20260915-fingerprint-staging`.
///
/// Per-character entropy cannot separate these from credentials: a long
/// identifier draws on as many distinct characters as a random token. Its
/// words can. Identifier words are long, pronounceable, and consistently
/// cased, while a random token of the same length breaks into short,
/// case-flipping fragments with digits scattered through them. Base64 padding
/// and alphabet (`+`, `=`) never occur in identifiers.
pub fn is_compound_identifier(token: &str) -> bool {
    if token.bytes().any(|byte| matches!(byte, b'+' | b'=')) {
        return false;
    }
    let mut words = 0usize;
    let mut letters = 0usize;
    let mut short_words = 0usize;
    let mut vowelless_words = 0usize;
    let mut digit_runs = 0usize;
    for component in token
        .split(['/', '_', '-'])
        .filter(|component| !component.is_empty())
    {
        let component = component.as_bytes();
        let has_lowercase = component.iter().any(u8::is_ascii_lowercase);
        let screaming = {
            let letters_end = component
                .iter()
                .position(|byte| !byte.is_ascii_uppercase())
                .unwrap_or(component.len());
            component[letters_end..].iter().all(u8::is_ascii_digit)
        };
        let mut index = 0usize;
        while index < component.len() {
            if component[index].is_ascii_digit() {
                while index < component.len() && component[index].is_ascii_digit() {
                    index += 1;
                }
                digit_runs += 1;
                continue;
            }
            let start = index;
            let end = if component[index].is_ascii_uppercase() {
                let mut upper_end = index;
                while upper_end < component.len() && component[upper_end].is_ascii_uppercase() {
                    upper_end += 1;
                }
                let title_follows = component.get(upper_end).is_some_and(u8::is_ascii_lowercase);
                if title_follows && upper_end - index > 1 {
                    upper_end - 1
                } else if title_follows {
                    lowercase_run_end(component, upper_end)
                } else {
                    upper_end
                }
            } else if index == 0 {
                lowercase_run_end(component, index)
            } else {
                // Only a component's first word may open lowercase; one that
                // follows a digit run (`7kq`) is a random fragment.
                return false;
            };
            index = end;
            let word = &component[start..end];
            let version_marker = word.len() == 1
                && word[0].is_ascii_uppercase()
                && component.get(end).is_some_and(u8::is_ascii_digit);
            if version_marker {
                continue;
            }
            let acronym = word.iter().all(u8::is_ascii_uppercase);
            if acronym && word.len() > MAX_ACRONYM_LENGTH && (has_lowercase || !screaming) {
                return false;
            }
            if has_consonant_cluster(word) {
                return false;
            }
            words += 1;
            letters += word.len();
            short_words += usize::from(word.len() <= 2);
            vowelless_words += usize::from(word.len() >= 2 && !word.iter().any(is_vowel));
        }
    }
    words > 0
        && short_words <= MAX_SHORT_WORDS
        && vowelless_words <= MAX_VOWELLESS_WORDS
        && digit_runs <= MAX_DIGIT_RUNS
        && letters * 10 >= words * MIN_MEAN_WORD_LENGTH_TENTHS
}

fn lowercase_run_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_lowercase() {
        index += 1;
    }
    index
}

fn is_vowel(byte: &u8) -> bool {
    matches!(
        byte.to_ascii_lowercase(),
        b'a' | b'e' | b'i' | b'o' | b'u' | b'y'
    )
}

fn has_consonant_cluster(word: &[u8]) -> bool {
    word.split(is_vowel)
        .any(|cluster| cluster.len() > MAX_CONSONANT_CLUSTER)
}

const MAX_ACRONYM_LENGTH: usize = 4;
const MAX_CONSONANT_CLUSTER: usize = 4;
const MAX_SHORT_WORDS: usize = 2;
const MAX_VOWELLESS_WORDS: usize = 2;
const MAX_DIGIT_RUNS: usize = 2;
const MIN_MEAN_WORD_LENGTH_TENTHS: usize = 35;

const MIN_TOKEN_LENGTH: usize = 36;
const MIN_TOKEN_ENTROPY_PER_MILLE: u32 = 4_200;
const ENTROPY_SCALE: u128 = 1 << 20;

fn fixed_log2(value: usize) -> u128 {
    debug_assert!(value > 0);
    let integer = usize::BITS - 1 - value.leading_zeros();
    let mut result = u128::from(integer) * ENTROPY_SCALE;
    let mut normalized = (value as u128) << (63 - integer);
    for bit in 1..=20 {
        normalized = (normalized * normalized) >> 63;
        if normalized >= (2_u128 << 63) {
            normalized >>= 1;
            result += ENTROPY_SCALE >> bit;
        }
    }
    result
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;

    struct KeySet(BTreeSet<String>);

    impl SensitiveKeyPolicy for KeySet {
        type Match = ();

        fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match> {
            self.0.contains(key.ascii_compact()).then_some(())
        }
    }

    #[test]
    fn normalizes_keys_once_for_policy_adapters() {
        let key = NormalizedSensitiveKey::new(" Client--Secret ");
        assert_eq!(key.ascii_compact(), "clientsecret");
        assert_eq!(key.separated(), "client_secret");
        assert_eq!(key.compact(), "clientsecret");

        let camel_case = NormalizedSensitiveKey::new("refreshToken");
        assert_eq!(camel_case.separated(), "refresh_token");
        assert_eq!(
            NormalizedSensitiveKey::new("vendorAPIKey").separated(),
            "vendor_api_key"
        );
        assert_eq!(
            NormalizedSensitiveKey::new("JWTToken").separated(),
            "jwt_token"
        );
    }

    #[test]
    fn recursively_redacts_structured_sensitive_values() {
        let policy = KeySet(BTreeSet::from(["token".to_string()]));
        let mut value = json!({"outer": [{"token": "hidden"}], "safe": "kept"});
        let mut visited = Vec::new();
        let changed = visit_sensitive_json_mut(&mut value, &policy, |value, path| match value {
            JsonVisitMut::SensitiveValue(child, ()) => {
                assert_eq!(
                    path,
                    &[
                        JsonPathSegment::Field(0),
                        JsonPathSegment::Index(0),
                        JsonPathSegment::Field(0)
                    ]
                );
                *child = Value::String("redacted".to_string());
                true
            }
            JsonVisitMut::String(text) => {
                visited.push((text.clone(), path.to_vec()));
                false
            }
        });

        assert!(changed);
        assert_eq!(value["outer"][0]["token"], "redacted");
        assert_eq!(value["safe"], "kept");
        assert_eq!(
            visited,
            vec![("kept".to_string(), vec![JsonPathSegment::Field(1)])]
        );
    }

    #[test]
    fn replaced_sensitive_values_are_not_visited_recursively() {
        let policy = KeySet(BTreeSet::from(["credential".to_string()]));
        let mut value = json!({
            "credential": {"nested": "secret = p@ssw0rd!"},
            "safe": "kept"
        });
        let mut strings = Vec::new();

        visit_sensitive_json_mut(&mut value, &policy, |value, _| match value {
            JsonVisitMut::SensitiveValue(child, ()) => {
                *child = Value::String("redacted".to_string());
                true
            }
            JsonVisitMut::String(text) => {
                strings.push(text.clone());
                false
            }
        });

        assert_eq!(strings, ["kept"]);
    }

    #[test]
    fn entropy_kernel_finds_tokens_and_excludes_hex_digests() {
        let token = "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";
        assert!(looks_high_entropy_token(token));
        assert_eq!(
            high_entropy_ranges(&format!("value: {token}"), EntropyScope::Value),
            vec![7..7 + token.len()]
        );
        assert!(!looks_high_entropy_token(
            "3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5"
        ));

        let below_threshold = "abcdefghi123456789".repeat(2);
        let above_threshold = "abcdefghij123456789".repeat(2);
        assert!(!looks_high_entropy_token(&below_threshold));
        assert!(looks_high_entropy_token(&above_threshold));
    }

    #[test]
    fn entropy_kernel_preserves_ordinary_macos_temp_paths() {
        for path in [
            "/private/var/folders/ab/cd0123456789abcdefghijklmnopqrst/T/",
            "/private/var/folders/ab/cd0123456789abcdefghijklmnopqrst/T/fe600511-e406-4142-b74a-cbf177c5f2cb/",
        ] {
            assert!(looks_high_entropy_token(path));
            assert!(high_entropy_ranges(path, EntropyScope::Value).is_empty());
        }
    }

    #[test]
    fn entropy_kernel_redacts_high_entropy_macos_temp_path_components() {
        let secret = "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";
        let path = format!("/private/var/folders/ab/{secret}/T/");
        let start = "/private/var/folders/ab/".len();
        assert_eq!(
            high_entropy_ranges(&path, EntropyScope::Value),
            vec![start..start + secret.len()]
        );
    }

    #[test]
    fn entropy_kernel_redacts_slash_bearing_secrets_with_short_components() {
        let secret = "/AbCdEfGhIjKlMnOpQrStUvWxYz01234/56789aBcDeFgHiJkLmNoPqRsTuVwXy";
        assert!(secret.split('/').all(|part| part.len() < 36));
        assert!(looks_high_entropy_token(secret));
        assert_eq!(
            high_entropy_ranges(secret, EntropyScope::Value),
            vec![0..secret.len()]
        );
    }

    #[test]
    fn entropy_kernel_preserves_absolute_paths_with_uuid_directories() {
        let observed = "/run/nl/w/fe600511-e406-4142-b74a-cbf177c5f2cb/tmp/";
        assert!(looks_high_entropy_token(observed));
        for path in [
            observed,
            "/workspace/artifacts/fe600511-e406-4142-b74a-cbf177c5f2cb/project/src/",
            "/workspace/FE600511-E406-4142-B74A-CBF177C5F2CB/source/",
        ] {
            assert!(
                high_entropy_ranges(path, EntropyScope::Value).is_empty(),
                "{path}"
            );
        }
        let malformed = "/run/nl/w/fe600511-e406-4142-b74a-cbf177c5f2cg/tmp/";
        assert_eq!(
            high_entropy_ranges(malformed, EntropyScope::Value),
            vec![0..malformed.len()]
        );
    }

    #[test]
    fn entropy_kernel_redacts_secrets_inside_uuid_directory_paths() {
        let prefix = "/run/nl/w/fe600511-e406-4142-b74a-cbf177c5f2cb/tmp/";
        for secret in [
            "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE",
            "AbCdEfGhIjKlMnOpQrStUvWxYz01234/56789aBcDeFgHiJkLmNoPqRsTuVwXy",
        ] {
            let path = format!("{prefix}{secret}");
            assert_eq!(
                high_entropy_ranges(&path, EntropyScope::Value),
                vec![prefix.len()..prefix.len() + secret.len()]
            );
        }
    }

    #[test]
    fn uuid_paths_preserve_dated_build_artifact_locations() {
        for uuid in [
            "00000000-0000-0000-0000-000000000000",
            "fe600511-e406-4142-b74a-cbf177c5f2cb",
        ] {
            for artifact in [
                "build_20261018_x86_64_linux",
                "build_20261018_x86_64_linux/typescript",
                "typescript/build_20261018_x86_64_linux",
                "build_20261018_x86_64_linux/javascript",
                "build_20261018_x86_64_linux/documentation",
                "build_20261018_x86_64_linux/generated_sources",
                "build_20261018_x86_64_linux/typescript_v2_01",
                "typescript_v2_01/build_20261018_x86_64_linux",
            ] {
                let path = format!("/workspace/{uuid}/dist/{artifact}");
                assert_eq!(
                    high_entropy_ranges(&path, EntropyScope::Value),
                    Vec::<Range<usize>>::new(),
                    "{path}"
                );
                let metadata = json!({"path": path});
                assert_eq!(
                    crate::sanitize_provider_metadata_json(&metadata.to_string(), 4096)
                        .expect("structured artifact metadata"),
                    metadata
                );
            }
        }
    }

    #[test]
    fn uuid_paths_do_not_dilute_adjacent_short_secret_components() {
        let prefix = format!(
            "/workspace/fe600511-e406-4142-b74a-cbf177c5f2cb/{}/",
            "a".repeat(512)
        );
        let secret = "AbCdEfGhIjKlMnOpQrStUvWxYz01234/56789aBcDeFgHiJkLmNoPqRsTuVwXy";
        assert!(secret.split('/').all(|component| component.len() < 36));
        assert!(looks_high_entropy_token(secret));
        let path = format!("{prefix}{secret}/source");
        assert!(!looks_high_entropy_token(&path));
        assert_eq!(
            high_entropy_ranges(&path, EntropyScope::Value),
            vec![prefix.len()..prefix.len() + secret.len()]
        );
    }

    #[test]
    fn uuid_paths_detect_tokens_with_a_sparse_neighbor_component() {
        let prefix = "/workspace/fe600511-e406-4142-b74a-cbf177c5f2cb/";
        let letters = "AbCdEfGhIjKlMnOpQrStUvWxYzABCD";
        let digits = "012345678901234567890123456789";
        for secret in [format!("{letters}/{digits}"), format!("{digits}/{letters}")] {
            assert!(secret.split('/').all(|component| component.len() < 36));
            assert!(looks_high_entropy_token(&secret));
            let path = format!("{prefix}{secret}/source");
            assert_eq!(
                high_entropy_ranges(&path, EntropyScope::Value),
                vec![prefix.len()..prefix.len() + secret.len()]
            );
        }
    }

    #[test]
    fn uuid_paths_redact_slash_bearing_secrets_with_two_sparse_components() {
        let secret = "abcdefghijklmnopabcdefghijklmnop/QRSTUVWXYZ012345QRSTUVWXYZ012345";
        for component in secret.split('/') {
            assert!(entropy_bits_per_mille(component).unwrap() < MIN_TOKEN_ENTROPY_PER_MILLE);
        }
        assert!(looks_high_entropy_token(secret));
        let prefix = format!(
            "/workspace/fe600511-e406-4142-b74a-cbf177c5f2cb/{}/",
            "a".repeat(512)
        );
        let path = format!("{prefix}{secret}/source");
        assert!(!looks_high_entropy_token(&path));
        assert_eq!(
            high_entropy_ranges(&path, EntropyScope::Value),
            vec![prefix.len()..prefix.len() + secret.len()]
        );
        let metadata = json!({"path": path});
        let sanitized = crate::sanitize_provider_metadata_json(&metadata.to_string(), 4096)
            .expect("structured path metadata");
        let sanitized_path = sanitized["path"].as_str().unwrap();
        assert!(sanitized_path.starts_with(&prefix));
        assert!(sanitized_path.ends_with("/source"));
        assert!(!sanitized_path.contains(secret));
        assert!(
            secret
                .split('/')
                .all(|component| !sanitized_path.contains(component))
        );
        let (first, second) = secret.split_once('/').unwrap();
        let separated = format!("/{first}/fe600511-e406-4142-b74a-cbf177c5f2cb/{second}");
        assert!(
            high_entropy_ranges(&separated, EntropyScope::Value).is_empty(),
            "UUID identity must separate the candidate components"
        );
        let compact_secret = "abcdefghijklmnopab/QRSTUVWXYZ012345QR";
        assert!(looks_high_entropy_token(compact_secret));
        for component in compact_secret.split('/') {
            assert!(entropy_bits_per_mille(component).unwrap() < MIN_TOKEN_ENTROPY_PER_MILLE);
        }
        let compact_path = format!("{prefix}{compact_secret}/source");
        assert_eq!(
            high_entropy_ranges(&compact_path, EntropyScope::Value),
            vec![prefix.len()..prefix.len() + compact_secret.len()]
        );
    }

    #[test]
    fn uuid_paths_redact_minimum_width_tokens_at_every_internal_slash_position() {
        let prefix = format!(
            "/workspace/fe600511-e406-4142-b74a-cbf177c5f2cb/{}/",
            "a".repeat(512)
        );
        for payload in [
            "abcdefghijklmnopaQRSTUVWXYZ012345QR",
            "abcdefghijklmnOPQRSTUVWXYZ012345OPQ",
        ] {
            assert_eq!(payload.len() + 1, MIN_TOKEN_LENGTH);
            for split in 1..payload.len() {
                let secret = format!("{}/{}", &payload[..split], &payload[split..]);
                assert!(looks_high_entropy_token(&secret));
                let path = format!("{prefix}{secret}/source");
                assert!(!looks_high_entropy_token(&path));
                assert_eq!(
                    high_entropy_ranges(&path, EntropyScope::Value),
                    vec![prefix.len()..prefix.len() + secret.len()],
                    "{payload} split at {split}"
                );
                let metadata = json!({"path": path});
                let sanitized = crate::sanitize_provider_metadata_json(&metadata.to_string(), 4096)
                    .expect("structured path metadata");
                let source = sanitized["path"].as_str().unwrap();
                assert!(!source.contains(&secret), "{payload} split at {split}");
                assert!(source.starts_with(&prefix));
                assert!(source.ends_with("/source"));
            }
        }
    }

    #[test]
    fn uuid_directory_paths_do_not_dilute_secret_components() {
        let prefix = format!(
            "/workspace/fe600511-e406-4142-b74a-cbf177c5f2cb/{}/",
            "a".repeat(512)
        );
        let secret = "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";
        let path = format!("{prefix}{secret}");
        assert!(!looks_high_entropy_token(&path));
        assert_eq!(
            high_entropy_ranges(&path, EntropyScope::Value),
            vec![prefix.len()..path.len()]
        );
    }

    #[test]
    fn structured_metadata_keeps_uuid_paths_but_redacts_path_credentials() {
        let prefix = "/run/nl/w/fe600511-e406-4142-b74a-cbf177c5f2cb/tmp/";
        let credential = "ghp_KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ";
        let metadata = json!({
            "path": format!("{prefix}.tmp4cknUf/project/src/lib.rs"),
            "source": format!("{prefix}{credential}/source"),
        });
        let sanitized = crate::sanitize_provider_metadata_json(&metadata.to_string(), 4096)
            .expect("structured path metadata");
        assert_eq!(sanitized["path"], metadata["path"]);
        assert!(!sanitized["source"].as_str().unwrap().contains(credential));
    }

    #[test]
    fn compound_identifiers_are_exempt_only_as_names() {
        for identifier in [
            "WorkDuplicateAdjudicationAppendOutcomeV1",
            "GitHubReadOnlyCredentialAuthorityOutcomeV1",
            "RootScopeOutcomeV1_for_Nullable_string",
            "clone_peak_rss_added_at_most_1_gib_for_39582_symbols",
            "benchmark_data/index-bench/clone-envelope-20260915-fingerprint-staging",
            "MAXIMUM_WORKFLOW_PAYLOAD_JSON_BYTES_FOR_GRAPH_V2",
        ] {
            assert!(looks_high_entropy_token(identifier), "{identifier}");
            assert!(is_compound_identifier(identifier), "{identifier}");
            assert!(
                high_entropy_ranges(identifier, EntropyScope::Key).is_empty(),
                "{identifier}"
            );
            assert!(
                !high_entropy_ranges(identifier, EntropyScope::Value).is_empty(),
                "{identifier}"
            );
        }
    }

    #[test]
    fn identifier_words_do_not_launder_random_material() {
        for token in [
            "session_token_KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ",
            "WorkDuplicateAdjudication/x7Kq2mZ9vB4nR6cX1jH8pL3d",
            "AppendOutcomeV1_aGVsbG8gd29ybGQgc2VjcmV0IHRva2Vu",
            "ARiz6HYp7Iq8_Jr9-KsALtBMuCNvDOwEPxFQyGRzHS0ITaJU",
        ] {
            assert!(looks_high_entropy_token(token), "{token}");
        }
    }

    /// Uniform random tokens at the length floor rarely decompose into
    /// identifier words; the exemption must stay a rounding error against
    /// them, not a gap a generated credential falls through.
    #[test]
    fn random_tokens_are_almost_never_exempted_as_identifiers() {
        let alphabets: [&[u8]; 4] = [
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
            b"abcdefghijklmnopqrstuvwxyz0123456789",
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        let mut eligible = 0usize;
        let mut exempted = 0usize;
        for alphabet in alphabets {
            for _ in 0..5_000 {
                let token: String = (0..MIN_TOKEN_LENGTH)
                    .map(|_| char::from(alphabet[next(alphabet.len())]))
                    .collect();
                if looks_high_entropy_token(&token) {
                    eligible += 1;
                    exempted += usize::from(is_compound_identifier(&token));
                }
            }
        }
        assert!(
            eligible > 15_000,
            "fixture produced {eligible} eligible tokens"
        );
        assert!(
            exempted * 10_000 < eligible,
            "{exempted} of {eligible} random tokens were exempted"
        );
    }
}
