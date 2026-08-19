//! Credential rules: a vendored community catalogue plus `TraceDecay`'s local
//! supplement.
//!
//! `TraceDecay` used to carry its own short list of provider token shapes. Seven
//! alternations maintained by hand is not a credible answer to "what does a
//! leaked key look like", so the catalogue is now vendored from gitleaks (MIT)
//! and this module compiles it. See
//! `rules/vendor/gitleaks/PROVENANCE.md` for the source commit, the licence,
//! the deviations, and the refresh procedure; `rules/supplement.toml` for the
//! handful of rules upstream has no equivalent for.
//!
//! What is *not* vendored is the engine. The bounded scan, the parse-before-scan
//! structured layer, the entropy kernel, the typed findings and assessments, and
//! the redaction merge all stay `TraceDecay`'s. Vendored rules are data feeding
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

use std::borrow::Cow;
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

/// The exact rule-document bytes the detector compiles, in evaluation order.
///
/// Revision-sensitive consumers (the at-rest privacy rescan watermark) bind to
/// this data rather than to the pinned sanitizer contract string, because a
/// vendored-catalogue or supplement refresh changes what the detector finds
/// without changing the receipt contract.
pub(crate) const fn rule_document_bytes() -> [&'static [u8]; 2] {
    [
        VENDORED_RULES_TOML.as_bytes(),
        SUPPLEMENT_RULES_TOML.as_bytes(),
    ]
}

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

/// Stands in for a rule id when a document-level allowlist fails to compile.
const DOCUMENT_ALLOWLIST_ID: &str = "<document allowlist>";

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
    /// Upstream `keywords`, lowercased. Empty means the rule always runs.
    ///
    /// These are not an optimisation. Gitleaks only evaluates a rule when one
    /// of its keywords is present, and several rules are written assuming that
    /// gate: `sourcegraph-access-token` accepts a bare 40-character hex string,
    /// which is also every git SHA in existence, and is only safe because it
    /// never runs unless "sourcegraph" or "sgp_" is nearby. Dropping the gate
    /// would not merely cost time, it would redact every commit id we observe.
    keywords: Vec<String>,
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
        // Keyword gate first, exactly as upstream orders it: it is both the
        // rule's precondition and the cheapest possible reject, which is what
        // keeps a 200-rule catalogue affordable inline at ingest.
        if !self.keywords_present(text) || !self.regex.is_match(text) {
            return false;
        }
        !self.ranges(text).is_empty()
    }

    fn keywords_present(&self, text: &str) -> bool {
        self.keywords.is_empty()
            || self
                .keywords
                .iter()
                .any(|keyword| contains_ignore_ascii_case(text, keyword))
    }

    /// Byte ranges to redact. The whole match, not just the secret group: a
    /// sanitizer must not leave secret bytes behind, and the context a rule
    /// matched on is itself worth removing.
    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        // Same keyword gate as `is_match`, and for the same reason: some
        // rules (`sourcegraph-access-token` among them) accept a bare match
        // that is only safe when their keyword precondition holds.
        // `redact_text` calls `ranges` directly, bypassing `is_match`, so the
        // gate has to live here too or a keyword-gated rule fires unguarded
        // on every caller that doesn't separately check `is_match` first.
        if !self.keywords_present(text) {
            return Vec::new();
        }
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

#[derive(Clone)]
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
        // `regexTarget` selects what the *regexes* read. Stopwords always read
        // the secret, upstream included — pointing them at the match would let
        // the keyword that triggered the rule excuse it, so `auth = <secret>`
        // would be waved through by the stopword "auth".
        let regex_target = match self.target {
            AllowlistTarget::Secret => secret.as_str(),
            AllowlistTarget::Match => whole.as_str(),
            AllowlistTarget::Line => line_containing(text, whole.start()),
        };
        let regex_hit = self
            .regexes
            .iter()
            .any(|regex| regex.is_match(regex_target));
        let stopword_hit = self
            .stopwords
            .iter()
            .any(|stopword| contains_ignore_ascii_case(secret.as_str(), stopword));
        if self.all_of {
            (self.regexes.is_empty() || regex_hit) && (self.stopwords.is_empty() || stopword_hit)
        } else {
            regex_hit || stopword_hit
        }
    }
}

/// ASCII-case-insensitive substring test, allocation-free.
///
/// Keywords and stopwords are lowercased at load, and both are matched against
/// text we must not copy on every rule for every value we scan.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
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
    /// `TraceDecay` schema: kind and profiles are mandatory, because they are the
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

    // Document-level allowlists apply to every rule, so they are compiled once
    // and attributed to the document rather than to whichever rule happened to
    // be first. An error naming `1password-secret-key` for a global regex would
    // send the next reader to the wrong line.
    let mut shared_allowlists = Vec::new();
    for allowlist in parsed.allowlist.iter().chain(parsed.allowlists.iter()) {
        if let Some(compiled) = compile_allowlist(document, DOCUMENT_ALLOWLIST_ID, allowlist)? {
            shared_allowlists.push(compiled);
        }
    }

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

        let regex = compile_regex(document, &rule.id, source_regex)?;

        let mut allowlists = Vec::new();
        for allowlist in rule.allowlist.iter().chain(rule.allowlists.iter()) {
            if let Some(compiled) = compile_allowlist(document, &rule.id, allowlist)? {
                allowlists.push(compiled);
            }
        }
        allowlists.extend(shared_allowlists.iter().cloned());

        patterns.push(CredentialPattern {
            kind,
            regex,
            assignment_min_len: rule.assignment_min_len,
            secret_group: rule.secret_group,
            min_entropy_per_mille: rule.entropy.map(entropy_threshold_per_mille),
            allowlists,
            keywords: rule
                .keywords
                .iter()
                .map(|keyword| keyword.to_ascii_lowercase())
                .collect(),
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
        regexes.push(compile_regex(document, rule_id, source_regex)?);
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

fn compile_regex(
    document: &'static str,
    rule_id: &str,
    source_regex: &str,
) -> Result<Regex, CredentialRuleSetError> {
    Regex::new(&re2_compatible_regex(source_regex)).map_err(|_| CredentialRuleSetError::Regex {
        document,
        rule_id: rule_id.to_string(),
    })
}

/// Rewrites an upstream regex into the Rust `regex` crate's dialect without
/// changing what it means.
///
/// Gitleaks rules are authored for Go's RE2. RE2 and Rust's `regex` share the
/// important restrictions — no backreferences, no lookaround — which is why the
/// catalogue transfers at all. They disagree in exactly two places, and both
/// are mechanical:
///
/// * **A literal `{`.** RE2 reads a brace that opens no valid repetition as a
///   literal; Rust refuses it. Upstream depends on the RE2 reading — the global
///   allowlist matches shell placeholders with `^\$(?:\d+|{\d+})$`. Those braces
///   are escaped; real quantifiers (`{16}`, `{0,50}`, `{20,}`) are untouched.
/// * **`\w`.** In RE2 it is exactly `[0-9A-Za-z_]`. In Rust it is Unicode-aware,
///   so it is *both* a different match and vastly larger to compile: three
///   upstream rules that repeat `\w` over a wide bound
///   (`pypi-...[\w-]{50,1000}`) blow past the compiler's 10 MB program limit.
///   Expanding `\w` to its RE2 meaning fixes the semantics and the size at once
///   — every rule in the catalogue then compiles under the default limit, with
///   no memory headroom bought and no rule dropped.
///
/// `\W`, `\D` and `\S` would need the same treatment but appear nowhere in the
/// catalogue; a refresh that introduces one is caught by the compile test,
/// which names the rule.
///
/// Character classes are tracked because `\w` expands differently inside one:
/// `[\w-]` has to become `[0-9A-Za-z_-]`, never a nested class.
fn re2_compatible_regex(pattern: &str) -> Cow<'_, str> {
    if !pattern.contains('{') && !pattern.contains(r"\w") {
        return Cow::Borrowed(pattern);
    }
    let bytes = pattern.as_bytes();
    let mut rewritten = String::with_capacity(pattern.len() + 16);
    let mut index = 0;
    let mut in_class = false;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'w' {
                rewritten.push_str(if in_class {
                    "0-9A-Za-z_"
                } else {
                    "[0-9A-Za-z_]"
                });
                index += 2;
                continue;
            }
            // Any other escape pair is copied whole: its second byte is never
            // structural, so it must not be re-examined.
            let end = next_boundary(pattern, index + 1);
            rewritten.push_str(&pattern[index..end]);
            index = end;
            continue;
        }
        match bytes[index] {
            b'[' if !in_class => {
                in_class = true;
                rewritten.push('[');
                index += 1;
                if bytes.get(index) == Some(&b'^') {
                    rewritten.push('^');
                    index += 1;
                }
                // A `]` in first position is a literal member, not the closer.
                if bytes.get(index) == Some(&b']') {
                    rewritten.push_str("\\]");
                    index += 1;
                }
            }
            b']' if in_class => {
                in_class = false;
                rewritten.push(']');
                index += 1;
            }
            b'{' if !in_class && repetition_len(&bytes[index..]).is_none() => {
                rewritten.push_str("\\{");
                index += 1;
            }
            _ => {
                let end = next_boundary(pattern, index);
                rewritten.push_str(&pattern[index..end]);
                index = end;
            }
        }
    }
    Cow::Owned(rewritten)
}

fn next_boundary(text: &str, index: usize) -> usize {
    let mut end = index + 1;
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    end.min(text.len())
}

/// Length of a valid `{n}`, `{n,}` or `{n,m}` at the head of `rest`, which
/// begins with `{`. `None` means the brace is a literal.
fn repetition_len(rest: &[u8]) -> Option<usize> {
    let mut index = 1;
    let digits = rest[index..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    index += digits;
    if rest.get(index) == Some(&b',') {
        index += 1;
        index += rest[index..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    }
    (rest.get(index) == Some(&b'}')).then_some(index + 1)
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
    keywords: Vec<String>,
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

        if assignment_uses_colon(&matched)
            && is_obvious_rust_non_secret_value(text, matched.start(), value_start, line_end)
        {
            return None;
        }

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
                            .find(|next| !matches!(**next, b' ' | b'\t'))
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

/// A bare `key: value` prefix also appears in Rust field and parameter syntax.
/// Only reject a value when it is plainly a type or a call expression without a
/// literal: neither form carries credential bytes in the indexed source.
fn assignment_uses_colon(matched: &Match<'_>) -> bool {
    matched.as_str().trim_end().ends_with(':')
}

fn is_obvious_rust_non_secret_value(
    text: &str,
    assignment_start: usize,
    value_start: usize,
    line_end: usize,
) -> bool {
    let value = &text[value_start..line_end];
    (has_rust_declaration_context(text, assignment_start) && looks_like_rust_type_annotation(value))
        || looks_like_non_literal_call_expression(value)
}

fn has_rust_declaration_context(text: &str, assignment_start: usize) -> bool {
    let line_start = text[..assignment_start]
        .rfind(['\r', '\n'])
        .map_or(0, |newline| newline + 1);
    let prefix = text[line_start..assignment_start].trim_end();
    prefix.ends_with("pub")
        || prefix.starts_with("pub(")
        || prefix.ends_with(['(', '{', ','])
        || prefix.ends_with("let")
        || prefix.ends_with("let mut")
}

fn looks_like_rust_type_annotation(value: &str) -> bool {
    let Some((candidate, terminator)) = value
        .char_indices()
        .find(|(_, character)| matches!(character, ',' | ')' | '=' | ';'))
        .map(|(index, terminator)| (&value[..index], terminator))
    else {
        return false;
    };
    if !matches!(terminator, ',' | ')' | '=') {
        return false;
    }

    let candidate = candidate.trim();
    if candidate.is_empty()
        || !candidate.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '_' | '&' | '*' | ':' | '<' | '>' | '[' | ']' | '\'' | ' ' | '\t'
                )
        })
    {
        return false;
    }

    let type_name = candidate
        .trim_start_matches(['&', '*', '\'', ' ', '\t'])
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|name| !name.is_empty());
    type_name.is_some_and(|name| {
        matches!(
            name,
            "String"
                | "str"
                | "bool"
                | "char"
                | "usize"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "f32"
                | "f64"
                | "Vec"
                | "Option"
                | "Result"
        ) || name.chars().next().is_some_and(char::is_uppercase)
    })
}

fn looks_like_non_literal_call_expression(value: &str) -> bool {
    let value = value.trim().trim_end_matches([';', ',', '}']).trim_end();
    if !value.contains('(')
        || !value.ends_with(')')
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    '_' | ':' | '.' | '(' | ')' | '&' | '*' | '<' | '>' | ' ' | '\t'
                )
        })
    {
        return false;
    }

    let mut depth = 0usize;
    for character in value.chars() {
        match character {
            '(' => depth += 1,
            ')' => {
                let Some(next_depth) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next_depth;
            }
            _ => {}
        }
    }
    depth == 0
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
        assert!(aws.is_match("aws_key = AKIA4S27TQXBVCZ5MJ6L"));
        // Upstream deliberately excuses AWS's own documented example key, and
        // that allowlist has to keep working or the catalogue is not loaded.
        assert!(!aws.is_match("aws_key = AKIAIOSFODNN7EXAMPLE"));
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

    /// The RE2 translation must preserve real quantifiers, rescue the literal
    /// braces upstream's placeholders are written with, and give `\w` its RE2
    /// meaning both inside and outside a character class.
    #[test]
    fn re2_translation_preserves_meaning() {
        for untouched in [r"[A-Z]{16}", r"sk-[A-Za-z0-9_-]{20,}", r"\bplain\b"] {
            assert_eq!(re2_compatible_regex(untouched), untouched);
        }

        assert_eq!(
            re2_compatible_regex(r"^\$(?:\d+|{\d+})$"),
            r"^\$(?:\d+|\{\d+})$"
        );
        // An already-escaped brace must not be escaped twice.
        assert_eq!(re2_compatible_regex(r"\{literal}"), r"\{literal}");

        // `\w` expands to a class outside one, and to bare members inside one.
        assert_eq!(re2_compatible_regex(r"\w+"), "[0-9A-Za-z_]+");
        assert_eq!(
            re2_compatible_regex(r"[\w.-]{0,50}?"),
            "[0-9A-Za-z_.-]{0,50}?"
        );
        assert_eq!(re2_compatible_regex(r"[^\w]"), "[^0-9A-Za-z_]");

        assert!(Regex::new(&re2_compatible_regex(r"^\$(?:\d+|{\d+})$")).is_ok());
    }

    /// The size limit this avoids is not hypothetical: Rust's Unicode-aware
    /// `\w` repeated over a wide bound is what pushed three upstream rules past
    /// the 10 MB program limit before the translation gave `\w` its RE2 meaning.
    #[test]
    fn wide_word_repetitions_compile_within_the_default_program_limit() {
        let upstream = r"pypi-AgEIcHlwaS5vcmc[\w-]{50,1000}";
        assert!(Regex::new(upstream).is_err());
        assert!(Regex::new(&re2_compatible_regex(upstream)).is_ok());
    }

    /// Upstream keywords are a precondition, not a hint. `sourcegraph-access-token`
    /// accepts a bare 40-character hex string — which is every git SHA — and is
    /// only safe because it never runs without its keyword nearby.
    #[test]
    fn keywords_gate_rules_that_would_otherwise_match_digests() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let sourcegraph = rule(&compiled, "sourcegraph-access-token");

        assert!(!sourcegraph.is_match("commit 3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5"));
        assert!(sourcegraph.is_match("sourcegraph token 3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5"));
    }

    /// `redact_text` calls `ranges` directly, never `is_match`, so the
    /// keyword gate has to be enforced inside `ranges` itself — not just in
    /// `is_match` — or a bare git SHA gets redacted (and, via
    /// `redact_sensitive_values`, can quarantine whole records keyed by
    /// commit ids) without its keyword precondition ever holding.
    #[test]
    fn keywords_gate_ranges_directly_not_only_is_match() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let sourcegraph = rule(&compiled, "sourcegraph-access-token");

        assert!(
            sourcegraph
                .ranges("commit 3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5")
                .is_empty()
        );
        assert!(
            !sourcegraph
                .ranges("sourcegraph token 3bc562b8a1f0d9e7c6b5a4d3e2f1a0b9c8d7e6f5")
                .is_empty()
        );
    }

    /// `regexTarget` steers the allowlist regexes only. A stopword read from the
    /// match instead of the secret lets the very keyword that triggered a rule
    /// excuse it — "auth" is both this rule's trigger and one of its stopwords.
    #[test]
    fn allowlist_stopwords_read_the_secret_not_the_match() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let generic = rule(&compiled, "generic-api-key");

        assert!(generic.is_match(r#"let auth = "Zx9Kq2Lm7Pv4Ns8Rt3Wy6Bd1";"#));
        // A stopword inside the secret still excuses it.
        assert!(!generic.is_match(r#"let auth = "Zx9Kq2Lm7swagger4Ns8Rt3Wy6";"#));
    }

    #[test]
    fn allowlist_line_target_reads_the_matching_line() {
        assert_eq!(line_containing("alpha\nbeta\ngamma", 6), "beta");
        assert_eq!(line_containing("alpha", 0), "alpha");
        assert_eq!(line_containing("alpha\nbeta", 9), "beta");
    }

    #[test]
    fn credential_assignments_do_not_claim_rust_type_annotations_or_expressions() {
        let compiled = patterns(CredentialPatternProfile::Observation);
        let assignment = rule(&compiled, "tracedecay-credential-assignment-observation");

        for ordinary_source in [
            "pub token: String,",
            "fn authenticate(token: String) {}",
            "Config { token: String::new() }",
        ] {
            assert!(
                assignment.ranges(ordinary_source).is_empty(),
                "ordinary Rust source must not be redacted: {ordinary_source}"
            );
        }

        assert!(assignment.is_match(r#"token: "actual-secret""#));
        assert!(assignment.is_match("token: actual-secret"));
        assert!(assignment.is_match(r#"token: Some("actual-secret")"#));
        assert!(assignment.is_match("token: ActualSecret,"));
    }
}
