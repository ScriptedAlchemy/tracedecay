// Rust guideline compliant 2026-05-25
//! AST-level functional duplicate detection (issue #83).
//!
//! Computes four kinds of fingerprint per function/method body:
//!
//! 1. **AST shape hash** — kind-only pre-order walk of the tree-sitter
//!    subtree, normalised over identifier names. Catches the
//!    `ast_isomorphic` duplicate bucket.
//! 2. **CFG hash** — same walk filtered to control-flow node kinds
//!    (`if`, `for`, `while`, `loop`, `switch`/`match`, `return`/`break`).
//!    Catches reorder-refactor duplicates whose statement order differs.
//! 3. **Call-sequence hash** — ordered list of called identifiers extracted
//!    from call/invocation nodes. Catches "rewrote it from scratch and
//!    didn't notice the helper existed" duplicates.
//! 4. **Token shingles** — set of 32-bit hashes of 5-grams of alphanumeric
//!    tokens within the body. Jaccard similarity over this set catches
//!    the long tail of near-duplicates.
//!
//! These four signals are blended into a composite similarity score and
//! bucketed into `definite` / `likely` / `naming_only` severities.
//!
//! Language-agnostic by design: every signal is derived from raw
//! tree-sitter kind strings, so the same code path works for every
//! grammar the project supports. Two duplicates can only match within the
//! same language (tree-sitter kind names don't align across grammars),
//! which matches user expectations.

use std::collections::HashSet;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser, Tree};

/// Length of an n-gram shingle, in tokens.
const SHINGLE_N: usize = 5;

/// Composite-score weights. The weights must sum to 1.0.
const W_AST: f64 = 0.40;
const W_CFG: f64 = 0.25;
const W_CALL_SEQ: f64 = 0.20;
const W_SHINGLE: f64 = 0.15;

/// Per-symbol fingerprint produced by [`compute_fingerprint`].
#[derive(Debug, Clone)]
pub struct Fingerprint {
    pub ast_hash: String,
    pub cfg_hash: String,
    pub call_seq_hash: String,
    /// Sorted, dedup'd set of u32 shingle hashes (rendered as comma-
    /// separated lowercase hex to keep the wire format text-friendly).
    pub shingles: Vec<u32>,
    /// Approximate body size in alphanumeric tokens. Used to bucket
    /// candidates before pairwise comparison so we stay sub-quadratic.
    pub body_tokens: usize,
    /// Hash of the body source. Used to detect when a cached fingerprint
    /// is stale relative to the current file content.
    pub source_hash: String,
}

/// Full scoring verdict for one candidate pair.
///
/// `ranking_score` orders results (composite blended with the discounted
/// cosine, generic helpers downranked); `severity` is derived from the raw
/// signals only — the generic-helper downrank never changes severity.
#[derive(Debug, Clone, PartialEq)]
pub struct RedundancyMatchScore {
    pub similarity: f64,
    pub ranking_score: f64,
    pub vector_cosine: f64,
    pub shingle_jaccard: f64,
    pub overlap_kind: &'static str,
    pub severity: &'static str,
    pub generic_helper_downranked: bool,
}

impl Fingerprint {
    /// Render the shingles vector as a comma-separated lowercase hex
    /// string (suitable for storage in a TEXT column).
    pub(crate) fn shingles_to_string(&self) -> String {
        let mut s = String::with_capacity(self.shingles.len() * 9);
        for (i, h) in self.shingles.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            // Use std fmt; not perf-critical, called once per persist.
            let _ = write!(s, "{h:08x}");
        }
        s
    }

    /// Parse a comma-separated lowercase hex string back into a shingles
    /// vector. Best-effort: unparseable entries are skipped.
    pub(crate) fn shingles_from_string(s: &str) -> Vec<u32> {
        if s.is_empty() {
            return Vec::new();
        }
        s.split(',')
            .filter_map(|hex| u32::from_str_radix(hex, 16).ok())
            .collect()
    }
}

/// Compute every fingerprint signal for a single function body.
///
/// `full_source` is the entire file contents (tree-sitter needs context
/// outside the body to parse correctly); `body_node` is the function's
/// AST subtree.
pub fn compute_fingerprint(full_source: &str, body_node: Node<'_>) -> Fingerprint {
    let body_text = body_node
        .utf8_text(full_source.as_bytes())
        .unwrap_or_default();
    let body_tokens = tokenize(body_text);

    Fingerprint {
        ast_hash: hash_kind_walk(body_node, false),
        cfg_hash: hash_kind_walk(body_node, true),
        call_seq_hash: hash_call_sequence(body_node, full_source.as_bytes()),
        shingles: compute_shingles(&body_tokens),
        body_tokens: body_tokens.len(),
        source_hash: short_sha256(body_text),
    }
}

/// Parse a source file with the given tree-sitter language and return the
/// `Tree`. Returns `None` when parsing fails (malformed input, missing
/// grammar). Builds a fresh `Parser` per call — the call site for
/// fingerprint computation invokes this once per file, not per node.
pub fn parse_file(source: &str, language: &tree_sitter::Language) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    parser.parse(source, None)
}

/// Locate a child node within `tree` that overlaps the given 0-indexed
/// line range. Used to map a `Node` row (with its `start_line` /
/// `end_line`) back to a tree-sitter node after re-parsing.
pub fn find_node_at_lines<'tree>(
    tree: &'tree Tree,
    start_line_zero_indexed: u32,
    end_line_zero_indexed: u32,
) -> Option<Node<'tree>> {
    let root = tree.root_node();
    let mut best: Option<Node<'tree>> = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let ns = node.start_position().row as u32;
        let ne = node.end_position().row as u32;
        if ns <= start_line_zero_indexed && ne >= end_line_zero_indexed {
            // Prefer the deepest enclosing match (most specific).
            if let Some(b) = best {
                let b_span = b.end_position().row - b.start_position().row;
                let n_span = ne - ns;
                if n_span < u32::try_from(b_span).unwrap_or(u32::MAX) {
                    best = Some(node);
                }
            } else {
                best = Some(node);
            }
            // Continue descending only into matching children.
            let mut cursor = node.walk();
            if cursor.goto_first_child() {
                loop {
                    stack.push(cursor.node());
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Tokenisation
// ---------------------------------------------------------------------------

/// Split body text into alphanumeric runs (a–z, A–Z, 0–9, underscore).
/// Whitespace and punctuation are skipped. Numbers are kept as their
/// literal text so `1` and `2` are different tokens (helps shingles).
fn tokenize(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut tokens: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' {
            let start = i;
            while i < bytes.len() {
                let bb = bytes[i];
                if bb.is_ascii_alphanumeric() || bb == b'_' {
                    i += 1;
                } else {
                    break;
                }
            }
            tokens.push(&body[start..i]);
        } else {
            i += 1;
        }
    }
    tokens
}

// ---------------------------------------------------------------------------
// AST / CFG fingerprints
// ---------------------------------------------------------------------------

/// Pre-order kind walk. If `control_flow_only`, emit only the kinds whose
/// names look like control-flow constructs.
fn hash_kind_walk(root: Node<'_>, control_flow_only: bool) -> String {
    let mut hasher = Sha256::new();
    let mut stack: Vec<(Node<'_>, u32)> = vec![(root, 0)];
    while let Some((node, depth)) = stack.pop() {
        let kind = node.kind();
        let emit = if control_flow_only {
            is_control_flow_kind(kind)
        } else {
            true
        };
        if emit {
            // Encode depth so structural reshapes don't collide. Using a
            // separator byte (0x1f, unit separator) keeps the
            // serialisation unambiguous.
            hasher.update(kind.as_bytes());
            hasher.update([0x1f]);
            hasher.update(depth.to_le_bytes());
            hasher.update([0x1e]);
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            let mut children: Vec<Node<'_>> = Vec::new();
            loop {
                children.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            // Reverse-push so pop yields left-to-right order.
            for child in children.into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
    short_hex(hasher.finalize().as_slice())
}

/// Heuristic: a tree-sitter kind name represents control flow if it
/// contains any of the marker substrings below. Language-agnostic — all
/// supported grammars use these strings consistently.
fn is_control_flow_kind(kind: &str) -> bool {
    const MARKERS: [&str; 12] = [
        "if", "for", "while", "loop", "switch", "case", "match", "return", "break", "continue",
        "try", "catch",
    ];
    MARKERS.iter().any(|m| kind.contains(m))
}

// ---------------------------------------------------------------------------
// Call-sequence fingerprint
// ---------------------------------------------------------------------------

/// Pre-order walk, collecting the leftmost identifier of every
/// call/invocation/macro node, in source order, then hashing them.
fn hash_call_sequence(root: Node<'_>, source: &[u8]) -> String {
    let mut calls: Vec<String> = Vec::new();
    let mut stack: Vec<Node<'_>> = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if is_call_kind(kind)
            && let Some(name) = leftmost_callable_name(node, source)
        {
            calls.push(name);
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            let mut children: Vec<Node<'_>> = Vec::new();
            loop {
                children.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }

    let mut hasher = Sha256::new();
    for name in &calls {
        hasher.update(name.as_bytes());
        hasher.update([0x1f]);
    }
    short_hex(hasher.finalize().as_slice())
}

fn is_call_kind(kind: &str) -> bool {
    const MARKERS: [&str; 4] = ["call", "invocation", "macro", "apply"];
    MARKERS.iter().any(|m| kind.contains(m))
}

/// Return the leftmost identifier-like child of a call node, treating
/// `field_expression` / `member_expression` as a chain (returns the
/// rightmost field of the leftmost chain — i.e. the called method).
fn leftmost_callable_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        let kind = child.kind();
        if kind == "identifier"
            || kind == "field_identifier"
            || kind == "property_identifier"
            || kind == "scoped_identifier"
        {
            return child.utf8_text(source).ok().map(str::to_string);
        }
        if kind.contains("field_expression")
            || kind.contains("member_expression")
            || kind.contains("scoped")
        {
            let mut inner = child.walk();
            if inner.goto_first_child() {
                let mut last_id: Option<String> = None;
                loop {
                    let ic = inner.node();
                    let ik = ic.kind();
                    if ik.contains("identifier")
                        && let Ok(t) = ic.utf8_text(source)
                    {
                        last_id = Some(t.to_string());
                    }
                    if !inner.goto_next_sibling() {
                        break;
                    }
                }
                if last_id.is_some() {
                    return last_id;
                }
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Shingles + Jaccard
// ---------------------------------------------------------------------------

/// Build a sorted, deduplicated vector of u32 shingle hashes over the
/// token stream. `n` is the n-gram length (`SHINGLE_N`).
fn compute_shingles(tokens: &[&str]) -> Vec<u32> {
    if tokens.len() < SHINGLE_N {
        return Vec::new();
    }
    let mut set: HashSet<u32> = HashSet::new();
    for window in tokens.windows(SHINGLE_N) {
        let mut hasher = Sha256::new();
        for tok in window {
            hasher.update(tok.as_bytes());
            hasher.update([0x1f]);
        }
        let digest = hasher.finalize();
        // Fold the digest into a u32 by xoring 32-bit chunks.
        let mut acc: u32 = 0;
        for chunk in digest.chunks(4) {
            let mut b = [0u8; 4];
            for (i, v) in chunk.iter().enumerate() {
                b[i] = *v;
            }
            acc ^= u32::from_le_bytes(b);
        }
        set.insert(acc);
    }
    let mut out: Vec<u32> = set.into_iter().collect();
    out.sort_unstable();
    out
}

/// Size of the intersection of two sorted, dedup'd shingle sets.
///
/// Both body-similarity limbs — Jaccard and the body-vector cosine — are pure
/// functions of this one count plus the two set sizes, so the pairwise scan
/// pays the two-pointer merge once per pair instead of once per limb.
fn shingle_intersection(a: &[u32], b: &[u32]) -> usize {
    // Two pointer merge over sorted sequences.
    let (mut i, mut j) = (0usize, 0usize);
    let mut inter = 0usize;
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    inter
}

/// Jaccard similarity from a precomputed intersection count. Returns 1.0
/// for two empty sets (vacuous match — they're both "no content").
fn jaccard_from_intersection(a_len: usize, b_len: usize, intersection: usize) -> f64 {
    if a_len == 0 && b_len == 0 {
        return 1.0;
    }
    if a_len == 0 || b_len == 0 {
        return 0.0;
    }
    let union = a_len + b_len - intersection;
    if union == 0 {
        return 1.0;
    }
    intersection as f64 / union as f64
}

/// Cosine similarity from a precomputed intersection count.
///
/// This is the cheap vector-style body similarity signal used by the
/// redundancy tool for candidate discovery and ranking. Unlike Jaccard, it is
/// less harsh when two larger bodies share a strong core but differ in a few
/// surrounding shingles.
fn cosine_from_intersection(a_len: usize, b_len: usize, intersection: usize) -> f64 {
    if a_len == 0 || b_len == 0 {
        return 0.0;
    }
    intersection as f64 / ((a_len as f64).sqrt() * (b_len as f64).sqrt())
}

/// Jaccard similarity over two sorted/dedup'd shingle sets. Returns 1.0
/// for two empty sets (vacuous match — they're both "no content").
pub(crate) fn jaccard_similarity(a: &[u32], b: &[u32]) -> f64 {
    jaccard_from_intersection(a.len(), b.len(), shingle_intersection(a, b))
}

/// Cosine similarity over sorted/dedup'd shingle vectors. Production callers
/// score a pair through [`redundancy_match_score`], which shares one shingle
/// merge across both similarity limbs; this stays as the limb's direct
/// property test surface.
#[cfg(test)]
pub(crate) fn vector_cosine_similarity(a: &[u32], b: &[u32]) -> f64 {
    cosine_from_intersection(a.len(), b.len(), shingle_intersection(a, b))
}

// ---------------------------------------------------------------------------
// Composite similarity + severity
// ---------------------------------------------------------------------------

/// Blend the four signals into a single \[0,1\] similarity score, taking the
/// shingle Jaccard already computed so a caller scoring a pair pays the merge
/// cost once.
fn composite_similarity_with_jaccard(a: &Fingerprint, b: &Fingerprint, jaccard: f64) -> f64 {
    let ast = if a.ast_hash == b.ast_hash { 1.0 } else { 0.0 };
    let cfg = if a.cfg_hash == b.cfg_hash { 1.0 } else { 0.0 };
    let call = if a.call_seq_hash == b.call_seq_hash {
        1.0
    } else {
        0.0
    };
    W_AST * ast + W_CFG * cfg + W_CALL_SEQ * call + W_SHINGLE * jaccard
}

/// Determine the "kind" of overlap two functions share. Returned alongside
/// the composite score so callers can filter (e.g. drop `naming` matches).
pub fn overlap_kind(a: &Fingerprint, b: &Fingerprint) -> &'static str {
    overlap_kind_with_jaccard(a, b, jaccard_similarity(&a.shingles, &b.shingles))
}

/// [`overlap_kind`] with the shingle Jaccard already computed, so a caller
/// scoring a pair pays the merge cost once.
fn overlap_kind_with_jaccard(a: &Fingerprint, b: &Fingerprint, jaccard: f64) -> &'static str {
    if a.ast_hash == b.ast_hash {
        "ast_isomorphic"
    } else if a.cfg_hash == b.cfg_hash {
        "control_flow"
    } else if a.call_seq_hash == b.call_seq_hash {
        "algorithmic"
    } else if jaccard >= 0.5 {
        "token_overlap"
    } else {
        "naming"
    }
}

/// Minimum score for a non-AST match to be bucketed `likely`. Shared with the
/// `naming` -> `body_vector` relabel in [`redundancy_match_score`] so a pair
/// can never carry the `body_vector` kind with a `naming_only` severity.
pub(crate) const LIKELY_SEVERITY_FLOOR: f64 = 0.55;

/// Severity bucket for a `(score, overlap_kind)` pair.
///
/// `definite` requires AST isomorphism — anything less can still be a
/// false positive. `likely` covers control-flow or algorithmic matches
/// with high shingle overlap. `naming_only` is the long tail.
pub(crate) fn severity_bucket(score: f64, kind: &str) -> &'static str {
    if kind == "ast_isomorphic" && score >= 0.80 {
        "definite"
    } else if kind == "naming" {
        "naming_only"
    } else if score >= LIKELY_SEVERITY_FLOOR {
        "likely"
    } else {
        "naming_only"
    }
}

/// Score a candidate pair, or `None` when it should not be reported.
///
/// A pair passes the gate when either the composite similarity or the
/// body-vector cosine clears `threshold`. A `naming` pair whose cosine clears
/// both `threshold` and [`LIKELY_SEVERITY_FLOOR`] is reclassified as
/// `body_vector` (the body evidence, not the name, is what matched); weaker
/// `naming` pairs stay `naming` and honor `include_naming`. Pairs sharing an
/// identical non-generic name are retained as `naming_only` leads even below
/// the gate (see [`same_name_rescue`]).
pub fn redundancy_match_score(
    a_name: &str,
    a: &Fingerprint,
    b_name: &str,
    b: &Fingerprint,
    threshold: f64,
    include_naming: bool,
) -> Option<RedundancyMatchScore> {
    // Bodies below SHINGLE_N tokens have no shingle evidence at all; their
    // ast/cfg/call hashes are near-constant (kinds only, no identifiers), so
    // without token evidence only textually identical bodies are trustworthy.
    if a.shingles.is_empty() && b.shingles.is_empty() && a.source_hash != b.source_hash {
        return None;
    }

    // One shingle merge feeds both body-similarity limbs. Jaccard and the
    // body-vector cosine differ only in their denominator, so computing the
    // intersection twice was the single hottest redundant operation in the
    // pairwise scan; the arithmetic below is the same, in the same order.
    let intersection = shingle_intersection(&a.shingles, &b.shingles);
    let shingle_jaccard =
        jaccard_from_intersection(a.shingles.len(), b.shingles.len(), intersection);
    let similarity = composite_similarity_with_jaccard(a, b, shingle_jaccard);
    let vector_cosine = cosine_from_intersection(a.shingles.len(), b.shingles.len(), intersection);
    if similarity < threshold
        && vector_cosine < threshold
        && !same_name_rescue(a_name, b_name, vector_cosine, include_naming)
    {
        return None;
    }

    let mut overlap_kind = overlap_kind_with_jaccard(a, b, shingle_jaccard);
    if overlap_kind == "naming"
        && vector_cosine >= threshold
        && vector_cosine >= LIKELY_SEVERITY_FLOOR
    {
        overlap_kind = "body_vector";
    }
    if !include_naming && overlap_kind == "naming" {
        return None;
    }

    let generic_helper_downranked = generic_helper_pair(a_name, b_name);
    // The cosine-only signal is trusted slightly less than the composite, so
    // rank it at a 0.95 discount. ranking_score is a rank key, not a
    // thresholded quantity — it can legitimately sit below `threshold`.
    let mut ranking_score = similarity.max(vector_cosine * 0.95);
    if generic_helper_downranked {
        ranking_score *= 0.75;
    }

    Some(RedundancyMatchScore {
        similarity,
        ranking_score,
        vector_cosine,
        shingle_jaccard,
        overlap_kind,
        severity: severity_bucket(similarity.max(vector_cosine), overlap_kind),
        generic_helper_downranked,
    })
}

/// Minimum body-vector cosine for the same-name rescue: identical non-generic
/// names with less shared body than this are treated as coincidence.
const SAME_NAME_COSINE_FLOOR: f64 = 0.3;

/// Identical non-generic names across two bodies with modest vector overlap
/// are real duplicate leads even when both score limbs miss the gate
/// (verified live: `clean_comment` duplicated across extractor modules was
/// invisible at every practical threshold). Rescued pairs keep their natural
/// overlap kind — usually `naming` — and therefore surface only with
/// `include_naming`, making that flag a genuine recall lever.
fn same_name_rescue(a_name: &str, b_name: &str, vector_cosine: f64, include_naming: bool) -> bool {
    include_naming
        && a_name == b_name
        && !is_generic_helper_name(a_name)
        && vector_cosine >= SAME_NAME_COSINE_FLOOR
}

fn generic_helper_pair(a_name: &str, b_name: &str) -> bool {
    a_name == b_name && is_generic_helper_name(a_name)
}

/// Method names whose bodies are structurally near-identical across unrelated
/// types (trait impls and ubiquitous idioms), in Rust and the other indexed
/// languages. Pairs of these are downranked, never dropped, and only the
/// ranking is affected — severity is intentionally left untouched.
fn is_generic_helper_name(name: &str) -> bool {
    matches!(
        name,
        "drop"
            | "fmt"
            | "clone"
            | "default"
            | "new"
            | "from"
            | "into"
            | "as_ref"
            | "as_mut"
            | "eq"
            | "ne"
            | "hash"
            | "cmp"
            | "partial_cmp"
            | "deref"
            | "deref_mut"
            | "index"
            | "next"
            | "len"
            | "is_empty"
            | "to_string"
            | "try_from"
            | "constructor"
            | "toString"
            | "__init__"
            | "__str__"
            | "__repr__"
            | "__eq__"
    )
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn short_hex(bytes: &[u8]) -> String {
    // 16 hex chars = 64 bits of entropy — enough to make a collision
    // between two functions in the same repo astronomically unlikely.
    let mut s = String::with_capacity(16);
    for b in bytes.iter().take(8) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Round a score to 4 decimal places for stable JSON/markdown output.
pub fn round4(value: f64) -> f64 {
    (value * 10000.0).round() / 10000.0
}

fn short_sha256(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    short_hex(h.finalize().as_slice())
}

// ---------------------------------------------------------------------------
// Pairwise redundancy scan
// ---------------------------------------------------------------------------

/// One scored redundant pair: the [`RedundancyMatchScore`] verdict plus
/// borrows of the two graph nodes and their fingerprints. Orientation is
/// canonicalized by [`redundant_pair`] so the same logical pair always
/// presents the same `a`/`b` sides regardless of input order.
pub struct RedundantPair<'a> {
    pub score: RedundancyMatchScore,
    pub node_a: &'a crate::types::Node,
    pub node_b: &'a crate::types::Node,
    pub fp_a: &'a Fingerprint,
    pub fp_b: &'a Fingerprint,
}

/// Scan a set of `(node, fingerprint)` candidates for redundant pairs.
///
/// Candidates are sorted by `body_tokens` (ties broken on node id so the
/// enumeration order never depends on DB row order), then each is compared
/// only against the following candidates whose token count falls inside its
/// ±25 % [`body_token_window`] — a linear window over the sorted slice that
/// keeps the pairwise comparison sub-quadratic. Surviving pairs are ranked by
/// `ranking_score` (a total order: ties fall through similarity, cosine, then
/// names and node ids) and truncated to `max_pairs`.
pub fn find_redundant_pairs<'a>(
    scoped: Vec<(&'a crate::types::Node, &'a Fingerprint)>,
    threshold: f64,
    include_naming: bool,
    max_pairs: usize,
) -> Vec<RedundantPair<'a>> {
    let mut scan = RedundancyPairScan::new(scoped, threshold, include_naming, max_pairs);
    while scan.advance(usize::MAX) {}
    scan.finish()
}

/// The same scan as [`find_redundant_pairs`], resumable in bounded slices.
///
/// The scan is a long, uninterrupted CPU loop: run whole inside an async task
/// it pins a runtime worker for its full duration and starves whatever else
/// that worker was serving. This type exposes the identical enumeration as a
/// cursor so an async caller can yield between slices. Enumeration order,
/// scoring, ranking and truncation are unchanged — the cursor only decides
/// *when* the loop pauses, never which pairs it visits or in what order — so
/// a sliced run and a single-shot run return byte-identical results.
pub struct RedundancyPairScan<'a> {
    scoped: Vec<(&'a crate::types::Node, &'a Fingerprint)>,
    threshold: f64,
    include_naming: bool,
    max_pairs: usize,
    /// Index of the candidate whose window is being scanned.
    outer: usize,
    /// Next partner index inside that window. `0` means "window not started",
    /// which is unambiguous because a live partner index is always `outer + 1`
    /// or greater.
    inner: usize,
    found: Vec<RedundantPair<'a>>,
}

impl<'a> RedundancyPairScan<'a> {
    pub fn new(
        mut scoped: Vec<(&'a crate::types::Node, &'a Fingerprint)>,
        threshold: f64,
        include_naming: bool,
        max_pairs: usize,
    ) -> Self {
        // Sort by body_tokens so the size-window check is a linear scan; break
        // ties on node id so candidate enumeration never depends on DB row order.
        scoped.sort_by(|(na, fa), (nb, fb)| {
            fa.body_tokens
                .cmp(&fb.body_tokens)
                .then_with(|| na.id.cmp(&nb.id))
        });
        Self {
            scoped,
            threshold,
            include_naming,
            max_pairs,
            outer: 0,
            inner: 0,
            found: Vec::new(),
        }
    }

    /// Score at most `budget` further candidate pairs.
    ///
    /// Returns `true` while the scan has more work, `false` once every pair has
    /// been visited. The budget counts scored comparisons rather than
    /// candidates so a cluster of same-sized bodies — where one candidate's
    /// window spans thousands of partners — still pauses on schedule.
    pub fn advance(&mut self, budget: usize) -> bool {
        let mut spent = 0usize;
        while self.outer < self.scoped.len() {
            let (node_a, fp_a) = self.scoped[self.outer];
            let (lo, hi) = body_token_window(fp_a.body_tokens);
            if self.inner == 0 {
                self.inner = self.outer + 1;
            }
            while self.inner < self.scoped.len() {
                let (node_b, fp_b) = self.scoped[self.inner];
                if fp_b.body_tokens > hi {
                    break; // sorted, no need to scan further
                }
                if fp_b.body_tokens >= lo
                    && let Some(pair) = redundant_pair(
                        node_a,
                        fp_a,
                        node_b,
                        fp_b,
                        self.threshold,
                        self.include_naming,
                    )
                {
                    self.found.push(pair);
                }
                self.inner += 1;
                spent += 1;
                if spent >= budget {
                    return true;
                }
            }
            self.outer += 1;
            self.inner = 0;
        }
        false
    }

    /// Rank the collected pairs and truncate to `max_pairs`.
    pub fn finish(self) -> Vec<RedundantPair<'a>> {
        let mut found = self.found;
        found.sort_by(|a: &RedundantPair<'_>, b: &RedundantPair<'_>| {
            b.score
                .ranking_score
                .partial_cmp(&a.score.ranking_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b.score
                        .similarity
                        .partial_cmp(&a.score.similarity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    b.score
                        .vector_cosine
                        .partial_cmp(&a.score.vector_cosine)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.node_a.name.cmp(&b.node_a.name))
                .then_with(|| a.node_b.name.cmp(&b.node_b.name))
                .then_with(|| a.node_a.id.cmp(&b.node_a.id))
                .then_with(|| a.node_b.id.cmp(&b.node_b.id))
        });
        found.truncate(self.max_pairs);
        found
    }
}

/// The ±25 % `body_tokens` window used to bucket candidates before scoring.
/// Returns the inclusive `(low, high)` token bounds for a body of the given
/// size.
pub fn body_token_window(body_tokens: usize) -> (usize, usize) {
    (
        (body_tokens as f64 * 0.75).floor() as usize,
        (body_tokens as f64 * 1.25).ceil() as usize,
    )
}

/// Score one candidate pair, returning a canonically-oriented
/// [`RedundantPair`] or `None` when [`redundancy_match_score`] rejects it.
///
/// Orientation is fixed by `(file_path, start_line, id)` so the same logical
/// pair always presents the same `a`/`b` sides regardless of input order
/// (scoring is symmetric).
pub(crate) fn redundant_pair<'a>(
    node_a: &'a crate::types::Node,
    fp_a: &'a Fingerprint,
    node_b: &'a crate::types::Node,
    fp_b: &'a Fingerprint,
    threshold: f64,
    include_naming: bool,
) -> Option<RedundantPair<'a>> {
    let score = redundancy_match_score(
        &node_a.name,
        fp_a,
        &node_b.name,
        fp_b,
        threshold,
        include_naming,
    )?;
    // Canonicalize orientation so the same logical pair always presents the
    // same a/b sides regardless of DB row order (scoring is symmetric).
    let a_key = (&node_a.file_path, node_a.start_line, &node_a.id);
    let b_key = (&node_b.file_path, node_b.start_line, &node_b.id);
    let (node_a, fp_a, node_b, fp_b) = if a_key <= b_key {
        (node_a, fp_a, node_b, fp_b)
    } else {
        (node_b, fp_b, node_a, fp_a)
    };
    Some(RedundantPair {
        score,
        node_a,
        node_b,
        fp_a,
        fp_b,
    })
}

/// Connected components over the returned pairs — the shared source of truth
/// for both the JSON `groups` array and the markdown Groups section, so the
/// two views cannot drift on membership.
pub fn connected_node_groups<'a>(
    pairs: &'a [RedundantPair<'a>],
) -> Vec<Vec<&'a crate::types::Node>> {
    let mut groups: Vec<Vec<&'a crate::types::Node>> = Vec::new();
    for pair in pairs {
        let mut matching_groups = Vec::new();
        for (idx, group) in groups.iter().enumerate() {
            if group
                .iter()
                .any(|node| node.id == pair.node_a.id || node.id == pair.node_b.id)
            {
                matching_groups.push(idx);
            }
        }

        let nodes = [pair.node_a, pair.node_b];
        if matching_groups.is_empty() {
            groups.push(Vec::from(nodes));
            continue;
        }

        let first = matching_groups[0];
        for node in nodes {
            push_unique_node(&mut groups[first], node);
        }
        for idx in matching_groups.into_iter().skip(1).rev() {
            let merged = groups.remove(idx);
            for node in merged {
                push_unique_node(&mut groups[first], node);
            }
        }
    }

    groups
}

fn push_unique_node<'a>(nodes: &mut Vec<&'a crate::types::Node>, node: &'a crate::types::Node) {
    if nodes.iter().any(|existing| existing.id == node.id) {
        return;
    }
    nodes.push(node);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Helper that parses a Rust snippet and returns the first function body.
    fn fingerprint_for_rust_fn(snippet: &str) -> Fingerprint {
        let lang = tracedecay_code_extraction::ts_provider::language("rust").expect("rust grammar");
        let tree = parse_file(snippet, &lang).expect("parse failed");
        let root = tree.root_node();
        let fn_node = find_first_kind(root, "function_item").expect("no function in snippet");
        compute_fingerprint(snippet, fn_node)
    }

    fn find_first_kind<'t>(root: Node<'t>, target: &str) -> Option<Node<'t>> {
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if n.kind() == target {
                return Some(n);
            }
            let mut cursor = n.walk();
            if cursor.goto_first_child() {
                loop {
                    stack.push(cursor.node());
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        None
    }

    #[test]
    fn identical_functions_have_identical_ast_hash() {
        let a =
            fingerprint_for_rust_fn("fn a(x: i32) -> i32 { if x > 0 { x + 1 } else { x - 1 } }");
        let b =
            fingerprint_for_rust_fn("fn b(y: i32) -> i32 { if y > 0 { y + 1 } else { y - 1 } }");
        assert_eq!(
            a.ast_hash, b.ast_hash,
            "renamed identifiers must not change AST hash"
        );
        // AST + CFG + call-seq all match; shingles diverge because token
        // names changed. Score lower-bound: 0.40+0.25+0.20 = 0.85.
        let score =
            composite_similarity_with_jaccard(&a, &b, jaccard_similarity(&a.shingles, &b.shingles));
        assert!(score >= 0.85, "expected >= 0.85, got {score}");
        assert_eq!(overlap_kind(&a, &b), "ast_isomorphic");
        assert_eq!(severity_bucket(score, "ast_isomorphic"), "definite");
    }

    #[test]
    fn different_structure_produces_different_ast_hash() {
        let a = fingerprint_for_rust_fn("fn a(x: i32) -> i32 { x + 1 }");
        let b =
            fingerprint_for_rust_fn("fn b(x: i32) -> i32 { if x > 0 { x + 1 } else { x - 1 } }");
        assert_ne!(a.ast_hash, b.ast_hash);
        assert_ne!(a.cfg_hash, b.cfg_hash);
    }

    #[test]
    fn cfg_hash_matches_under_renaming_and_inline_changes() {
        // Two functions with identical control flow but different operations.
        let a = fingerprint_for_rust_fn(
            "fn a(x: i32) -> i32 { if x > 0 { return 1; } else { return 2; } }",
        );
        let b = fingerprint_for_rust_fn(
            "fn b(x: i32) -> i32 { if x > 0 { return 99; } else { return 100; } }",
        );
        assert_eq!(a.cfg_hash, b.cfg_hash);
    }

    #[test]
    fn jaccard_self_similarity_is_one() {
        let a = fingerprint_for_rust_fn(
            "fn a() { let x = 1; let y = 2; let z = x + y; println!(\"{}\", z); }",
        );
        assert!((jaccard_similarity(&a.shingles, &a.shingles) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jaccard_disjoint_is_zero() {
        let a = fingerprint_for_rust_fn(
            "fn a() { let aaaa = 1; let bbbb = 2; let cccc = 3; let dddd = 4; let eeee = 5; }",
        );
        let b = fingerprint_for_rust_fn(
            "fn b() { let zzzz = 9; let yyyy = 8; let xxxx = 7; let wwww = 6; let vvvv = 5; }",
        );
        let j = jaccard_similarity(&a.shingles, &b.shingles);
        // Some token overlap (e.g. `let`), but should be very low.
        assert!(j < 0.4, "expected low Jaccard, got {j}");
    }

    #[test]
    fn vector_cosine_rewards_shared_body_core() {
        let a = vec![1, 2, 3, 4, 5, 6];
        let b = vec![1, 2, 3, 4, 5, 99];
        let j = jaccard_similarity(&a, &b);
        let cosine = vector_cosine_similarity(&a, &b);
        assert!(cosine > j, "cosine={cosine}, jaccard={j}");
        assert!((vector_cosine_similarity(&a, &a) - 1.0).abs() < 1e-9);
        assert!(vector_cosine_similarity(&[], &[]).abs() < 1e-9);
    }

    #[test]
    fn redundancy_match_score_rejects_empty_body_vectors() {
        let a = Fingerprint {
            ast_hash: "ast_a".into(),
            cfg_hash: "cfg_a".into(),
            call_seq_hash: "call_a".into(),
            shingles: Vec::new(),
            body_tokens: 1,
            source_hash: "src_a".into(),
        };
        let b = Fingerprint {
            ast_hash: "ast_b".into(),
            cfg_hash: "cfg_b".into(),
            call_seq_hash: "call_b".into(),
            shingles: Vec::new(),
            body_tokens: 1,
            source_hash: "src_b".into(),
        };

        assert!(redundancy_match_score("a", &a, "b", &b, 0.55, true).is_none());
    }

    #[test]
    fn redundancy_match_score_downranks_generic_helpers() {
        let fp = Fingerprint {
            ast_hash: "ast".into(),
            cfg_hash: "cfg".into(),
            call_seq_hash: "call".into(),
            shingles: vec![1, 2, 3, 4, 5],
            body_tokens: 5,
            source_hash: "src".into(),
        };
        let regular = redundancy_match_score("compute", &fp, "compute", &fp, 0.55, true).unwrap();
        let generic = redundancy_match_score("drop", &fp, "drop", &fp, 0.55, true).unwrap();
        assert!(generic.generic_helper_downranked);
        assert!(generic.ranking_score < regular.ranking_score);
    }

    fn tiny_body_fingerprint(source_hash: &str) -> Fingerprint {
        Fingerprint {
            ast_hash: "tiny_ast".into(),
            cfg_hash: "tiny_cfg".into(),
            call_seq_hash: "tiny_call".into(),
            shingles: Vec::new(),
            body_tokens: 2,
            source_hash: source_hash.into(),
        }
    }

    #[test]
    fn empty_shingles_with_identical_hashes_require_identical_source() {
        // Tiny bodies (< SHINGLE_N tokens) hash identically on ast/cfg/call
        // even when textually different — without token evidence, only a
        // source_hash match is trustworthy.
        let a = tiny_body_fingerprint("src_a");
        let b = tiny_body_fingerprint("src_b");
        assert!(redundancy_match_score("width", &a, "height", &b, 0.55, true).is_none());

        let twin = tiny_body_fingerprint("src_a");
        let matched = redundancy_match_score("width", &a, "width_copy", &twin, 0.55, true)
            .expect("textually identical tiny bodies should match");
        assert_eq!(matched.overlap_kind, "ast_isomorphic");
        assert_eq!(matched.severity, "definite");
    }

    fn shingle_fingerprint(tag: &str, shingles: Vec<u32>) -> Fingerprint {
        Fingerprint {
            ast_hash: format!("{tag}_ast"),
            cfg_hash: format!("{tag}_cfg"),
            call_seq_hash: format!("{tag}_call"),
            body_tokens: shingles.len(),
            source_hash: format!("{tag}_src"),
            shingles,
        }
    }

    #[test]
    fn sub_floor_cosine_pairs_stay_naming_and_honor_include_naming() {
        // cosine 9/20 = 0.45 clears a 0.4 threshold but not the 0.55
        // severity floor: the pair keeps kind "naming" (no body_vector
        // relabel), gets severity "naming_only", and include_naming filters
        // it — kind, severity, and filter stay mutually consistent.
        let a = shingle_fingerprint("na", (1..=20).collect());
        let b_shingles: Vec<u32> = (1..=9).chain(101..=111).collect();
        let b = shingle_fingerprint("nb", b_shingles);

        assert!(redundancy_match_score("alpha", &a, "beta", &b, 0.4, false).is_none());
        let kept = redundancy_match_score("alpha", &a, "beta", &b, 0.4, true)
            .expect("include_naming=true keeps the pair");
        assert_eq!(kept.overlap_kind, "naming");
        assert_eq!(kept.severity, "naming_only");
    }

    #[test]
    fn cosine_rescue_relabels_naming_to_body_vector_as_likely() {
        // cosine 6/10 = 0.6 with jaccard 6/14 < 0.5 and all hashes distinct:
        // the naming pair is rescued by body-vector evidence, and rescued
        // pairs are reported even with include_naming=false.
        let a = shingle_fingerprint("va", (1..=10).collect());
        let b_shingles: Vec<u32> = (1..=6).chain(101..=104).collect();
        let b = shingle_fingerprint("vb", b_shingles);

        let rescued = redundancy_match_score("merge_spans", &a, "merge_ranges", &b, 0.55, false)
            .expect("cosine >= floor rescues the pair");
        assert_eq!(rescued.overlap_kind, "body_vector");
        assert_eq!(rescued.severity, "likely");
        assert!(!rescued.generic_helper_downranked);
    }

    #[test]
    fn same_name_non_generic_pairs_survive_the_gate_as_naming_only() {
        // clean_comment shape: identical helper name duplicated across
        // extractor modules, cosine 10/sqrt(24*18) ~= 0.48 — below every
        // practical threshold, invisible without the same-name rescue.
        let a = shingle_fingerprint("sna", (1..=24).collect());
        let b_shingles: Vec<u32> = (1..=10).chain(101..=108).collect();
        let b = shingle_fingerprint("snb", b_shingles);

        let rescued = redundancy_match_score("clean_comment", &a, "clean_comment", &b, 0.55, true)
            .expect("identical non-generic names with shared body must be retained");
        assert_eq!(rescued.overlap_kind, "naming");
        assert_eq!(rescued.severity, "naming_only");

        // Filtered without include_naming; inert for different or generic names.
        assert!(
            redundancy_match_score("clean_comment", &a, "clean_comment", &b, 0.55, false).is_none()
        );
        assert!(
            redundancy_match_score("clean_comment", &a, "strip_comment", &b, 0.55, true).is_none()
        );
        assert!(redundancy_match_score("new", &a, "new", &b, 0.55, true).is_none());
    }

    #[test]
    fn redundancy_eval_fixture_scores_real_cases() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/redundancy_eval_labeled.json"
        ))
        .expect("valid redundancy eval fixture");
        let threshold = fixture["threshold"].as_f64().expect("threshold");
        let include_naming = fixture["include_naming"].as_bool().expect("include_naming");

        let mut scored: Vec<(&str, RedundancyMatchScore)> = Vec::new();
        let mut rejected: Vec<&str> = Vec::new();
        let mut positives: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut seen_labels: std::collections::HashSet<&str> = std::collections::HashSet::new();

        for case in fixture["cases"].as_array().expect("cases") {
            let label = case["label"].as_str().expect("label");
            assert!(seen_labels.insert(label), "duplicate fixture label {label}");
            let expect = &case["expect"];
            let a = fixture_fingerprint(&case["a"]);
            let b = fixture_fingerprint(&case["b"]);
            let score = redundancy_match_score(
                case["a_name"].as_str().expect("a_name"),
                &a,
                case["b_name"].as_str().expect("b_name"),
                &b,
                threshold,
                include_naming,
            );
            match expect["outcome"].as_str().expect("outcome") {
                "reject" => {
                    assert!(
                        score.is_none(),
                        "case {label} should be rejected, got {score:?}"
                    );
                    rejected.push(label);
                }
                "match" => {
                    let score =
                        score.unwrap_or_else(|| panic!("case {label} should match threshold"));
                    assert_eq!(
                        score.overlap_kind,
                        expect["overlap_kind"].as_str().expect("overlap_kind"),
                        "case {label} overlap_kind"
                    );
                    assert_eq!(
                        score.severity,
                        expect["severity"].as_str().expect("severity"),
                        "case {label} severity"
                    );
                    assert_eq!(
                        score.generic_helper_downranked,
                        expect["generic_helper_downranked"]
                            .as_bool()
                            .expect("generic_helper_downranked"),
                        "case {label} generic_helper_downranked"
                    );
                    if expect["positive"].as_bool().expect("positive") {
                        positives.insert(label);
                    }
                    scored.push((label, score));
                }
                other => panic!("unknown outcome '{other}' for case {label}"),
            }
        }

        scored.sort_by(|(_, a), (_, b)| {
            b.ranking_score
                .partial_cmp(&a.ranking_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // A ranking tie would make the expected order an accident of sort
        // stability rather than scoring behavior — keep the fixture tie-free.
        for window in scored.windows(2) {
            assert!(
                window[0].1.ranking_score > window[1].1.ranking_score,
                "ranking tie between '{}' and '{}' — fixture must stay tie-free",
                window[0].0,
                window[1].0
            );
        }

        let labels = scored.iter().map(|(label, _)| *label).collect::<Vec<_>>();
        let expected = &fixture["expected"];
        let expected_labels = expected["ranked_labels"]
            .as_array()
            .expect("ranked labels")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(labels, expected_labels);
        let expected_rejected = expected["rejected_labels"]
            .as_array()
            .expect("rejected labels")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        assert_eq!(rejected, expected_rejected);

        // Metrics are recomputed from the ranking, so a fixture whose
        // expected.metrics disagree with its own ranked_labels fails loudly.
        let metrics = &expected["metrics"];
        for k in 1..=3 {
            let key = format!("p_at_{k}");
            let actual = round2(precision_at_k(&labels, &positives, k));
            let expected_metric = metrics[key.as_str()].as_f64().expect("p_at_k");
            assert!(
                (actual - expected_metric).abs() < 1e-9,
                "{key}: computed {actual}, fixture expects {expected_metric}"
            );
        }
        let actual_ap = round2(average_precision(&labels, &positives));
        let expected_ap = metrics["average_precision"]
            .as_f64()
            .expect("average_precision");
        assert!(
            (actual_ap - expected_ap).abs() < 1e-9,
            "average_precision: computed {actual_ap}, fixture expects {expected_ap}"
        );
    }

    fn fixture_fingerprint(value: &serde_json::Value) -> Fingerprint {
        Fingerprint {
            ast_hash: value["ast_hash"].as_str().expect("ast_hash").to_string(),
            cfg_hash: value["cfg_hash"].as_str().expect("cfg_hash").to_string(),
            call_seq_hash: value["call_seq_hash"]
                .as_str()
                .expect("call_seq_hash")
                .to_string(),
            shingles: value["shingles"]
                .as_array()
                .expect("shingles")
                .iter()
                .map(|item| item.as_u64().expect("shingle") as u32)
                .collect(),
            body_tokens: value["body_tokens"].as_u64().expect("body_tokens") as usize,
            source_hash: value["source_hash"]
                .as_str()
                .unwrap_or("fixture")
                .to_string(),
        }
    }

    fn round2(value: f64) -> f64 {
        (value * 100.0).round() / 100.0
    }

    fn precision_at_k(
        labels: &[&str],
        positives: &std::collections::HashSet<&str>,
        k: usize,
    ) -> f64 {
        let hits = labels
            .iter()
            .take(k)
            .filter(|label| positives.contains(**label))
            .count();
        hits as f64 / k as f64
    }

    fn average_precision(labels: &[&str], positives: &std::collections::HashSet<&str>) -> f64 {
        let mut hits = 0usize;
        let mut sum = 0.0;
        for (idx, label) in labels.iter().enumerate() {
            if positives.contains(*label) {
                hits += 1;
                sum += hits as f64 / (idx + 1) as f64;
            }
        }
        if positives.is_empty() {
            0.0
        } else {
            sum / positives.len() as f64
        }
    }

    #[test]
    fn shingles_roundtrip_through_string_format() {
        let original: Vec<u32> = vec![1, 2, 0xdead_beef, 0xffff_ffff];
        let fp = Fingerprint {
            ast_hash: "x".into(),
            cfg_hash: "x".into(),
            call_seq_hash: "x".into(),
            shingles: original.clone(),
            body_tokens: 0,
            source_hash: "x".into(),
        };
        let s = fp.shingles_to_string();
        let parsed = Fingerprint::shingles_from_string(&s);
        assert_eq!(parsed, original);
    }

    #[test]
    fn call_sequence_captures_order() {
        let a = fingerprint_for_rust_fn("fn a() { foo(); bar(); baz(); }");
        let b = fingerprint_for_rust_fn("fn b() { foo(); bar(); baz(); }");
        let c = fingerprint_for_rust_fn("fn c() { baz(); bar(); foo(); }");
        assert_eq!(a.call_seq_hash, b.call_seq_hash);
        assert_ne!(a.call_seq_hash, c.call_seq_hash);
    }

    #[test]
    fn severity_naming_only_for_low_score() {
        assert_eq!(severity_bucket(0.10, "naming"), "naming_only");
        assert_eq!(severity_bucket(0.30, "token_overlap"), "naming_only");
        assert_eq!(severity_bucket(0.60, "control_flow"), "likely");
    }
}
