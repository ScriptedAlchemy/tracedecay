//! Legacy LCM field shaping after canonical temporal selection and hydration.

use crate::global_db::GlobalDb;
use crate::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeResponse, LcmError, LcmExpandRequest,
    LcmExpandResponse,
};

pub(crate) async fn describe(
    db: &GlobalDb,
    request: LcmDescribeRequest,
) -> Result<LcmDescribeResponse, LcmError> {
    let mut description = db.lcm_describe(request).await?;
    for raw in &mut description.raw_messages {
        raw.content_preview.clear();
        raw.content_range.returned_chars = 0;
    }
    for summary in &mut description.summary_nodes {
        summary.summary_preview.clear();
    }
    if let Some(external) = description.external_payload.as_mut() {
        external.content_preview.clear();
    }
    Ok(description)
}

pub(crate) async fn expand(
    db: &GlobalDb,
    request: LcmExpandRequest,
    canonical_content: &str,
) -> Result<LcmExpandResponse, LcmError> {
    let slice = request.content_slice.unwrap_or(LcmContentSlice {
        offset: 0,
        limit: usize::MAX,
    });
    let mut expansion = db.lcm_expand(request).await?;
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
    Ok(expansion)
}
