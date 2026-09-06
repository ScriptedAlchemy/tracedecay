//! Shared prompt-rules rendering and managed-block reconciliation.
//!
//! Copilot, Gemini, `OpenCode`, Kimi, and Vibe share the same marker-gated
//! tracedecay rules block. Claude and Kiro keep host-specific text but reuse
//! the block-splicing helpers here.

use std::ops::Range;
use std::path::Path;

use crate::errors::Result;

/// Marker heading shared by every standard prompt-rules host.
pub(crate) const PROMPT_RULE_MARKER: &str = "## Prefer tracedecay MCP tools";

/// Explicit ownership sentinels of a tracedecay-managed steering block.
///
/// The sentinels, not any heading or sentence inside the block, are the
/// ownership contract: install rewrites exactly the span between them,
/// uninstall removes exactly that span, and doctor judges the span's bytes
/// against the embedded block. Prose can therefore change freely without
/// another marker migration.
#[derive(Clone, Copy)]
pub(crate) struct OwnedBlockSentinels {
    pub start: &'static str,
    pub end: &'static str,
}

impl OwnedBlockSentinels {
    /// Render `body` wrapped in this block's sentinels.
    pub(crate) fn render(self, body: &str) -> String {
        format!("{}\n{body}\n{}", self.start, self.end)
    }

    /// Byte range of the first sentinel-delimited block starting at or after
    /// `from`. A start sentinel without an end sentinel is not a block.
    pub(crate) fn block_range(self, contents: &str, from: usize) -> Option<Range<usize>> {
        let start = from + contents[from..].find(self.start)?;
        let end = start + contents[start..].find(self.end)? + self.end.len();
        Some(start..end)
    }
}

/// Every tracedecay-owned range in `contents` in document order. `locate_first`
/// returns the earliest owned block (current or historical shape) starting at
/// or after an offset; ranges never overlap because each search resumes at the
/// previous block's end.
pub(crate) fn owned_block_ranges(
    contents: &str,
    locate_first: impl Fn(&str, usize) -> Option<Range<usize>>,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut from = 0;
    while from < contents.len() {
        let Some(range) = locate_first(contents, from) else {
            break;
        };
        from = range.end.max(range.start + 1);
        ranges.push(range);
    }
    ranges
}

/// Peer text with every owned range removed; when `replacement` is given it
/// takes the first range's place. Later ranges (duplicate or mixed-marker
/// installs) are dropped so the file converges on exactly one block. Peer
/// segments keep their order; only the blank lines around removed blocks are
/// normalized. `None` when nothing remains.
pub(crate) fn rebuild_with_owned_blocks(
    contents: &str,
    ranges: &[Range<usize>],
    replacement: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut cursor = 0;
    for (index, range) in ranges.iter().enumerate() {
        let peer = contents[cursor..range.start].trim();
        if !peer.is_empty() {
            parts.push(peer);
        }
        if index == 0
            && let Some(block) = replacement
        {
            parts.push(block);
        }
        cursor = range.end;
    }
    let tail = contents[cursor..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    if ranges.is_empty()
        && let Some(block) = replacement
    {
        parts.push(block);
    }
    if parts.is_empty() {
        return None;
    }
    let mut rebuilt = parts.join("\n\n");
    rebuilt.push('\n');
    Some(rebuilt)
}

/// Whether the owned ranges already are exactly one copy of `block`, so a
/// reinstall is a no-op rather than a rewrite.
pub(crate) fn owned_block_is_current(contents: &str, ranges: &[Range<usize>], block: &str) -> bool {
    matches!(ranges, [range] if &contents[range.clone()] == block)
}

/// Install `block` as the single owned block: unchanged when already current,
/// otherwise converge every current or historical owned range onto one copy
/// (the first range's position, or appended when none exists).
pub(crate) fn converge_owned_block(
    existing: &str,
    ranges: &[Range<usize>],
    block: &str,
) -> PromptRulesEdit {
    if owned_block_is_current(existing, ranges, block) {
        return PromptRulesEdit::Unchanged;
    }
    let rebuilt = rebuild_with_owned_blocks(existing, ranges, Some(block))
        .unwrap_or_else(|| format!("{block}\n"));
    if ranges.is_empty() {
        PromptRulesEdit::Added(rebuilt)
    } else {
        PromptRulesEdit::Refreshed(rebuilt)
    }
}

/// Remove every owned range, deleting the file when only owned text was there.
pub(crate) fn remove_owned_blocks(contents: &str, ranges: &[Range<usize>]) -> PromptRulesRemoval {
    if ranges.is_empty() {
        return PromptRulesRemoval::Unchanged;
    }
    match rebuild_with_owned_blocks(contents, ranges, None) {
        Some(rebuilt) => PromptRulesRemoval::Rewrite(rebuilt),
        None => PromptRulesRemoval::Remove,
    }
}

/// Earliest of the shipped boundaries that closes a heading-marked historical
/// block searched from `search_from`: the next `\n## ` heading, the managed
/// skill index, a current start sentinel, or EOF.
pub(crate) fn historical_heading_block_end(
    contents: &str,
    search_from: usize,
    sentinels: OwnedBlockSentinels,
) -> usize {
    let heading_end = heading_block_end(contents, search_from);
    contents[search_from..]
        .find(sentinels.start)
        .map_or(heading_end, |offset| heading_end.min(search_from + offset))
}

/// Managed-skill index marker prefix (the full marker carries a per-host
/// suffix); strip heuristics stop here.
const SKILL_INDEX_START_PREFIX: &str = "<!-- TRACEDECAY MANAGED SKILLS START";

/// Canonical rules paragraphs shared by the standard hosts.
const STANDARD_PARAGRAPHS: &[&str] = &[
    "Before reading source files or scanning the codebase, use the tracedecay MCP tools \
     (`tracedecay_context`, `tracedecay_grep`, `tracedecay_search`, `tracedecay_callers`, \
     `tracedecay_callees`, `tracedecay_impact`, `tracedecay_node`, `tracedecay_files`, \
     `tracedecay_affected`). Route literal/regex text to `tracedecay_grep`, symbol names \
     to `tracedecay_search`, and concepts to `tracedecay_context`. They provide instant \
     semantic results from a pre-built knowledge graph and are faster than file reads.",
    "For project/storage identity questions, use `tracedecay_active_project` \
     or `tracedecay_storage_status` instead of inferring from repo-local marker \
     files or direct DB paths.",
    "If a code analysis question cannot be fully answered by tracedecay MCP tools, \
     prefer built-in MCP tools first. If the user explicitly needs raw store \
     inspection, use the resolved graph DB path reported by `tracedecay_storage_status` \
     rather than a hardcoded repo-local path. Use SQL to answer complex structural \
     queries that go beyond what the built-in tools expose.",
    "For durable project/user facts, use `tracedecay_fact_store_add` to persist them and \
     `tracedecay_fact_store_search` to recall or deduplicate them; use \
     `tracedecay_fact_feedback` and read-only `tracedecay_memory_status` over ad-hoc notes. \
     Use `memory_scope=user` for durable preferences or projectless chat and \
     `memory_scope=project` for active-codebase facts. \
     Use `tracedecay_message_search` for active-project transcript recall when \
     prior conversation context matters. Do not store secrets, credentials, or \
     unnecessary PII in persistent facts.",
    super::CLI_FALLBACK_PROMPT_RULES,
    "If you discover a gap where an extractor, schema, or tracedecay tool could be \
     improved to answer a question natively, propose to the user that they open an issue \
     at https://github.com/ScriptedAlchemy/tracedecay describing the limitation. \
     **Remind the user to strip any sensitive or proprietary code from the bug description \
     before submitting.**",
];

/// Host-specific knobs for [`standard_prompt_rules`].
pub(crate) struct PromptRulesOptions {
    /// Extra paragraphs appended after the shared canonical text.
    pub extra_paragraphs: &'static [&'static str],
}

/// Renders the full managed block (marker heading plus paragraphs, no
/// surrounding newlines) for a standard host.
pub(crate) fn standard_prompt_rules(marker: &str, options: &PromptRulesOptions) -> String {
    let mut block = String::from(marker);
    for paragraph in STANDARD_PARAGRAPHS.iter().chain(options.extra_paragraphs) {
        block.push_str("\n\n");
        block.push_str(paragraph);
    }
    block
}

/// The CLI-fallback paragraph every host's rules must carry; exposed so
/// integration tests can assert parity across hosts.
pub fn cli_fallback_paragraph() -> &'static str {
    super::CLI_FALLBACK_PROMPT_RULES
}

/// End offset of a managed block whose marker heading ends at `search_from`:
/// the next `\n## ` heading, the managed-skill index start marker, or EOF,
/// whichever comes first.
fn heading_block_end(contents: &str, search_from: usize) -> usize {
    let heading = contents[search_from..].find("\n## ");
    let skill_index = contents[search_from..].find(SKILL_INDEX_START_PREFIX);
    let relative = match (heading, skill_index) {
        (Some(h), Some(s)) => h.min(s),
        (Some(h), None) => h,
        (None, Some(s)) => s,
        (None, None) => return contents.len(),
    };
    search_from + relative
}

/// Removes `contents[start..end]` and normalizes surrounding blank lines.
pub(crate) fn splice_out(contents: &str, start: usize, end: usize) -> String {
    let mut new_contents = String::new();
    new_contents.push_str(contents[..start].trim_end());
    let remainder = &contents[end..];
    if !remainder.is_empty() {
        new_contents.push_str("\n\n");
        new_contents.push_str(remainder.trim_start());
    }
    new_contents.trim().to_string()
}

/// Contents with the managed block removed (marker heading through
/// [`heading_block_end`]); `None` when the marker is absent.
pub(crate) fn strip_heading_block(contents: &str, marker: &str) -> Option<String> {
    let start = contents.find(marker)?;
    let end = heading_block_end(contents, start + marker.len());
    Some(splice_out(contents, start, end))
}

/// Render preserved operator text followed by exactly one managed block.
pub(crate) fn refreshed_contents(stripped: &str, block: &str) -> String {
    let mut new_contents = String::with_capacity(stripped.len() + block.len() + 3);
    new_contents.push_str(stripped);
    if !new_contents.is_empty() {
        new_contents.push_str("\n\n");
    }
    new_contents.push_str(block);
    new_contents.push('\n');
    new_contents
}

/// Result of reconciling one host's prompt-rule syntax under the path lock.
pub(crate) enum PromptRulesEdit {
    Unchanged,
    Refreshed(String),
    Added(String),
}

pub(crate) enum PromptRulesRemoval {
    Unchanged,
    Rewrite(String),
    Remove,
}

#[derive(Clone, Copy)]
enum PromptRulesEditOutcome {
    Unchanged,
    Refreshed,
    Added,
}

/// Read, reconcile, and publish host-specific prompt rules in one transaction.
///
/// The callback runs while the stable per-path lock is held, so no host branch
/// can compute replacement bytes from an observation made outside the write
/// authority.
pub(crate) fn reconcile_prompt_rules_with(
    path: &Path,
    reconcile: impl FnOnce(&str) -> Result<PromptRulesEdit>,
) -> Result<()> {
    let outcome = super::update_text_file_transactionally(path, |existing| {
        Ok(match reconcile(existing)? {
            PromptRulesEdit::Unchanged => (
                PromptRulesEditOutcome::Unchanged,
                super::TextFileMutation::Unchanged,
            ),
            PromptRulesEdit::Refreshed(contents) => (
                PromptRulesEditOutcome::Refreshed,
                super::TextFileMutation::Write(contents),
            ),
            PromptRulesEdit::Added(contents) => (
                PromptRulesEditOutcome::Added,
                super::TextFileMutation::Write(contents),
            ),
        })
    })?;
    match outcome {
        PromptRulesEditOutcome::Unchanged => {
            eprintln!(
                "  {} already contains tracedecay rules, skipping",
                path.display()
            );
        }
        PromptRulesEditOutcome::Refreshed => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Refreshed tracedecay rules in {}",
                path.display()
            );
        }
        PromptRulesEditOutcome::Added => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Added tracedecay rules to {}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Read, reconcile, and conditionally remove host-specific prompt rules while
/// holding the same path lock used by installation.
pub(crate) fn remove_prompt_rules_with(
    path: &Path,
    reconcile: impl FnOnce(&str) -> Result<PromptRulesRemoval>,
) -> Result<()> {
    let removed = super::update_text_file_transactionally(path, |existing| {
        Ok(match reconcile(existing)? {
            PromptRulesRemoval::Unchanged => (false, super::TextFileMutation::Unchanged),
            PromptRulesRemoval::Rewrite(contents) => {
                (true, super::TextFileMutation::Write(contents))
            }
            PromptRulesRemoval::Remove => (true, super::TextFileMutation::Remove),
        })
    })?;
    if removed {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            path.display()
        );
    } else {
        eprintln!(
            "  {} does not contain removable tracedecay rules, skipping",
            path.display()
        );
    }
    Ok(())
}

/// Remove the standard marker-gated rules block from `path`, deleting the
/// file when nothing else remains. Shared by every host whose uninstall is
/// exactly "strip the [`PROMPT_RULE_MARKER`] block" (Kimi, `OpenCode`).
pub(crate) fn remove_standard_prompt_rules(path: &Path) -> Result<()> {
    remove_prompt_rules_with(path, |contents| {
        if !contents.contains("tracedecay") {
            return Ok(PromptRulesRemoval::Unchanged);
        }
        let Some(new_contents) = strip_heading_block(contents, PROMPT_RULE_MARKER) else {
            return Ok(PromptRulesRemoval::Unchanged);
        };
        if new_contents.is_empty() {
            Ok(PromptRulesRemoval::Remove)
        } else {
            Ok(PromptRulesRemoval::Rewrite(format!("{new_contents}\n")))
        }
    })
}

/// Install or refresh the managed rules block in `path`.
pub(crate) fn reconcile_prompt_rules(path: &Path, marker: &str, block: &str) -> Result<()> {
    reconcile_prompt_rules_with(path, |existing| {
        if existing.contains(block) {
            return Ok(PromptRulesEdit::Unchanged);
        }
        if let Some(stripped) = strip_heading_block(existing, marker) {
            return Ok(PromptRulesEdit::Refreshed(refreshed_contents(
                &stripped, block,
            )));
        }
        Ok(PromptRulesEdit::Added(format!("{existing}\n{block}\n")))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{PROMPT_RULE_MARKER, reconcile_prompt_rules};

    const ORIGINAL: &[u8] = b"operator-owned instructions\n";
    const BLOCK: &str = "## Prefer tracedecay MCP tools\n\nmanaged instructions";

    #[test]
    fn failed_write_intent_leaves_existing_prompt_rules_byte_identical() {
        let root = tempfile::tempdir().expect("prompt-rules fixture");
        let prompt = root.path().join("AGENTS.md");
        fs::write(&prompt, ORIGINAL).expect("original prompt rules");
        let blocked_intent_root = root.path().join("blocked-intent-root");
        fs::write(&blocked_intent_root, b"not a directory").expect("blocked intent root");

        let error = crate::agents::with_host_config_write_intents(blocked_intent_root, || {
            reconcile_prompt_rules(&prompt, PROMPT_RULE_MARKER, BLOCK)
        })
        .expect_err("an unpersisted write intent must refuse the prompt-rules write");

        assert!(
            error
                .to_string()
                .contains("could not create host config write intent directory"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read(&prompt).expect("prompt rules after refused write"),
            ORIGINAL,
            "intent failure must not expose unowned prompt-rule bytes"
        );
    }

    #[test]
    fn foreign_edit_after_refresh_read_is_refused_without_overwrite() {
        let root = tempfile::tempdir().expect("prompt-rules fixture");
        let prompt = root.path().join("AGENTS.md");
        let original = format!(
            "operator-owned instructions\n\n{PROMPT_RULE_MARKER}\n\nstale managed instructions\n"
        );
        fs::write(&prompt, &original).expect("original prompt rules");
        let pause = crate::agents::pause_next_host_config_write_after_validation(&prompt);
        let writer_prompt = prompt.clone();
        let writer = std::thread::spawn(move || {
            reconcile_prompt_rules(&writer_prompt, PROMPT_RULE_MARKER, BLOCK)
                .map_err(|error| error.to_string())
        });
        pause.wait_until_reached();
        let foreign = b"operator changed these instructions concurrently\n";
        fs::write(&prompt, foreign).expect("foreign concurrent edit");
        pause.resume();
        let error = writer
            .join()
            .expect("prompt-rules writer")
            .expect_err("a stale refresh must refuse the foreign edit");

        assert!(
            error.to_string().contains("changed since it was read"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read(&prompt).expect("prompt rules after stale refresh"),
            foreign,
            "a stale refresh must not overwrite foreign bytes"
        );
    }

    #[test]
    fn refresh_preserves_a_slugged_managed_skill_index_boundary() {
        let root = tempfile::tempdir().expect("prompt-rules fixture");
        let prompt = root.path().join("AGENTS.md");
        let start = "<!-- TRACEDECAY MANAGED SKILLS START opencode -->";
        let end = "<!-- TRACEDECAY MANAGED SKILLS END opencode -->";
        let original = format!(
            "{PROMPT_RULE_MARKER}\n\nstale managed instructions\n\n\
             {start}\n## TraceDecay managed skills\n\nmanaged skill\n{end}\n"
        );
        fs::write(&prompt, original).expect("original prompt rules");

        reconcile_prompt_rules(&prompt, PROMPT_RULE_MARKER, BLOCK)
            .expect("refresh must preserve the managed-skill block");

        let refreshed = fs::read_to_string(&prompt).expect("refreshed prompt rules");
        assert!(
            refreshed.contains(start),
            "slugged start marker was removed"
        );
        assert!(refreshed.contains(end), "slugged end marker was removed");
        assert!(refreshed.contains("managed skill"));
    }
}
