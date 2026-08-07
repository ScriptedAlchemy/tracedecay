//! Apply-grade symbol rename bound to exact graph evidence.
//!
//! The rename consumes the identity a `tracedecay_rename_preview` reported
//! (node ID, qualified name, kind, defining file, old name) and rewrites the
//! whole-identifier occurrences of the old name on the exact lines the graph
//! attests: the declaration line plus every incoming reference edge. A bare
//! spelling is never sufficient; any drift between the bound identity and the
//! live graph, or between an attested site and the current source, refuses
//! before any write. Text-only occurrences (comments, strings, dynamic
//! dispatch, unresolved refs) are reported and never rewritten.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use tracedecay_application::source_edit::{
    RenameFileEditV1, RenameResult, RenameSymbolBindingV1, RenameTextOnlyMatchV1,
};

use crate::errors::{Result, TraceDecayError};

use super::super::TraceDecay;
use super::file_authority::SourceEditFileAuthority;
use super::plan::{capture_planned_source_edit, validate_planned_source_edit};
use super::preview::{
    MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff, edit_success_message,
};

/// Identifier-boundary byte test shared with the rename preview: `_`, ASCII
/// alphanumerics, and every non-ASCII byte count as identifier bytes so
/// multi-byte unicode identifiers are never falsely split at a boundary.
fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric() || b >= 0x80
}

/// Byte ranges of `name` in `haystack` bounded as a whole identifier
/// (neither neighbouring byte is an identifier byte).
fn identifier_ranges(haystack: &str, name: &str) -> Vec<(usize, usize)> {
    if name.is_empty() {
        return Vec::new();
    }
    let bytes = haystack.as_bytes();
    let name_len = name.len();
    let mut ranges = Vec::new();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(name) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let after_idx = abs + name_len;
        let after_ok = after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
        if before_ok && after_ok {
            ranges.push((abs, after_idx));
        }
        start = abs + name_len;
    }
    ranges
}

fn count_identifier_occurrences(haystack: &str, name: &str) -> usize {
    identifier_ranges(haystack, name).len()
}

/// Replaces every whole-identifier occurrence of `old` on one line with
/// `new`, returning the rewritten line and how many occurrences changed.
fn replace_identifiers_on_line(line: &str, old: &str, new: &str) -> (String, usize) {
    let ranges = identifier_ranges(line, old);
    if ranges.is_empty() {
        return (line.to_owned(), 0);
    }
    let mut rewritten = String::with_capacity(line.len() + ranges.len() * new.len());
    let mut cursor = 0;
    for &(start, end) in &ranges {
        rewritten.push_str(&line[cursor..start]);
        rewritten.push_str(new);
        cursor = end;
    }
    rewritten.push_str(&line[cursor..]);
    (rewritten, ranges.len())
}

/// Whether `name` is a plausible identifier for the indexed languages:
/// non-empty, does not start with an ASCII digit, and every byte is an
/// identifier byte. Conservative on purpose — a rename target that fails this
/// is refused rather than guessed at.
fn is_valid_identifier(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty() && !bytes[0].is_ascii_digit() && bytes.iter().copied().all(is_ident_byte)
}

/// The exact per-file rewrite a bound rename resolved to.
struct PlannedRenameFile {
    relative_path: String,
    original: String,
    modified: String,
    replaced_count: usize,
    text_only_count: usize,
    authority: SourceEditFileAuthority,
}

impl TraceDecay {
    /// Renames a graph-bound symbol across its declaration and every graph
    /// reference site. `dry_run` computes and reports the complete plan while
    /// writing nothing; the real apply publishes each file through the atomic
    /// source-edit file authority and restores every already-published
    /// preimage if a later file fails, so the workspace is never left
    /// half-renamed.
    pub(crate) async fn rename_symbol(
        &self,
        binding: &RenameSymbolBindingV1,
        new_name: &str,
        dry_run: bool,
    ) -> Result<RenameResult> {
        let refused = |message: String| RenameResult {
            success: false,
            symbol: binding.qualified_name.clone(),
            old_name: binding.old_name.clone(),
            new_name: new_name.to_owned(),
            files: Vec::new(),
            reference_count: 0,
            text_only_matches: Vec::new(),
            dry_run,
            diff: None,
            message,
        };

        if !is_valid_identifier(new_name) {
            return Ok(refused(format!(
                "new name {new_name:?} is not a valid identifier"
            )));
        }
        if !is_valid_identifier(&binding.old_name) {
            return Ok(refused(format!(
                "bound old name {:?} is not a valid identifier",
                binding.old_name
            )));
        }
        if new_name == binding.old_name {
            return Ok(refused(
                "new name is identical to the bound old name".to_owned(),
            ));
        }

        let Some(node) = self.get_node(&binding.node_id).await? else {
            return Ok(refused(format!(
                "stale rename evidence: node {} no longer exists — recompute tracedecay_rename_preview",
                binding.node_id
            )));
        };
        if node.name != binding.old_name
            || node.qualified_name != binding.qualified_name
            || node.kind.as_str() != binding.kind
            || node.file_path != binding.file
        {
            return Ok(refused(format!(
                "stale rename evidence: node {} is now `{}` ({}) in {} — recompute tracedecay_rename_preview",
                node.id,
                node.qualified_name,
                node.kind.as_str(),
                node.file_path
            )));
        }

        // Attested sites: the declaration line plus every incoming reference
        // edge that carries exact line evidence. A reference the graph cannot
        // place on a line blocks the rename rather than being guessed at.
        let mut sites: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
        sites
            .entry(node.file_path.clone())
            .or_default()
            .insert(node.start_line as usize);
        let incoming = self.get_incoming_edges(&binding.node_id).await?;
        let mut reference_count = 0usize;
        for edge in &incoming {
            let Some(source_node) = self.get_node(&edge.source).await? else {
                continue;
            };
            let Some(line) = edge.line else {
                return Ok(refused(format!(
                    "blocked: reference from `{}` in {} has no exact line evidence — resolve it manually, then recompute the preview",
                    source_node.qualified_name, source_node.file_path
                )));
            };
            reference_count += 1;
            sites
                .entry(source_node.file_path)
                .or_default()
                .insert(line as usize);
        }

        let mut planned: Vec<PlannedRenameFile> = Vec::with_capacity(sites.len());
        for (relative_path, attested_lines) in sites {
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&relative_path))?;
            let (source, _identity) = authority.read_to_string(&relative_path)?;
            let lines: Vec<&str> = source.lines().collect();

            // Resolve each attested line to the exact line holding a
            // whole-identifier occurrence (graph line evidence tolerates a
            // one-line skew, exactly like the preview's snippet lookup).
            let mut resolved_lines: BTreeSet<usize> = BTreeSet::new();
            for approx in attested_lines {
                let candidates = [approx, approx.saturating_sub(1), approx + 1];
                let Some(index) = candidates.into_iter().find(|&candidate| {
                    lines.get(candidate).is_some_and(|line| {
                        count_identifier_occurrences(line, &binding.old_name) > 0
                    })
                }) else {
                    return Ok(refused(format!(
                        "stale rename evidence: {relative_path}:{} no longer contains `{}` — recompute tracedecay_rename_preview",
                        approx + 1,
                        binding.old_name
                    )));
                };
                resolved_lines.insert(index);
            }

            // Collision: the new name already occurs as a whole identifier in
            // a touched file. Refuse rather than risk changed resolution.
            if let Some(collision_line) = lines
                .iter()
                .position(|line| count_identifier_occurrences(line, new_name) > 0)
            {
                return Ok(refused(format!(
                    "blocked: `{new_name}` already occurs in {relative_path}:{} — renaming would collide or shadow",
                    collision_line + 1
                )));
            }

            let mut replaced_count = 0usize;
            let mut total_occurrences = 0usize;
            let rebuilt: Vec<String> = lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    total_occurrences += count_identifier_occurrences(line, &binding.old_name);
                    if resolved_lines.contains(&index) {
                        let (rewritten, count) =
                            replace_identifiers_on_line(line, &binding.old_name, new_name);
                        replaced_count += count;
                        rewritten
                    } else {
                        (*line).to_owned()
                    }
                })
                .collect();
            let mut modified = rebuilt.join("\n");
            if source.ends_with('\n') && !modified.is_empty() {
                modified.push('\n');
            }
            planned.push(PlannedRenameFile {
                relative_path,
                original: source,
                modified,
                replaced_count,
                text_only_count: total_occurrences - replaced_count,
                authority,
            });
        }

        let files: Vec<RenameFileEditV1> = planned
            .iter()
            .map(|file| RenameFileEditV1 {
                file: file.relative_path.clone(),
                replaced_count: file.replaced_count,
            })
            .collect();
        let text_only_matches: Vec<RenameTextOnlyMatchV1> = planned
            .iter()
            .filter(|file| file.text_only_count > 0)
            .map(|file| RenameTextOnlyMatchV1 {
                file: file.relative_path.clone(),
                text_only_count: file.text_only_count,
            })
            .collect();

        if dry_run {
            let mut diff = String::new();
            for file in &planned {
                capture_planned_source_edit(
                    &file.relative_path,
                    Some(file.original.as_str()),
                    Some(file.modified.as_str()),
                );
                if !diff.is_empty() {
                    diff.push('\n');
                }
                let _ = writeln!(diff, "--- {}", file.relative_path);
                diff.push_str(&bounded_region_diff(
                    &file.original,
                    &file.modified,
                    PREVIEW_DIFF_CONTEXT,
                    MAX_PREVIEW_DIFF_LINES,
                ));
            }
            return Ok(RenameResult {
                success: true,
                symbol: binding.qualified_name.clone(),
                old_name: binding.old_name.clone(),
                new_name: new_name.to_owned(),
                files,
                reference_count,
                text_only_matches,
                dry_run: true,
                diff: Some(diff),
                message: edit_success_message(true, "rename previewed"),
            });
        }

        // Apply: revalidate every file against the captured plan, then
        // publish each through the atomic CAS file authority. A failure on a
        // later file restores every already-published preimage so the
        // workspace is never left half-renamed.
        for file in &planned {
            validate_planned_source_edit(
                &file.relative_path,
                Some(file.original.as_str()),
                Some(file.modified.as_str()),
            )?;
        }
        let mut published: Vec<&PlannedRenameFile> = Vec::with_capacity(planned.len());
        for file in &planned {
            let expected_identity = file.authority.current_identity()?;
            let publish = file.authority.publish(
                &file.relative_path,
                Some(file.original.as_str()),
                expected_identity.as_ref(),
                &file.modified,
                || {},
            );
            if let Err(error) = publish {
                let restored = rollback_published_rename_files(&self.project_root, &published);
                return Err(TraceDecayError::Config {
                    message: match restored {
                        Ok(()) => format!(
                            "rename aborted before writing {}: {error}; already-renamed files were restored",
                            file.relative_path
                        ),
                        Err(rollback_error) => format!(
                            "rename aborted before writing {}: {error}; rollback incomplete: {rollback_error}",
                            file.relative_path
                        ),
                    },
                });
            }
            published.push(file);
        }

        // A rename changes symbol identity (the name participates in node
        // IDs), so reindex the whole change set in one generation instead of
        // file-by-file — exactly like move_symbol.
        self.sync().await?;

        Ok(RenameResult {
            success: true,
            symbol: binding.qualified_name.clone(),
            old_name: binding.old_name.clone(),
            new_name: new_name.to_owned(),
            files,
            reference_count,
            text_only_matches,
            dry_run: false,
            diff: None,
            message: "rename applied".to_owned(),
        })
    }
}

/// Restores the exact preimage of every already-published rename file. Never
/// overwrites foreign bytes: a file that no longer holds the rename's intended
/// content is left alone and reported.
fn rollback_published_rename_files(
    project_root: &Path,
    published: &[&PlannedRenameFile],
) -> Result<()> {
    for file in published.iter().rev() {
        let absolute = project_root.join(&file.relative_path);
        let current =
            std::fs::read_to_string(&absolute).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "cannot inspect {} during rename rollback: {error}",
                    file.relative_path
                ),
            })?;
        if current == file.original {
            continue;
        }
        if current != file.modified {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{} changed concurrently; refusing to overwrite it during rename rollback",
                    file.relative_path
                ),
            });
        }
        crate::agents::safe_write_text_file(&absolute, &file.original, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{identifier_ranges, is_valid_identifier, replace_identifiers_on_line};

    #[test]
    fn identifier_matching_is_whole_word_and_unicode_safe() {
        assert_eq!(identifier_ranges("foo(foo_bar, foo)", "foo").len(), 2);
        assert_eq!(identifier_ranges("préfoo foo", "foo").len(), 1);
        assert_eq!(
            replace_identifiers_on_line("foo + foofoo + foo", "foo", "bar"),
            ("bar + foofoo + bar".to_owned(), 2)
        );
    }

    #[test]
    fn identifier_validity_is_conservative() {
        assert!(is_valid_identifier("renamed_symbol"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("1abc"));
        assert!(!is_valid_identifier("a b"));
        assert!(!is_valid_identifier("a-b"));
    }
}
