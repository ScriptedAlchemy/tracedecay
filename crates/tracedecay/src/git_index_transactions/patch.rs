use serde::Serialize;
use tracedecay_domain::{HunkRefV1, canonical_sha256, full_hunk_selection_bitmap};

use super::NativeGitIndexError;

const MAX_PATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Serialize)]
struct NativePatchDigestMaterial<'a> {
    header: &'a str,
    body: &'a [String],
}

/// A patch fragment produced by the fixed preview builder. Its fields are
/// private so callers cannot pass opaque patch text to the native executor.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedIndexPatch {
    hunk: HunkRefV1,
    bytes: Vec<u8>,
}

impl ValidatedIndexPatch {
    pub(crate) fn new(hunk: HunkRefV1, bytes: Vec<u8>) -> Result<Self, NativeGitIndexError> {
        hunk.validate()?;
        if bytes.is_empty() || bytes.len() > MAX_PATCH_BYTES {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| NativeGitIndexError::PatchDoesNotMatchHunk)?;
        let lines: Vec<&str> = text.lines().collect();
        let hunk_positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| (*line == hunk.hunk_header.as_str()).then_some(index))
            .collect();
        let Some(&hunk_index) = hunk_positions.first() else {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        };
        if hunk_positions.len() != 1
            || hunk_index < 2
            || !lines[hunk_index - 2].starts_with("--- ")
            || !lines[hunk_index - 1].starts_with("+++ ")
            || lines[..hunk_index - 2].iter().any(|line| !line.is_empty())
        {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        }

        let body: Vec<String> = lines[hunk_index + 1..]
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
        if body.is_empty()
            || body.iter().any(|line| {
                !line.starts_with(' ')
                    && !line.starts_with('+')
                    && !line.starts_with('-')
                    && !line.starts_with('\\')
            })
            || body.iter().any(|line| line.starts_with("\\ No newline"))
            || !body
                .iter()
                .any(|line| line.starts_with('+') || line.starts_with('-'))
        {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        }
        let patch_digest = canonical_sha256(&NativePatchDigestMaterial {
            header: &hunk.hunk_header,
            body: &body,
        })?;
        let context: Vec<&str> = body
            .iter()
            .filter(|line| line.starts_with(' '))
            .map(String::as_str)
            .collect();
        let context_digest = canonical_sha256(&context)?;
        if patch_digest != hunk.patch_digest || context_digest != hunk.context_digest {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        }
        let (old_lines, new_lines) = parse_hunk_line_counts(&hunk.hunk_header)?;
        if hunk.selected_line_bitmap != full_hunk_selection_bitmap(old_lines.max(new_lines)) {
            return Err(NativeGitIndexError::PartialHunkSelectionUnsupported);
        }
        Ok(Self { hunk, bytes })
    }

    pub(crate) fn hunk(&self) -> &HunkRefV1 {
        &self.hunk
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn parse_hunk_line_counts(header: &str) -> Result<(u32, u32), NativeGitIndexError> {
    let mut fields = header.split_whitespace();
    if fields.next() != Some("@@") {
        return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
    }
    let old = fields
        .next()
        .and_then(|value| value.strip_prefix('-'))
        .and_then(parse_hunk_range_count)
        .ok_or(NativeGitIndexError::PatchDoesNotMatchHunk)?;
    let new = fields
        .next()
        .and_then(|value| value.strip_prefix('+'))
        .and_then(parse_hunk_range_count)
        .ok_or(NativeGitIndexError::PatchDoesNotMatchHunk)?;
    if fields.next() != Some("@@") || fields.next().is_some() {
        return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
    }
    Ok((old, new))
}

fn parse_hunk_range_count(value: &str) -> Option<u32> {
    match value.split_once(',') {
        Some((start, count)) if !start.is_empty() => count.parse().ok(),
        None if !value.is_empty() => Some(1),
        _ => None,
    }
}
