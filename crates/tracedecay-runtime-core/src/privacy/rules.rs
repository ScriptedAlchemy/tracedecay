//! Credential rules: a vendored community catalogue plus TraceDecay's local
//! supplement.
//!
//! TraceDecay used to carry its own short list of provider token shapes. Seven
//! alternations maintained by hand is not a credible answer to "what does a
//! leaked key look like", so the catalogue is now vendored from gitleaks (MIT)
//! and this module compiles it. See
//! `rules/vendor/gitleaks/PROVENANCE.md` for the source commit, the licence,
//! the deviations, and the refresh procedure; `rules/supplement.toml` for the
//! handful of rules upstream has no equivalent for.
//!
//! What is *not* vendored is the engine. The bounded scan, the parse-before-scan
//! structured layer, the entropy kernel, the typed findings and assessments, and
//! the redaction merge all stay TraceDecay's. Vendored rules are data feeding
//! the same [`CredentialPattern`] the detector always consumed, which is why
//! this change is invisible to `detect.rs`, `structured_text.rs`, and
//! `memory::hygiene` beyond the error type.
//!
//! Both documents are bound with [`include_str!`], so the ruleset is fixed at
//! compile time: no filesystem read, no network, no ordering nondeterminism.
//! Compilation failure is a typed [`CredentialRuleSetError`] that names the
//! document and the offending rule id. It is never an empty ruleset — a
//! detector that silently stops detecting is the one failure mode a privacy
//! boundary cannot have.

use std::collections::BTreeSet;
use std::ops::Range;

use regex::{Captures, Match, Regex};
use serde::Deserialize;
use thiserror::Error;

use super::detector_kernel::entropy_bits_per_mille;

/// Community catalogue, byte-for-byte upstream. Do not edit; refresh per
/// `rules/vendor/gitleaks/PROVENANCE.md`.
const VENDORED_RULES_TOML: &str = include_str!("rules/vendor/gitleaks/gitleaks.toml");
const VENDORED_SOURCE: &str = "vendor/gitleaks/gitleaks.toml";

/// TraceDecay-local rules with no community equivalent.
const SUPPLEMENT_RULES_TOML: &str = include_str!("rules/supplement.toml");
const SUPPLEMENT_SOURCE: &str = "supplement.toml";

/// Upstream's generated "context" rules all open with this preamble: an
/// unanchored run of identifier bytes ahead of the provider keyword. Its
/// presence is what distinguishes a rule that matches `provider_key = <secret>`
/// — an assignment, whose match is mostly context — from one that matches a
/// self-identifying token. The distinction decides which detector a finding is
/// attributed to, and a finding that misnames its detector is worse than no
/// finding.
const VENDORED_ASSIGNMENT_PREAMBLE: &str = r"(?i)[\w.-]{0,50}?";

/// The one vendored rule whose subject is a private key rather than a provider
/// token. Upstream has no field for this; inferring it from the regex would be
/// guesswork, so it is named.
const VENDORED_PRIVATE_KEY_RULE: &str = "private-key";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialPatternKind {
    PrivateKey,
    BearerToken,
    KnownCredential,
    CredentialAssignment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialPatternProfile {
    Observation,
    Memory,
}

impl CredentialPatternProfile {
    fn token(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Memory => "memory",
        }
    }
}

/// Why a credential ruleset could not be built.
///
/// Every variant names the document and, where it exists, the rule id.
/// Deliberately carries no regex source and no scanned text: this error is
/// allowed to reach a log, and the privacy boundary's whole contract is that
/// matched bytes never do.
#[derive(Debug, Error)]
pub(crate) enum CredentialRuleSetError {
    #[error("credential rule document `{document}` is not valid TOML: {reason}")]
    Document {
        document: &'static str,
        reason: String,
    },
    #[error("credential rule document `{document}` declares no usable rules")]
    Empty { document: &'static str },
    #[error("credential rule `{rule_id}` in `{document}` has an unsupported regex")]
    Regex {
        document: &'static str,
        rule_id: String,
    },
    #[error("credential rule `{rule_id}` in `{document}` is invalid: {reason}")]
    Rule {
        document: &'static str,
        rule_id: String,
        reason: &'static str,
    },
}

/// One compiled credential rule.
///
/// The surface is unchanged from the hand-written era — [`Self::kind`],
/// [`Self::is_match`], [`Self::ranges`] — so every caller kept working. What
/// changed is behind it: matches now pass a rule's entropy floor and its
/// allowlists before they count.
pub(crate) struct CredentialPattern {
    id: String,
    kind: CredentialPatternKind,
    regex: Regex,
    /// Opts into the bounded key=value scan: extend the match past the
    /// delimiter across a quoted or unquoted value, and drop it when the value
    /// is shorter than this. Supplement-only.
    assignment_min_len: Option<usize>,
    secret_group: Option<usize>,
    /// Exclusive lower bound, in Shannon bits per character scaled by 1000, on
    /// the extracted secret. Upstream `entropy`, scored by our kernel.
    min_entropy_per_mille: Option<u32>,
    allowlists: Vec<CompiledAllowlist>,
}

impl CredentialPattern {
    pub fn kind(&self) -> CredentialPatternKind {
        self.kind
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub fn is_match(&self, text: &str) -> bool {
        // Cheap structural reject first. The regex crate's literal prefilter
        // makes this a memchr-class scan for the overwhelming majority of
        // rules, which is what keeps a 200-rule catalogue affordable inline at
        // ingest; only a candidate pays for capture groups and gating.
        if !self.regex.is_match(text) {
            return false;
        }
        !self.ranges(text).is_empty()
    }

    /// Byte ranges to redact. The whole match, not just the secret group: a
    /// sanitizer must not leave secret bytes behind, and the context a rule
    /// matched on is itself worth removing.
    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        if let Some(min_len) = self.assignment_min_len {
            return credential_assignment_ranges(
                text,
                &self.regex,
                min_len,
                self.id == SOURCE_ASSIGNMENT_RULE_ID,
            )
            .collect();
        }
        self.regex
            .captures_iter(text)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                let secret = self.secret(&captures).unwrap_or(whole);
                self.admits(text, whole, secret).then(|| whole.range())
            })
            .collect()
    }

    /// Upstream's secret extraction: the named group when a rule declares one,
    /// otherwise the first non-empty capture, otherwise the whole match.
    fn secret<'t>(&self, captures: &Captures<'t>) -> Option<Match<'t>> {
        if let Some(group) = self.secret_group {
            return captures.get(group);
        }
        (1..captures.len()).find_map(|index| captures.get(index).filter(|found| !found.is_empty()))
    }

    fn admits(&self, text: &str, whole: Match<'_>, secret: Match<'_>) -> bool {
        // Abstention keeps the finding. A score we cannot represent is not
        // evidence that the token is innocent.
        if let Some(threshold) = self.min_entropy_per_mille
            && let Some(score) = entropy_bits_per_mille(secret.as_str())
            && score <= threshold
        {
            return false;
        }
        !self
            .allowlists
            .iter()
            .any(|allowlist| allowlist.excuses(text, whole, secret))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllowlistTarget {
    Secret,
    Match,
    Line,
}

struct CompiledAllowlist {
    /// `condition = "AND"`: every criterion the allowlist declares must hit.
    all_of: bool,
    target: AllowlistTarget,
    regexes: Vec<Regex>,
    /// Lowercased at load; compared as substrings, as upstream does.
    stopwords: Vec<String>,
}

impl CompiledAllowlist {
    fn excuses(&self, text: &str, whole: Match<'_>, secret: Match<'_>) -> bool {
        let target = match self.target {
            AllowlistTarget::Secret => secret.as_str(),
            AllowlistTarget::Match => whole.as_str(),
            AllowlistTarget::Line => line_containing(text, whole.start()),
        };
        let regex_hit = self.regexes.iter().any(|regex| regex.is_match(target));
        let stopword_hit = !self.stopwords.is_empty() && {
            let lowered = target.to_ascii_lowercase();
            self.stopwords
                .iter()
                .any(|stopword| lowered.contains(stopword.as_str()))
        };
        if self.all_of {
            (self.regexes.is_empty() || regex_hit) && (self.stopwords.is_empty() || stopword_hit)
        } else {
            regex_hit || stopword_hit
        }
    }
}

/// `regexTarget = "line"` resolved against the scanned text rather than a file:
/// the line the match starts on.
fn line_containing(text: &str, offset: usize) -> &str {
    let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    &text[start..end]
}

/// Compiles the rules a profile runs: the local supplement first, then the
/// vendored catalogue.
///
/// Supplement-first is load-bearing for the merge in `detect::redact_text`,
/// which resolves overlapping candidates by kind priority and, at equal
/// priority, by the order it saw them.
pub(crate) fn compile_credential_patterns(
    profile: CredentialPatternProfile,
) -> Result<Vec<CredentialPattern>, CredentialRuleSetError> {
    let mut patterns = compile_document(
        SUPPLEMENT_SOURCE,
        SUPPLEMENT_RULES_TOML,
        RuleOrigin::Supplement,
        profile,
    )?;
    patterns.extend(compile_document(
        VENDORED_SOURCE,
        VENDORED_RULES_TOML,
        RuleOrigin::Vendored,
        profile,
    )?);

    let mut seen = BTreeSet::new();
    for pattern in &patterns {
        if !seen.insert(pattern.id.as_str()) {
            return Err(CredentialRuleSetError::Rule {
                document: VENDORED_SOURCE,
                rule_id: pattern.id.clone(),
                reason: "rule id collides with another loaded rule",
            });
        }
    }
    Ok(patterns)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuleOrigin {
    /// Upstream schema: kind is inferred, every rule runs in every profile.
    Vendored,
    /// TraceDecay schema: kind and profiles are mandatory, because they are the
    /// part upstream has no way to express.
    Supplement,
}

fn compile_document(
    document: &'static str,
    text: &str,
    origin: RuleOrigin,
    profile: CredentialPatternProfile,
) -> Result<Vec<CredentialPattern>, CredentialRuleSetError> {
    let parsed: RuleDocumentToml =
        toml::from_str(text).map_err(|error| CredentialRuleSetError::Document {
            document,
            reason: error.to_string(),
        })?;

    let mut patterns = Vec::new();
    for rule in parsed.rules {
        // Upstream carries a few rules selected purely by file path. TraceDecay
        // scans records in memory, so there is no path to select on and the
        // rule has nothing to match; it is skipped rather than mis-applied.
        let Some(source_regex) = rule.regex.as_deref() else {
            continue;
        };

        let kind = match origin {
            RuleOrigin::Vendored => vendored_kind(&rule.id, source_regex),
            RuleOrigin::Supplement => {
                let Some(kind) = rule.kind.as_deref().and_then(parse_kind) else {
                    return Err(CredentialRuleSetError::Rule {
                        document,
                        rule_id: rule.id,
                        reason: "supplement rule needs a known `kind`",
                    });
                };
                kind
            }
        };

        let profiles = match origin {
            RuleOrigin::Vendored => None,
            RuleOrigin::Supplement => {
                let Some(profiles) = rule.profiles.as_ref() else {
                    return Err(CredentialRuleSetError::Rule {
                        document,
                        rule_id: rule.id,
                        reason: "supplement rule needs a `profiles` list",
                    });
                };
                if profiles.is_empty() {
                    return Err(CredentialRuleSetError::Rule {
                        document,
                        rule_id: rule.id,
                        reason: "supplement rule needs a `profiles` list",
                    });
                }
                Some(profiles)
            }
        };
        if profiles.is_some_and(|profiles| {
            !profiles
                .iter()
                .any(|entry| entry.as_str() == profile.token())
        }) {
            continue;
        }

        let regex = Regex::new(source_regex).map_err(|_| CredentialRuleSetError::Regex {
            document,
            rule_id: rule.id.clone(),
        })?;

        let mut allowlists = Vec::new();
        for allowlist in parsed
            .allowlist
            .iter()
            .chain(parsed.allowlists.iter())
            .chain(rule.allowlist.iter())
            .chain(rule.allowlists.iter())
        {
            if let Some(compiled) = compile_allowlist(document, &rule.id, allowlist)? {
                allowlists.push(compiled);
            }
        }

        patterns.push(CredentialPattern {
            kind,
            regex,
            assignment_min_len: rule.assignment_min_len,
            secret_group: rule.secret_group,
            min_entropy_per_mille: rule.entropy.map(entropy_threshold_per_mille),
            allowlists,
            id: rule.id,
        });
    }

    if patterns.is_empty() {
        return Err(CredentialRuleSetError::Empty { document });
    }
    Ok(patterns)
}

fn compile_allowlist(
    document: &'static str,
    rule_id: &str,
    allowlist: &AllowlistToml,
) -> Result<Option<CompiledAllowlist>, CredentialRuleSetError> {
    // Nothing evaluable: a path-only allowlist cannot excuse an in-memory
    // record, so it is dropped rather than treated as vacuously satisfied.
    if allowlist.regexes.is_empty() && allowlist.stopwords.is_empty() {
        return Ok(None);
    }
    let all_of = allowlist
        .condition
        .as_deref()
        .is_some_and(|condition| condition.eq_ignore_ascii_case("AND"));
    // An AND allowlist over a path criterion can never be satisfied here. Left
    // in, it would excuse nothing and cost a scan; dropped, TraceDecay simply
    // redacts where upstream would have excused. That is the safe direction.
    if all_of && (!allowlist.paths.is_empty() || allowlist.path.is_some()) {
        return Ok(None);
    }

    let mut regexes = Vec::with_capacity(allowlist.regexes.len());
    for source_regex in &allowlist.regexes {
        regexes.push(
            Regex::new(source_regex).map_err(|_| CredentialRuleSetError::Regex {
                document,
                rule_id: rule_id.to_string(),
            })?,
        );
    }

    Ok(Some(CompiledAllowlist {
        all_of,
        target: match allowlist.regex_target.as_deref() {
            Some("match") => AllowlistTarget::Match,
            Some("line") => AllowlistTarget::Line,
            _ => AllowlistTarget::Secret,
        },
        regexes,
        stopwords: allowlist
            .stopwords
            .iter()
            .map(|stopword| stopword.to_ascii_lowercase())
            .collect(),
    }))
}

fn vendored_kind(rule_id: &str, source_regex: &str) -> CredentialPatternKind {
    if rule_id == VENDORED_PRIVATE_KEY_RULE {
        return CredentialPatternKind::PrivateKey;
    }
    if source_regex.starts_with(VENDORED_ASSIGNMENT_PREAMBLE) {
        CredentialPatternKind::CredentialAssignment
    } else {
        CredentialPatternKind::KnownCredential
    }
}

fn parse_kind(token: &str) -> Option<CredentialPatternKind> {
    match token {
        "private_key" => Some(CredentialPatternKind::PrivateKey),
        "bearer_token" => Some(CredentialPatternKind::BearerToken),
        "known_credential" => Some(CredentialPatternKind::KnownCredential),
        "credential_assignment" => Some(CredentialPatternKind::CredentialAssignment),
        _ => None,
    }
}

/// Upstream states thresholds in Shannon bits per character; our kernel reports
/// per mille. A threshold beyond the representable range saturates rather than
/// wrapping to a low bound that would silently admit everything.
fn entropy_threshold_per_mille(bits_per_character: f64) -> u32 {
    let scaled = (bits_per_character * 1_000.0).round();
    if scaled.is_nan() || scaled <= 0.0 {
        0
    } else if scaled >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        scaled as u32
    }
}

#[derive(Deserialize)]
struct RuleDocumentToml {
    #[serde(default)]
    allowlist: Option<AllowlistToml>,
    #[serde(default)]
    allowlists: Vec<AllowlistToml>,
    #[serde(default)]
    rules: Vec<RuleToml>,
}

#[derive(Deserialize)]
struct RuleToml {
    id: String,
    #[serde(default)]
    regex: Option<String>,
    #[serde(default)]
    entropy: Option<f64>,
    #[serde(rename = "secretGroup", default)]
    secret_group: Option<usize>,
    #[serde(default)]
    allowlist: Option<AllowlistToml>,
    #[serde(default)]
    allowlists: Vec<AllowlistToml>,
    // TraceDecay supplement extensions; absent from the vendored schema.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    profiles: Option<Vec<String>>,
    #[serde(default)]
    assignment_min_len: Option<usize>,
}

#[derive(Deserialize)]
struct AllowlistToml {
    #[serde(default)]
    condition: Option<String>,
    #[serde(rename = "regexTarget", default)]
    regex_target: Option<String>,
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(default)]
    stopwords: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    path: Option<String>,
}

const MAX_ASSIGNMENT_SCAN_BYTES: usize = 1_048_576;
const MAX_SOURCE_ASSIGNMENT_INDENT_BYTES: usize = 1_024;
const SOURCE_ASSIGNMENT_RULE_ID: &str = "tracedecay-sensitive-source-assignment-observation";

/// Extends a credential-assignment prefix match across the value that follows
/// it, honouring quoting and stopping at the first real terminator.
///
/// This is the structural half of assignment detection and the reason the local
/// supplement still carries assignment rules: it walks to the value's actual
/// end rather than accepting whatever a character class happens to cover, so a
/// value containing punctuation is still redacted whole, and a value whose
/// closing quote the record truncated is still redacted to the line end.
fn credential_assignment_ranges<'a>(
    text: &'a str,
    prefix: &'a Regex,
    min_len: usize,
    allows_wrapped_source_value: bool,
) -> impl Iterator<Item = Range<usize>> + 'a {
    prefix.find_iter(text).filter_map(move |matched| {
        let prefix_end = matched.end();
        let limit = prefix_end
            .saturating_add(MAX_ASSIGNMENT_SCAN_BYTES)
            .min(text.len());
        let bytes = text.as_bytes();
        let value_start = if allows_wrapped_source_value {
            match source_assignment_value_start(bytes, prefix_end, limit) {
                Some(value_start) => value_start,
                None => return Some(matched.start()..limit),
            }
        } else {
            prefix_end
        };
        if bytes
            .get(value_start)
            .is_some_and(|byte| matches!(*byte, b'=' | b'>'))
        {
            return None;
        }
        let line_end = bytes[value_start..limit]
            .iter()
            .position(|byte| matches!(*byte, b'\r' | b'\n'))
            .map_or(limit, |offset| value_start + offset);

        if let Some(raw) = rust_raw_string(bytes, value_start, limit) {
            let mut cursor = raw.content_start;
            while cursor < limit {
                if bytes[cursor] == b'"'
                    && bytes
                        .get(cursor + 1..cursor + 1 + raw.hash_count)
                        .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
                {
                    if cursor.saturating_sub(raw.content_start) < min_len {
                        return None;
                    }
                    return Some(matched.start()..cursor + 1 + raw.hash_count);
                }
                cursor += 1;
            }

            // A malformed raw string can continue over line breaks. Do not let
            // an unproved terminator expose its eventual value.
            return Some(matched.start()..limit);
        }

        let quote = bytes
            .get(value_start)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        let content_start = value_start + usize::from(quote.is_some());
        let mut cursor = content_start;
        let mut closed = false;
        let mut unsupported_value_syntax = false;

        while cursor < line_end {
            let byte = bytes[cursor];
            let escaped = quote.is_some_and(|quote| {
                byte == quote
                    && bytes[content_start..cursor]
                        .iter()
                        .rev()
                        .take_while(|&&previous| previous == b'\\')
                        .count()
                        % 2
                        == 1
            });
            if quote.is_some_and(|quote| byte == quote) && !escaped {
                closed = true;
                break;
            }
            if quote.is_none()
                && matches!(
                    byte,
                    b' ' | b'\t' | b',' | b';' | b'}' | b']' | b'"' | b'\'' | b'(' | b'{' | b'['
                )
            {
                unsupported_value_syntax = matches!(byte, b'"' | b'\'' | b'(' | b'{' | b'[')
                    || matches!(byte, b' ' | b'\t')
                        && bytes[cursor..line_end]
                            .iter()
                            .skip_while(|next| matches!(**next, b' ' | b'\t'))
                            .next()
                            == Some(&b'(');
                break;
            }
            cursor += 1;
        }

        if unsupported_value_syntax {
            // Wrapper and constructor forms (for example `Some("secret")`)
            // are not plain values. Redact the rest of the record line rather
            // than stopping just before the wrapped secret.
            return Some(matched.start()..line_end);
        }

        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        if cursor.saturating_sub(content_start) < min_len {
            return None;
        }
        let end = cursor + usize::from(closed);
        Some(matched.start()..end)
    })
}

/// Source formatters may put an assigned value on the next indented line. This
/// is deliberately unavailable to generic dotenv/memory assignments, whose
/// values are line-delimited by contract.
fn source_assignment_value_start(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    if !bytes
        .get(start)
        .is_some_and(|byte| matches!(*byte, b'\r' | b'\n'))
    {
        return Some(start);
    }

    let max = start
        .saturating_add(MAX_SOURCE_ASSIGNMENT_INDENT_BYTES)
        .min(limit);
    let mut cursor = start;
    while cursor < max && matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n') {
        cursor += 1;
    }
    if cursor == max && cursor < limit && matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n') {
        return None;
    }
    (cursor < limit).then_some(cursor)
}

struct RustRawString {
    content_start: usize,
    hash_count: usize,
}

/// Recognizes Rust `r"…"`, `r#"…"#`, and byte-raw `br#"…"#` prefixes at an
/// assignment value boundary. The caller owns terminator validation.
fn rust_raw_string(bytes: &[u8], start: usize, limit: usize) -> Option<RustRawString> {
    let mut cursor = start;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;

    let hash_start = cursor;
    while cursor < limit && bytes[cursor] == b'#' {
        cursor += 1;
    }
    (cursor < limit && bytes[cursor] == b'"').then_some(RustRawString {
        content_start: cursor + 1,
        hash_count: cursor - hash_start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(profile: CredentialPatternProfile) -> Vec<CredentialPattern> {
        compile_credential_patterns(profile).expect("credential ruleset compiles")
    }

    fn rule<'a>(patterns: &'a [CredentialPattern], id: &str) -> &'a CredentialPattern {
        patterns
            .iter()
            .find(|pattern| pattern.id() == id)
            .unwrap_or_else(|| panic!("rule `{id}` is loaded"))
    }

    /// The whole catalogue compiles under Rust's regex engine. Upstream targets
    /// Go's RE2, which shares the no-backreference/no-lookaround restriction,
    /// and this test is what would tell us if that ever stopped being true.
    #[test]
    fn both_documents_compile_for_every_profile() {
        for profile in [
            CredentialPatternProfile::Observation,
            CredentialPatternProfile::Memory,
        ] {
            let compiled = patterns(profile);
            assert!(
                compiled.len() > 200,
                "expected the vendored catalogue, got {} rules",
                compiled.len()
            );
            assert!(
                compiled
                    .iter()
                    .any(|pattern| pattern.id().starts_with("tracedecay-")),
                "the local supplement must load alongside the vendored rules"
            );
        }
    }

    #[test]
    fn vendored_rules_fire_for_representative_providers() {
        let compiled = patterns(CredentialPatternProfile::Observation);

        let aws = rule(&compiled, "aws-access-token");
        assert!(aws.is_match("aws_key = AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(aws.kind(), CredentialPatternKind::KnownCredential);

        let github = rule(&compiled, "github-pat");
        assert!(github.is_match("ghp_KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ"));
        assert_eq!(github.kind(), CredentialPatternKind::KnownCredential);

        let private_key = rule(&compiled, "private-key");
        assert_eq!(private_key.kind(), CredentialPatternKind::PrivateKey);
    }

    /// Upstream's context rules match `keyword = <secret>`, so they are
    /// assignments, and a finding must say so rather than claim an exact
    /// credential it never identified.
    #[test]
    fn vendored_context_rules_are_attributed_as_assignments() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let generic = rule(&compiled, "generic-api-key");
        assert_eq!(generic.kind(), CredentialPatternKind::CredentialAssignment);
        assert!(generic.is_match(r#"let auth = "Zx9Kq2Lm7Pv4Ns8Rt3Wy6Bd1";"#));
    }

    /// The rule's own entropy floor, scored by our kernel.
    #[test]
    fn vendored_entropy_floor_rejects_structureless_values() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let generic = rule(&compiled, "generic-api-key");
        assert!(!generic.is_match(r#"let auth = "aaaaaaaaaaaaaaaa";"#));
    }

    /// Upstream stopwords are what keep the generic rule from redacting prose.
    #[test]
    fn vendored_allowlists_excuse_upstream_false_positives() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let generic = rule(&compiled, "generic-api-key");
        assert!(!generic.is_match(r#"let auth = "Zx9Kq2Lm7swagger4Ns8Rt3Wy6";"#));
    }

    #[test]
    fn supplement_rules_still_fire() {
        let compiled = patterns(CredentialPatternProfile::Observation);

        let openai = rule(&compiled, "tracedecay-openai-family-key");
        assert!(openai.is_match("api_key=sk-lcm-canonical-detector-1234567890abcdef"));
        assert!(openai.is_match("use sk-test-742913 for dry runs"));
        assert_eq!(openai.kind(), CredentialPatternKind::KnownCredential);

        let bearer = rule(&compiled, "tracedecay-bearer-token-observation");
        assert!(bearer.is_match("Authorization: Bearer abcdef123456"));

        // Truncated PEM: upstream `private-key` needs the closing armour.
        let truncated = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA";
        let block = rule(&compiled, "tracedecay-private-key-block");
        assert_eq!(block.ranges(truncated), vec![0..truncated.len()]);
        assert!(!rule(&compiled, "private-key").is_match(truncated));
    }

    #[test]
    fn supplement_rules_are_profile_scoped() {
        let observation = patterns(CredentialPatternProfile::Observation);
        let memory = patterns(CredentialPatternProfile::Memory);

        assert!(
            observation
                .iter()
                .any(|pattern| pattern.id() == "tracedecay-credential-assignment-observation")
        );
        assert!(
            !observation
                .iter()
                .any(|pattern| pattern.id() == "tracedecay-credential-assignment-memory")
        );
        assert!(
            memory
                .iter()
                .any(|pattern| pattern.id() == "tracedecay-credential-assignment-memory")
        );
        assert!(
            !memory
                .iter()
                .any(|pattern| pattern.id() == "tracedecay-private-key-block")
        );
    }

    #[test]
    fn assignment_patterns_include_bounded_quoted_punctuation() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let assignment = rule(&compiled, "tracedecay-credential-assignment-observation");

        assert!(assignment.is_match(r#"password = "p@ssw0rd!""#));
        assert!(assignment.is_match("password = p@ssw0rd!"));
        assert!(assignment.is_match(r#"passphrase = "p@ssw0rd!""#));
        assert!(assignment.is_match("password = \"truncated!"));

        let escaped_quote = r#"password = "abcdef\"tailsecret""#;
        assert_eq!(
            assignment.ranges(escaped_quote),
            vec![0..escaped_quote.len()]
        );
        let truncated = "password = \"truncated!";
        assert_eq!(assignment.ranges(truncated), vec![0..truncated.len()]);
    }

    #[test]
    fn source_declarations_cover_canonical_sensitive_key_suffixes() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let assignment = rule(
            &compiled,
            "tracedecay-sensitive-source-assignment-observation",
        );

        for source in [
            r#"const vault_passphrase = "p@ssw0rd!""#,
            r#"static DB_PASSPHRASE: &str = "p@ssw0rd!""#,
            r#"String dbPassphrase = "p@ssw0rd!""#,
            r#"const session_token = "p@ssw0rd!""#,
            r#"let clientApiKey = "p@ssw0rd!""#,
            r#"config.vault_passphrase = "p@ssw0rd!""#,
            r#"self.session_token = "p@ssw0rd!""#,
            r#"const char *db_password = "p@ssw0rd!";"#,
            r#"var dbPassword string = "p@ssw0rd!";"#,
            r#"db_password: str = "p@ssw0rd!";"#,
            r#"db_password = "p@ssw0rd!";"#,
            "let db_password =\n    \"p@ssw0rd!\";",
            r#"const settings = { vault_passphrase: "p@ssw0rd!" };"#,
            "const settings = { vault_passphrase:\n  \"p@ssw0rd!\" };",
            "const settings = {\n  vault_passphrase: \"p@ssw0rd!\"\n};",
            "const settings = {\n  \"vault_passphrase\": \"p@ssw0rd!\"\n};",
        ] {
            assert!(
                assignment.is_match(source),
                "missing source assignment: {source}"
            );
        }

        for ordinary in [
            r#"const char *db_password_hint = "ordinary";"#,
            r#"var dbPasswordHint string = "ordinary";"#,
            r#"db_password_hint: str = "ordinary";"#,
            r#"db_password_hint = "ordinary";"#,
            "if db_password == candidate {}",
            "db_password => expression",
        ] {
            assert!(
                !assignment.is_match(ordinary),
                "over-redacted ordinary identifier: {ordinary}"
            );
        }

        let short = r#"const vault_passphrase = "abc";"#;
        assert_eq!(assignment.ranges(short), vec![0..short.len() - 1]);

        let raw = r##"const vault_passphrase = r#"p@ssw0rd!"#;"##;
        assert_eq!(assignment.ranges(raw), vec![0..raw.len() - 1]);

        let wrapped = r#"const vault_passphrase = Some("p@ssw0rd!");"#;
        assert_eq!(assignment.ranges(wrapped), vec![0..wrapped.len()]);
    }

    /// A ruleset that fails to load must say so. The one outcome a privacy
    /// boundary cannot have is a detector that quietly holds no rules.
    #[test]
    fn rule_document_failures_are_typed_and_never_empty() {
        let malformed = compile_document(
            "fixture",
            "[[rules]\nid = 'x'",
            RuleOrigin::Vendored,
            CredentialPatternProfile::Observation,
        );
        assert!(matches!(
            malformed,
            Err(CredentialRuleSetError::Document {
                document: "fixture",
                ..
            })
        ));

        let empty = compile_document(
            "fixture",
            "title = 'no rules here'\n",
            RuleOrigin::Vendored,
            CredentialPatternProfile::Observation,
        );
        assert!(matches!(
            empty,
            Err(CredentialRuleSetError::Empty {
                document: "fixture"
            })
        ));

        // A document of nothing but path-selected rules yields no usable rule,
        // and that is reported as empty rather than accepted as a ruleset.
        let path_only = compile_document(
            "fixture",
            "[[rules]]\nid = 'path-only'\npath = '''\\.php$'''\n",
            RuleOrigin::Vendored,
            CredentialPatternProfile::Observation,
        );
        assert!(matches!(
            path_only,
            Err(CredentialRuleSetError::Empty {
                document: "fixture"
            })
        ));

        let bad_regex = compile_document(
            "fixture",
            "[[rules]]\nid = 'broken'\nregex = '''('''\n",
            RuleOrigin::Vendored,
            CredentialPatternProfile::Observation,
        );
        assert!(matches!(
            bad_regex,
            Err(CredentialRuleSetError::Regex { document: "fixture", rule_id }) if rule_id == "broken"
        ));

        let unlabelled_supplement = compile_document(
            "fixture",
            "[[rules]]\nid = 'local'\nregex = '''abc'''\n",
            RuleOrigin::Supplement,
            CredentialPatternProfile::Observation,
        );
        assert!(matches!(
            unlabelled_supplement,
            Err(CredentialRuleSetError::Rule { rule_id, .. }) if rule_id == "local"
        ));
    }

    #[test]
    fn entropy_thresholds_saturate_rather_than_wrap() {
        assert_eq!(entropy_threshold_per_mille(3.5), 3_500);
        assert_eq!(entropy_threshold_per_mille(-1.0), 0);
        assert_eq!(entropy_threshold_per_mille(f64::NAN), 0);
        assert_eq!(entropy_threshold_per_mille(f64::MAX), u32::MAX);
    }

    #[test]
    fn allowlist_line_target_reads_the_matching_line() {
        assert_eq!(line_containing("alpha\nbeta\ngamma", 6), "beta");
        assert_eq!(line_containing("alpha", 0), "alpha");
        assert_eq!(line_containing("alpha\nbeta", 9), "beta");
    }
}
