//! Shared prompt-rules rendering and managed-block reconciliation.
//!
//! Copilot, Gemini, `OpenCode`, Kimi, and Vibe share the same marker-gated
//! tracedecay rules block. Claude and Kiro keep host-specific text but reuse
//! the block-splicing helpers here.

use std::path::Path;

use crate::errors::Result;

/// Marker heading shared by every standard prompt-rules host.
pub(crate) const PROMPT_RULE_MARKER: &str = "## Prefer tracedecay MCP tools";

/// Managed-skill index marker; strip heuristics stop here.
pub(crate) const SKILL_INDEX_START: &str = "<!-- TRACEDECAY MANAGED SKILLS START -->";

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
    let skill_index = contents[search_from..].find(SKILL_INDEX_START);
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
            PromptRulesEdit::Unchanged => (PromptRulesEditOutcome::Unchanged, None),
            PromptRulesEdit::Refreshed(contents) => {
                (PromptRulesEditOutcome::Refreshed, Some(contents))
            }
            PromptRulesEdit::Added(contents) => (PromptRulesEditOutcome::Added, Some(contents)),
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

/// Shared uninstall for the standard hosts: strips the managed block and
/// deletes the file when nothing else remains.
pub(crate) fn remove_prompt_rules(path: &Path, marker: &str) {
    if !path.exists() {
        return;
    }
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    if !contents.contains("tracedecay") {
        eprintln!(
            "  {} does not contain tracedecay rules, skipping",
            path.display()
        );
        return;
    }
    let Some(new_contents) = strip_heading_block(&contents, marker) else {
        return;
    };
    if new_contents.is_empty() {
        std::fs::remove_file(path).ok();
        eprintln!("\x1b[32m✔\x1b[0m Removed {} (was empty)", path.display());
    } else {
        std::fs::write(path, format!("{new_contents}\n")).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay rules from {}",
            path.display()
        );
    }
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
}
