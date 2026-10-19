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

pub(crate) fn high_entropy_ranges(text: &str) -> Vec<Range<usize>> {
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
        } else if looks_high_entropy_token(candidate) && !is_lcm_payload_ref(candidate) {
            ranges.push(start..end);
        }
        start = end;
    }
    ranges
}

// UUIDs contribute identity, not credential entropy. Adjacent dense components
// form one token. Sparse pairs need independent information on each side;
// mixing two ordinary directory alphabets is not itself credential evidence.
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
                && has_token_fragment_information(&text[previous.clone()])
                && has_token_fragment_information(component)
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

fn has_token_fragment_information(component: &str) -> bool {
    entropy_bits_per_mille(component).is_none_or(|score| {
        u128::from(score) * component.len() as u128 >= MIN_TOKEN_FRAGMENT_INFORMATION_PER_MILLE
    })
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

const MIN_TOKEN_LENGTH: usize = 36;
const MIN_TOKEN_ENTROPY_PER_MILLE: u32 = 4_200;
// Joining two alphabets contributes at most one bit per byte of mixing entropy.
// Each sparse side must supply half the minimum token's remaining information.
const MIN_TOKEN_FRAGMENT_INFORMATION_PER_MILLE: u128 =
    MIN_TOKEN_LENGTH as u128 * (MIN_TOKEN_ENTROPY_PER_MILLE - 1_000) as u128 / 2;
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
            high_entropy_ranges(&format!("value: {token}")),
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
            assert!(high_entropy_ranges(path).is_empty());
        }
    }

    #[test]
    fn entropy_kernel_redacts_high_entropy_macos_temp_path_components() {
        let secret = "Qm9vZ2llV29vZ2llMTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3A4OTc2NTQzMjE";
        let path = format!("/private/var/folders/ab/{secret}/T/");
        let start = "/private/var/folders/ab/".len();
        assert_eq!(
            high_entropy_ranges(&path),
            vec![start..start + secret.len()]
        );
    }

    #[test]
    fn entropy_kernel_redacts_slash_bearing_secrets_with_short_components() {
        let secret = "/AbCdEfGhIjKlMnOpQrStUvWxYz01234/56789aBcDeFgHiJkLmNoPqRsTuVwXy";
        assert!(secret.split('/').all(|part| part.len() < 36));
        assert!(looks_high_entropy_token(secret));
        assert_eq!(high_entropy_ranges(secret), vec![0..secret.len()]);
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
            assert!(high_entropy_ranges(path).is_empty(), "{path}");
        }
        let malformed = "/run/nl/w/fe600511-e406-4142-b74a-cbf177c5f2cg/tmp/";
        assert_eq!(high_entropy_ranges(malformed), vec![0..malformed.len()]);
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
                high_entropy_ranges(&path),
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
            ] {
                let path = format!("/workspace/{uuid}/dist/{artifact}");
                assert_eq!(
                    high_entropy_ranges(&path),
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
            high_entropy_ranges(&path),
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
                high_entropy_ranges(&path),
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
            high_entropy_ranges(&path),
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
            high_entropy_ranges(&separated).is_empty(),
            "UUID identity must separate the candidate components"
        );
        let compact_secret = "abcdefghijklmnopab/QRSTUVWXYZ012345QR";
        assert!(looks_high_entropy_token(compact_secret));
        for component in compact_secret.split('/') {
            assert!(entropy_bits_per_mille(component).unwrap() < MIN_TOKEN_ENTROPY_PER_MILLE);
        }
        let compact_path = format!("{prefix}{compact_secret}/source");
        assert_eq!(
            high_entropy_ranges(&compact_path),
            vec![prefix.len()..prefix.len() + compact_secret.len()]
        );
    }

    #[test]
    fn uuid_paths_redact_minimum_width_tokens_at_every_slash_boundary() {
        let material = "abcdefghijklmnopaQRSTUVWXYZ012345QR";
        let prefix = format!(
            "/workspace/fe600511-e406-4142-b74a-cbf177c5f2cb/{}/",
            "a".repeat(512)
        );
        for split in 1..material.len() {
            let secret = format!("{}/{}", &material[..split], &material[split..]);
            assert!(looks_high_entropy_token(&secret), "split at {split}");
            let path = format!("{prefix}{secret}/source");
            assert!(!looks_high_entropy_token(&path));
            assert_eq!(
                high_entropy_ranges(&path),
                vec![prefix.len()..prefix.len() + secret.len()],
                "split at {split}"
            );
            let metadata = json!({"path": path});
            let sanitized = crate::sanitize_provider_metadata_json(&metadata.to_string(), 4096)
                .expect("structured split-token metadata");
            let sanitized_path = sanitized["path"].as_str().unwrap();
            assert!(sanitized_path.starts_with(&prefix));
            assert!(sanitized_path.ends_with("/source"));
            assert!(!sanitized_path.contains(&secret), "split at {split}");
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
        assert_eq!(high_entropy_ranges(&path), vec![prefix.len()..path.len()]);
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
}
