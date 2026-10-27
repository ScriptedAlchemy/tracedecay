//! Compact orientation for an agent deciding what to read next.

use tracedecay_contracts::retrieval::{
    PrContextBriefV1, PrContextExcerptV1, PrContextLocationV1, PrContextSourceStateV1,
};
use tracedecay_runtime_core::timeutil::humanize_unix_secs;

fn inline(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn code(value: &str) -> String {
    let value = inline(value);
    literal_code(&value)
}

fn literal_code(value: &str) -> String {
    let longest = value.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let delimiter = "`".repeat(longest + 1);
    format!("{delimiter} {value} {delimiter}")
}

fn location(value: &PrContextLocationV1) -> String {
    if value.line == value.end_line {
        code(&format!("{}:{}", value.file, value.line))
    } else {
        code(&format!("{}:{}-{}", value.file, value.line, value.end_line))
    }
}

fn short_oid(value: &str) -> &str {
    if value.is_ascii() && value.len() > 12 {
        &value[..12]
    } else {
        value
    }
}

fn source_read(node_id: &str) -> String {
    literal_code(&format!(
        "tracedecay_source_body {}",
        serde_json::json!({ "node_id": node_id })
    ))
}

fn revision_source_read(revision: &str, location: &PrContextLocationV1) -> String {
    literal_code(&format!(
        "tracedecay_source_lines {}",
        serde_json::json!({
            "revision": revision,
            "file": location.file,
            "start_line": location.line,
            "end_line": location.end_line,
        })
    ))
}

fn state(value: PrContextSourceStateV1) -> &'static str {
    match value {
        PrContextSourceStateV1::Ready => "ready",
        PrContextSourceStateV1::Partial => "partial",
        PrContextSourceStateV1::Stale => "stale",
        PrContextSourceStateV1::Unavailable => "unavailable",
        PrContextSourceStateV1::NotPublished => "not published",
        PrContextSourceStateV1::Denied => "denied",
        PrContextSourceStateV1::NotMounted => "not mounted",
        PrContextSourceStateV1::Ambiguous => "ambiguous",
        PrContextSourceStateV1::NotFound => "not found",
    }
}

fn quoted(lines: &mut Vec<String>, text: &str) {
    lines.extend(text.lines().map(|line| format!("> {line}")));
}

fn observed_at(micros: i64) -> String {
    humanize_unix_secs(micros.div_euclid(1_000_000))
}

fn excerpt(lines: &mut Vec<String>, label: &str, value: &PrContextExcerptV1, indent: &str) {
    let longest = value
        .text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    lines.push(String::new());
    lines.push(format!(
        "{indent}{label}: {} at {}{}",
        location(&value.location),
        code(short_oid(&value.revision)),
        if value.truncated { " (excerpt)" } else { "" }
    ));
    lines.push(String::new());
    lines.push(format!("{indent}{fence}"));
    lines.extend(value.text.lines().map(|line| format!("{indent}{line}")));
    lines.push(format!("{indent}{fence}"));
    lines.push(String::new());
}

pub(crate) fn render_pr_context_brief(brief: &PrContextBriefV1) -> String {
    let mut lines = Vec::new();
    if let Some(pr) = &brief.hosted.pull_request {
        lines.push(format!("# PR #{}: {}", pr.number, inline(&pr.title)));
        lines.push(format!(
            "{}{}; {} observation at {}; observed head {}.",
            pr.state,
            if pr.draft { " (draft)" } else { "" },
            if pr.matches_head {
                "matching-head retained"
            } else {
                "different-head retained"
            },
            observed_at(pr.observed_at),
            code(short_oid(&pr.observed_head))
        ));
        if let Some(description) = &pr.description {
            lines.push(String::new());
            lines.push("## Author description".to_owned());
            quoted(&mut lines, description);
            if pr.description_truncated {
                lines.push(
                    "Description excerpt; omitted text remains in the retained source.".to_owned(),
                );
            }
        } else {
            lines.push("Author description was not retained.".to_owned());
        }
    } else {
        lines.push("# PR context".to_owned());
    }
    lines.push(String::new());
    lines.push(format!(
        "Comparison: {} → {} (merge base {}); {} files, known text lines +{} / −{}.",
        code(&brief.base),
        code(&brief.head),
        code(short_oid(&brief.merge_base)),
        brief.files_changed,
        brief.additions,
        brief.deletions
    ));
    lines.push(format!(
        "Exact head: {}. Local source: {}. Graph: {}.",
        code(&brief.head_oid),
        state(brief.source_analysis.state),
        state(brief.graph_analysis.state)
    ));

    lines.push(String::new());
    lines.push("## Discussion and checks".to_owned());
    lines.push(format!(
        "PR source: {}. {}",
        state(brief.hosted.source.state),
        brief.hosted.source.detail
    ));
    if brief.hosted.discussions.is_empty() {
        lines.push("No discussion rows are included in this briefing; source coverage above determines what was observed.".to_owned());
    }
    for discussion in &brief.hosted.discussions {
        let author = discussion
            .author
            .as_deref()
            .map(|author| format!(" by {}", code(author)))
            .unwrap_or_default();
        let event = discussion
            .event_at
            .map(|at| format!("; activity {}", observed_at(at)))
            .unwrap_or_default();
        let anchor = match (&discussion.file, discussion.line) {
            (Some(file), Some(line)) => format!(" at {}", code(&format!("{file}:{line}"))),
            (Some(file), None) => format!(" at {}", code(file)),
            _ => String::new(),
        };
        lines.push(format!(
            "- {} {}{}: {} / {}{} [{}{}; observed {}].",
            discussion.kind,
            code(&discussion.id),
            author,
            discussion.review_state,
            discussion.lifecycle,
            anchor,
            if discussion.matches_head {
                "matching head"
            } else {
                "historical or unremapped"
            },
            event,
            observed_at(discussion.observed_at)
        ));
        if let Some(text) = &discussion.text {
            quoted(&mut lines, text);
            if discussion.text_truncated {
                lines.push("  Body excerpt is truncated.".to_owned());
            }
        } else if discussion.kind == "review" && discussion.body_anchor.is_none() {
            lines.push("  Review state was observed without a retained body.".to_owned());
        } else {
            lines.push("  Comment exists; body was not expanded.".to_owned());
        }
        if let Some(anchor) = &discussion.body_anchor {
            lines.push(format!("  Body provenance: {}.", code(anchor)));
        }
        if let Some(url) = &discussion.url {
            lines.push(format!("  Source: {url}"));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "CI source: {}. {}",
        state(brief.hosted.ci_source.state),
        brief.hosted.ci_source.detail
    ));
    for check in &brief.hosted.checks {
        lines.push(format!(
            "- {} · {}: {}{} [{} {}; observed {}]{}.",
            code(&check.workflow),
            inline(&check.name),
            check.status,
            check
                .conclusion
                .as_ref()
                .map(|value| format!(" / {value}"))
                .unwrap_or_default(),
            if check.matches_head {
                "head"
            } else {
                "historical head"
            },
            code(short_oid(&check.head)),
            observed_at(check.observed_at),
            check
                .failed_step
                .as_ref()
                .map(|value| format!("; failed step {}", code(value)))
                .unwrap_or_default()
        ));
        if !check.locations.is_empty() {
            lines.push(format!(
                "  Locations: {}.",
                check
                    .locations
                    .iter()
                    .map(location)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines.push(format!("  Evidence: {}.", code(&check.evidence_anchor)));
    }
    if !brief.hosted.required_checks_known {
        lines.push(
            "Required-check inventory is unknown; these rows do not establish merge readiness."
                .to_owned(),
        );
    }

    if !brief.entry_points.is_empty() {
        lines.push(String::new());
        lines.push("## Start reading here".to_owned());
        for (index, entry) in brief.entry_points.iter().enumerate() {
            lines.push(format!(
                "{}. {} · {} ({}, revision {}): {}",
                index + 1,
                location(&entry.location),
                code(&entry.symbol),
                entry.change,
                code(short_oid(&entry.revision)),
                entry.reason
            ));
            let indent = " ".repeat((index + 1).to_string().len() + 2);
            let complete_excerpt = [&entry.before_excerpt, &entry.after_excerpt]
                .into_iter()
                .flatten()
                .any(|excerpt| {
                    !excerpt.truncated
                        && excerpt.revision == entry.revision
                        && excerpt.location.file == entry.location.file
                        && excerpt.location.line <= entry.location.line
                        && excerpt.location.end_line >= entry.location.end_line
                });
            if !complete_excerpt {
                let selector = entry
                    .node_id
                    .as_deref()
                    .map(source_read)
                    .unwrap_or_else(|| revision_source_read(&entry.revision, &entry.location));
                lines.push(format!("{indent}Read: {selector}."));
            }
            if let Some(value) = &entry.before_excerpt {
                excerpt(&mut lines, "Before", value, &indent);
            } else if let Some(signature) = &entry.before_signature {
                lines.push(format!(
                    "{indent}Before: {}{}",
                    code(signature),
                    if entry.before_signature_truncated {
                        " (signature excerpt)"
                    } else {
                        ""
                    }
                ));
            }
            if let Some(value) = &entry.after_excerpt {
                excerpt(&mut lines, "After", value, &indent);
            } else if let Some(signature) = &entry.after_signature {
                lines.push(format!(
                    "{indent}After: {}{}",
                    code(signature),
                    if entry.after_signature_truncated {
                        " (signature excerpt)"
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    if !brief.areas.is_empty() {
        lines.push(String::new());
        lines.push("## Changed areas".to_owned());
        for area in &brief.areas {
            lines.push(format!(
                "- {}: {} files, +{} / −{}.",
                code(&area.area),
                area.files,
                area.additions,
                area.deletions
            ));
        }
    }
    if !brief.changes.is_empty() {
        lines.push(String::new());
        lines.push("## Change map".to_owned());
        for change in &brief.changes {
            let previous = change
                .previous_file
                .as_ref()
                .map(|path| format!("{} → ", code(path)))
                .unwrap_or_default();
            lines.push(format!(
                "- {}{}: {}.",
                previous,
                code(&change.file),
                change.change
            ));
        }
    }
    if !brief.impact.is_empty() {
        lines.push(String::new());
        lines.push("## Nearby consumers".to_owned());
        for impact in &brief.impact {
            lines.push(format!(
                "- {} {} {} at {} (consumer file {}).",
                code(&impact.consumer),
                impact.relation,
                code(&impact.changed_symbol),
                location(&impact.location),
                if impact.consumer_changed {
                    "changed"
                } else {
                    "unchanged"
                }
            ));
            if let Some(node_id) = &impact.node_id {
                lines.push(format!("  Read: {}.", source_read(node_id)));
            }
        }
    }
    lines.push(String::new());
    lines.push("## Quality evidence and limits".to_owned());
    for quality in &brief.quality {
        lines.push(format!("- {}: {}", quality.signal, quality.observation));
    }
    lines.push(format!("- Source: {}", brief.source_analysis.detail));
    lines.push(format!("- Graph: {}", brief.graph_analysis.detail));
    if !brief.commits.is_empty() {
        lines.push(String::new());
        lines.push("## Commit intent".to_owned());
        for commit in &brief.commits {
            lines.push(format!(
                "- {} {}",
                code(short_oid(&commit.hash)),
                inline(&commit.subject)
            ));
        }
    }
    let budget = &brief.budget;
    lines.push(String::new());
    lines.push(format!("Budget: ~{} JSON-estimated tokens / {} requested{}. Omitted: {} files, {} entry points, {} discussion rows, {} checks, {} consumers, {} commits, {} areas, {} quality signals.",
        budget.estimated_tokens, budget.requested_tokens, if budget.over_budget { " (minimum evidence exceeds budget)" } else { "" },
        budget.files_omitted, budget.entry_points_omitted, budget.discussions_omitted, budget.checks_omitted,
        budget.impact_omitted, budget.commits_omitted, budget.areas_omitted, budget.quality_omitted));
    if !brief.next_steps.is_empty() {
        lines.push(String::new());
        lines.push("## Continue".to_owned());
        lines.extend(brief.next_steps.iter().map(|step| format!("- {step}")));
    }
    lines.join("\n")
}
