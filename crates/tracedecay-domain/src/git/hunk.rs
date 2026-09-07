//! Immutable hunk selection and compare-and-swap identity.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{DomainError, ManifestDigest, RepositoryId, WorktreeId, canonical_sha256};

use super::*;

/// `HunkRef` operation direction: working tree to index, or index to
/// HEAD/base. No other direction is encodable.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum HunkDirectionV1 {
    WorkingTreeToIndex,
    IndexToHead,
}

/// Expected blob identity, or explicit absent-file state.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitBlobExpectationV1 {
    Present(GitOidV1),
    AbsentFile,
}

impl GitBlobExpectationV1 {
    pub fn blob(&self) -> Option<&GitOidV1> {
        match self {
            Self::Present(oid) => Some(oid),
            Self::AbsentFile => None,
        }
    }
}

/// Expected index entry state for compare-and-swap: blob identity (or
/// absent), mode, and unmerged-stage state. `unmerged_stage` is `None` for a
/// merged (stage-0) entry.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitIndexEntryExpectationV1 {
    pub blob: GitBlobExpectationV1,
    pub mode: Option<GitFileModeV1>,
    pub unmerged_stage: Option<u8>,
}

impl GitIndexEntryExpectationV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self.unmerged_stage {
            None => Ok(()),
            Some(0) => Err(DomainError::NonCanonical {
                field: "index unmerged stage",
            }),
            Some(stage) if stage <= 3 => Ok(()),
            Some(_) => Err(DomainError::NonCanonical {
                field: "index unmerged stage",
            }),
        }
    }
}

/// One parsed `@@ -old[,count] +new[,count] @@[ section]` unified hunk header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedHunkHeader<'a> {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// The optional trailing section heading. Callers whose envelope forbids
    /// a section (normalized stored headers) reject `Some`.
    pub section: Option<&'a str>,
}

/// Parses one unified hunk header exactly as git emits it: single-space
/// separation, numeric start/count fields (an omitted count is 1), a
/// mandatory ` @@` terminator, and an optional space-separated section
/// heading after it. Anything looser is not a hunk header.
pub fn parse_hunk_header(line: &str) -> Option<ParsedHunkHeader<'_>> {
    let body = line.strip_prefix("@@ -")?;
    let (old, rest) = body.split_once(' ')?;
    let rest = rest.strip_prefix('+')?;
    let (new, rest) = rest.split_once(" @@")?;
    let (old_start, old_count) = parse_hunk_range(old)?;
    let (new_start, new_count) = parse_hunk_range(new)?;
    let section = if rest.is_empty() {
        None
    } else {
        let section = rest.strip_prefix(' ')?.trim();
        (!section.is_empty()).then_some(section)
    };
    Some(ParsedHunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
        section,
    })
}

fn parse_hunk_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

pub fn full_hunk_selection_bitmap(line_count: u32) -> Vec<u64> {
    if line_count == 0 {
        return vec![0];
    }
    let words = line_count.div_ceil(64) as usize;
    let mut bitmap = vec![u64::MAX; words];
    let remainder = line_count % 64;
    if remainder != 0 {
        bitmap[words - 1] = (1u64 << remainder) - 1;
    }
    bitmap
}

/// Immutable hunk identity for compare-and-swap. A hunk is identified by exact repository,
/// direction, path, expected base/index/worktree identity, normalized hunk
/// header, context and patch digests, and the preview that issued the
/// reference — never by display ordinal or line number alone.
///
/// query mints these as read-only identity evidence only. Applying them is a
/// daemon Git mutation path and is not representable here.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HunkRefV1 {
    pub repository: RepositoryId,
    pub worktree: WorktreeId,
    pub direction: HunkDirectionV1,
    pub path: String,
    /// Old path for a rename or copy.
    pub original_path: Option<String>,
    pub expected_base_blob: GitBlobExpectationV1,
    pub expected_index_entry: GitIndexEntryExpectationV1,
    /// Expected working-tree identity when the operation reads the worktree:
    /// a native content digest or explicit absent-file state. `None` means
    /// the operation direction does not read the worktree.
    pub expected_worktree_blob: Option<GitBlobExpectationV1>,
    pub expected_worktree_mode: Option<GitFileModeV1>,
    /// Normalized `@@ -o,l +n,m @@` header text.
    pub hunk_header: String,
    pub context_digest: ManifestDigest,
    pub patch_digest: ManifestDigest,
    /// Selected hunk-line bitmap (little-endian word order, line 1 = bit 0
    /// of word 0). Full-hunk identity covers the larger old/new side so
    /// deletion-only hunks remain representable.
    pub selected_line_bitmap: Vec<u64>,
    /// Attributes/filter identity relevant to clean/smudge and EOL handling.
    pub attributes_digest: Option<ManifestDigest>,
    pub preview_id: String,
    pub schema_version: String,
    pub snapshot_digest: ManifestDigest,
}

#[derive(Serialize)]
struct HunkRefDigestEnvelope<'a> {
    domain: &'static str,
    hunk_ref: &'a HunkRefV1,
}

impl HunkRefV1 {
    pub fn selected_line_count(&self) -> u64 {
        self.selected_line_bitmap
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum()
    }

    pub fn selects_line(&self, line: u32) -> bool {
        if line == 0 {
            return false;
        }
        let index = (line - 1) as usize;
        self.selected_line_bitmap
            .get(index / 64)
            .is_some_and(|word| word & (1u64 << (index % 64)) != 0)
    }

    /// Canonical domain-separated digest of this hunk reference. This digest
    /// is the `HunkRef` identity used by preview/apply compare-and-swap.
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&HunkRefDigestEnvelope {
            domain: HUNK_REF_DIGEST_DOMAIN,
            hunk_ref: self,
        })
    }

    /// Verify a previously issued digest against this reference.
    pub fn verify_digest(&self, digest: &ManifestDigest) -> Result<(), DomainError> {
        if &self.compute_digest()? == digest {
            Ok(())
        } else {
            Err(DomainError::DigestMismatch)
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_path_label(&self.path, "hunk ref path")?;
        if let Some(original) = &self.original_path {
            validate_path_label(original, "hunk ref original path")?;
        }
        validate_path_label(&self.hunk_header, "hunk ref header")?;
        validate_path_label(&self.preview_id, "hunk ref preview id")?;
        validate_path_label(&self.schema_version, "hunk ref schema version")?;
        self.expected_index_entry.validate()?;
        match (&self.direction, &self.expected_worktree_blob) {
            (HunkDirectionV1::WorkingTreeToIndex, Some(GitBlobExpectationV1::Present(_)))
                if self.expected_worktree_mode.is_some() => {}
            (HunkDirectionV1::WorkingTreeToIndex, Some(GitBlobExpectationV1::AbsentFile))
                if self.expected_worktree_mode.is_none() => {}
            (HunkDirectionV1::IndexToHead, None) if self.expected_worktree_mode.is_none() => {}
            _ => {
                return Err(DomainError::NonCanonical {
                    field: "hunk ref worktree expectation",
                });
            }
        }
        if self.selected_line_bitmap.is_empty()
            || self.selected_line_bitmap.iter().all(|word| *word == 0)
        {
            return Err(DomainError::Empty {
                field: "hunk ref selected line bitmap",
            });
        }
        Ok(())
    }
}

/// Domain separator for the immutable repository-state digest retained by a
/// Git index preview. This digest is distinct from the content-addressed
/// [`RepositoryStateSnapshotId`] so it can bind the full typed snapshot into
/// every `HunkRefV1` compare-and-swap precondition.
pub const GIT_INDEX_SNAPSHOT_DIGEST_DOMAIN_V1: &str = "tracedecay.git-index.snapshot.v1";

/// Domain separator for a canonical commitment to the complete commit intent.
pub const GIT_INDEX_COMMIT_INTENT_DIGEST_DOMAIN_V1: &str = "tracedecay.git-index.commit-intent.v1";

/// Domain separator for immutable Git index previews.
pub const GIT_INDEX_PREVIEW_DIGEST_DOMAIN_V1: &str = "tracedecay.git-index.preview.v1";

/// Domain separator for terminal Git index transaction receipts.
pub const GIT_INDEX_RECEIPT_DIGEST_DOMAIN_V1: &str = "tracedecay.git-index.receipt.v1";

crate::canonical_text::validated_string_newtype!(
    schema,
    DomainError,
    validate_path_label;
    GitIndexPreviewId => "git index preview id",
    GitIndexTransactionId => "git index transaction id",
    GitIndexReceiptId => "git index receipt id",
    GitIndexIdempotencyKey => "git index idempotency key",
);

#[cfg(test)]
mod hunk_header_tests {
    use super::{ParsedHunkHeader, parse_hunk_header};

    #[test]
    fn parses_explicit_counts_and_section() {
        assert_eq!(
            parse_hunk_header("@@ -10,6 +12,8 @@ fn example() {"),
            Some(ParsedHunkHeader {
                old_start: 10,
                old_count: 6,
                new_start: 12,
                new_count: 8,
                section: Some("fn example() {"),
            })
        );
    }

    #[test]
    fn omitted_counts_default_to_one_and_no_section_is_none() {
        assert_eq!(
            parse_hunk_header("@@ -5 +0,0 @@"),
            Some(ParsedHunkHeader {
                old_start: 5,
                old_count: 1,
                new_start: 0,
                new_count: 0,
                section: None,
            })
        );
    }

    #[test]
    fn trailing_whitespace_after_the_terminator_is_not_a_section() {
        assert_eq!(
            parse_hunk_header("@@ -1,2 +3,4 @@ ").and_then(|header| header.section),
            None
        );
    }

    #[test]
    fn malformed_headers_are_rejected() {
        for header in [
            "@@ -,2 +3,4 @@",                     // empty old start
            "@@ -1,2 +3, @@",                     // empty new count
            "@@ -x,2 +3,4 @@",                    // non-numeric start
            "@@ -1,2 +3,4",                       // missing terminator
            "@@ -1,2 +3,4 @@x",                   // terminator glued to trailing text
            "@@  -1,2 +3,4 @@",                   // double separator
            "@@ +1,2 -3,4 @@",                    // swapped signs
            "@@ -1,2,3 +4,5 @@",                  // extra range field
            "-1,2 +3,4 @@",                       // missing prefix
            "@@ -1,2 +18446744073709551616,4 @@", // start exceeds u32
        ] {
            assert_eq!(parse_hunk_header(header), None, "must reject {header:?}");
        }
    }
}
