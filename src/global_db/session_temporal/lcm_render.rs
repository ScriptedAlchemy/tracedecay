//! LCM content shaping after canonical temporal selection and hydration.

use crate::sessions::lcm::{LcmContentSlice, LcmExpandResponse};

pub(super) fn apply_canonical_content(
    mut expansion: LcmExpandResponse,
    slice: LcmContentSlice,
    canonical_content: &str,
) -> LcmExpandResponse {
    let total_chars = canonical_content.chars().count();
    let offset = slice.offset.min(total_chars);
    let content = canonical_content
        .chars()
        .skip(offset)
        .take(slice.limit)
        .collect::<String>();
    let returned_chars = content.chars().count();

    expansion.content.clone_from(&content);
    expansion.content_range.offset = offset as u64;
    expansion.content_range.limit = slice.limit as u64;
    expansion.content_range.returned_chars = returned_chars as u64;
    expansion.content_range.total_chars = total_chars as u64;
    expansion.content_range.truncated =
        offset > 0 || offset.saturating_add(returned_chars) < total_chars;
    if let Some(raw) = expansion.raw_message.as_mut() {
        raw.content.clone_from(&content);
    }
    if let Some(summary) = expansion.summary_node.as_mut() {
        summary.summary_text = content;
    }
    expansion
}
