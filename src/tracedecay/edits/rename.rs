//! Apply-grade symbol rename bound to exact graph and candidate-state evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use tracedecay_application::source_edit::{
    RenameDispositionCountsV1, RenameFileEditV1, RenameHazardKindV1, RenameHazardV1,
    RenameImpactV1, RenameProtectedValueCategoryV1, RenameProtectedValueV1, RenameResult,
    RenameSiteDispositionV1, RenameSiteKindV1, RenameSiteV1, RenameSymbolBindingV1,
};
use tracedecay_domain::{
    ContentDigest, ManifestDigest, SnapshotFileDispositionV1, canonical_sha256,
};
use tracedecay_usecases::tracedecay::SourceEditGraphReadV1;

use crate::errors::{Result, TraceDecayError};

use super::super::TraceDecay;
use super::file_authority::SourceEditFileAuthority;
use super::plan::{
    PlannedSourceEditFile, SourceEditManifestPublishOutcome, capture_planned_source_edit,
    publish_planned_source_edit_manifest,
};
use super::preview::{
    MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff, edit_success_message,
};

mod graph_evidence;
mod lexical;

use graph_evidence::{RenameGraphEvidenceLoadV1, RenameGraphSiteV1, ensure_active, relation_kind};
use lexical::{is_valid_identifier, string_literal_at};

fn is_ident_byte(byte: u8) -> bool {
    byte == b'_' || byte == b'$' || byte.is_ascii_alphanumeric() || byte >= 0x80
}

fn identifier_ranges(haystack: &str, name: &str) -> Vec<(usize, usize)> {
    if name.is_empty() {
        return Vec::new();
    }
    let bytes = haystack.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    while let Some(position) = haystack[start..].find(name) {
        let absolute = start + position;
        let end = absolute + name.len();
        if (absolute == 0 || !is_ident_byte(bytes[absolute - 1]))
            && (end == bytes.len() || !is_ident_byte(bytes[end]))
        {
            ranges.push((absolute, end));
        }
        start = end;
    }
    ranges
}

fn source_lines(source: &str) -> Vec<(usize, &str)> {
    source
        .split_inclusive('\n')
        .scan(0usize, |offset, segment| {
            let start = *offset;
            *offset += segment.len();
            Some((start, segment.strip_suffix('\n').unwrap_or(segment)))
        })
        .collect()
}

fn is_generated_path(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("generated" | "gen" | "vendor" | "vendored" | "target" | "node_modules")
        )
    })
}

fn looks_like_test(path: &str, node_name: Option<&str>) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("tests/")
        || path.starts_with("test/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.ends_with("_test.rs")
        || path.ends_with(".test.ts")
        || path.ends_with(".spec.ts")
        || node_name.is_some_and(|name| name.starts_with("test_") || name.ends_with("_test"))
}

fn macro_syntax_at(line: &str, start: usize) -> bool {
    line.contains("macro_rules!")
        || ["!(", "![", "!{"].into_iter().any(|marker| {
            line.get(..start)
                .is_some_and(|prefix| prefix.contains(marker))
        })
}

fn comment_at(line: &str, start: usize) -> bool {
    line.get(..start).is_some_and(|prefix| {
        prefix.contains("//") || prefix.contains("/*") || line.trim_start().starts_with('#')
    })
}

fn protected_category(line: &str) -> RenameProtectedValueCategoryV1 {
    let lower = line.to_ascii_lowercase();
    if lower.contains("serde") || lower.contains("rename") {
        RenameProtectedValueCategoryV1::SerializedName
    } else if lower.contains("sql") {
        RenameProtectedValueCategoryV1::SqlIdentifier
    } else if lower.contains("wire") || lower.contains("json") {
        RenameProtectedValueCategoryV1::WireValue
    } else if lower.contains("persist") || lower.contains("storage") || lower.contains("store_key")
    {
        RenameProtectedValueCategoryV1::PersistedName
    } else if lower.contains("schema") || lower.contains("epoch") {
        RenameProtectedValueCategoryV1::SchemaEpoch
    } else if lower.contains("sha256") || lower.contains("digest") || lower.contains("domain") {
        RenameProtectedValueCategoryV1::HashDomain
    } else if lower.contains("protocol") || lower.contains("jsonrpc") {
        RenameProtectedValueCategoryV1::ProtocolName
    } else if lower.contains("snapshot") || lower.contains("contract") {
        RenameProtectedValueCategoryV1::ContractSnapshot
    } else if line.contains("b\"") || line.contains("br\"") {
        RenameProtectedValueCategoryV1::ByteLiteral
    } else {
        RenameProtectedValueCategoryV1::StringLiteral
    }
}

fn site_id(file: &str, start: usize, end: usize, kind: RenameSiteKindV1) -> Result<String> {
    canonical_sha256(&("tracedecay.rename-site.v1", file, start, end, kind))
        .map(|digest| {
            format!(
                "site.rename.{}",
                digest.as_str().trim_start_matches("sha256:")
            )
        })
        .map_err(|error| TraceDecayError::Config {
            message: format!("cannot derive rename site identity: {error}"),
        })
}

fn preview_id(binding: &RenameSymbolBindingV1, new_name: &str) -> Result<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.rename-preview-identity.v1",
        &binding.node_id,
        &binding.qualified_name,
        &binding.kind,
        &binding.file,
        &binding.old_name,
        new_name,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("cannot derive rename preview identity: {error}"),
    })
}

fn repository_revision(project_root: &Path) -> Result<Option<String>> {
    if !project_root.join(".git").try_exists()? {
        return Ok(None);
    }
    let repository = gix::open(project_root).map_err(|error| TraceDecayError::Config {
        message: format!("cannot open the rename repository: {error}"),
    })?;
    let commit = repository
        .head_commit()
        .map_err(|error| TraceDecayError::Config {
            message: format!("cannot resolve the rename repository HEAD: {error}"),
        })?;
    Ok(Some(commit.id().to_hex().to_string()))
}

fn disposition_counts(sites: &[RenameSiteV1]) -> RenameDispositionCountsV1 {
    let mut counts = RenameDispositionCountsV1::default();
    for site in sites {
        match site.disposition {
            RenameSiteDispositionV1::Changed => counts.changed += 1,
            RenameSiteDispositionV1::Unchanged => counts.unchanged += 1,
            RenameSiteDispositionV1::Skipped => counts.skipped += 1,
            RenameSiteDispositionV1::Blocked => counts.blocked += 1,
        }
    }
    counts
}

struct PlannedRenameFile {
    relative_path: String,
    original: String,
    modified: String,
    readonly: bool,
    unix_mode: Option<u32>,
    replaced_count: usize,
}

#[derive(Clone)]
struct ResolvedRenameSite {
    start: usize,
    end: usize,
    source_node_id: String,
    source_qualified_name: String,
    kind: RenameSiteKindV1,
    apply_grade: bool,
}

enum EvidenceResolution {
    Exact(ResolvedRenameSite),
    Missing,
    Ambiguous,
}

fn resolve_graph_site(
    source: &str,
    old_name_ranges: &[(usize, usize)],
    old_name: &str,
    evidence: &RenameGraphSiteV1,
) -> EvidenceResolution {
    let ranges = if let Some(span) = evidence.evidence_span {
        let Ok(start) = usize::try_from(span.start_byte) else {
            return EvidenceResolution::Missing;
        };
        let Ok(end) = usize::try_from(span.end_byte) else {
            return EvidenceResolution::Missing;
        };
        if start > end
            || end > source.len()
            || !source.is_char_boundary(start)
            || !source.is_char_boundary(end)
        {
            return EvidenceResolution::Missing;
        }
        old_name_ranges
            .iter()
            .copied()
            .filter(|(candidate_start, candidate_end)| {
                *candidate_start >= start && *candidate_end <= end
            })
            .collect::<Vec<_>>()
    } else {
        let lines = source_lines(source);
        let Some((line_offset, line)) = lines.get(evidence.line as usize).copied() else {
            return EvidenceResolution::Missing;
        };
        let column = evidence.column as usize;
        let mut ranges = identifier_ranges(line, old_name)
            .into_iter()
            .filter(|(start, _)| {
                !string_literal_at(line, *start, &evidence.file) && !comment_at(line, *start)
            })
            .map(|(start, end)| (line_offset + start, line_offset + end))
            .collect::<Vec<_>>();
        if let Some(exact) = ranges.iter().copied().find(|(start, end)| {
            column >= start.saturating_sub(line_offset) && column < end.saturating_sub(line_offset)
        }) {
            ranges = vec![exact];
        }
        ranges
    };
    let [(start, end)] = ranges.as_slice() else {
        return if ranges.is_empty() {
            EvidenceResolution::Missing
        } else {
            EvidenceResolution::Ambiguous
        };
    };
    let Some(line) = source_lines(source)
        .into_iter()
        .find(|(line_start, line)| *start >= *line_start && *start <= *line_start + line.len())
        .map(|(_, line)| line)
    else {
        return EvidenceResolution::Missing;
    };
    let kind = match (evidence.declaration_kind, evidence.relation_kind) {
        (Some(kind), _) => kind,
        (None, Some(edge)) => relation_kind(edge, line, &evidence.file),
        (None, None) => return EvidenceResolution::Missing,
    };
    EvidenceResolution::Exact(ResolvedRenameSite {
        start: *start,
        end: *end,
        source_node_id: evidence.source_occurrence.clone(),
        source_qualified_name: evidence.source_qualified_name.clone(),
        kind,
        apply_grade: evidence.apply_grade,
    })
}

impl TraceDecay {
    pub(crate) async fn rename_symbol(
        &self,
        graph: SourceEditGraphReadV1,
        binding: &RenameSymbolBindingV1,
        new_name: &str,
        dry_run: bool,
    ) -> Result<RenameResult> {
        let identity = preview_id(binding, new_name)?;
        let refused = |message: String, kind: RenameHazardKindV1| RenameResult {
            success: false,
            preview_id: Some(identity.clone()),
            preview_digest: None,
            plan_digest: None,
            repository_revision: None,
            graph_revision: None,
            symbol: binding.qualified_name.clone(),
            old_name: binding.old_name.clone(),
            new_name: new_name.to_owned(),
            files: Vec::new(),
            reference_count: 0,
            sites: Vec::new(),
            dispositions: RenameDispositionCountsV1::default(),
            hazards: vec![RenameHazardV1 {
                kind,
                blocking: true,
                message: message.clone(),
                site_id: None,
            }],
            protected_values: Vec::new(),
            impact: RenameImpactV1::default(),
            dry_run,
            rolled_back: false,
            diff: None,
            message,
        };

        if !is_valid_identifier(new_name, &binding.file)
            || !is_valid_identifier(&binding.old_name, &binding.file)
        {
            return Ok(refused(
                "rename requires valid old and new identifiers".to_owned(),
                RenameHazardKindV1::InvalidIdentifier,
            ));
        }
        if new_name == binding.old_name {
            return Ok(refused(
                "new name is identical to the bound old name".to_owned(),
                RenameHazardKindV1::InvalidIdentifier,
            ));
        }
        let evidence = match graph_evidence::load(&graph, binding)? {
            RenameGraphEvidenceLoadV1::Ready(evidence) => evidence,
            RenameGraphEvidenceLoadV1::Refused { message, kind } => {
                return Ok(refused(message, kind));
            }
        };
        let graph_revision = evidence.graph_revision;
        let repository_revision = repository_revision(&self.project_root)?;
        let callers = evidence.callers;
        let mut reexports = BTreeSet::new();
        let mut affected_tests = evidence.affected_tests;
        let reference_count = evidence.reference_count;
        let mut target_sites = BTreeMap::<String, Vec<RenameGraphSiteV1>>::new();
        for site in evidence.target_sites {
            target_sites
                .entry(site.file.clone())
                .or_default()
                .push(site);
        }
        let mut other_sites = BTreeMap::<String, Vec<RenameGraphSiteV1>>::new();
        for site in evidence.other_sites {
            other_sites.entry(site.file.clone()).or_default().push(site);
        }

        let mut planned = Vec::new();
        let mut sites = Vec::new();
        let mut hazards = Vec::new();
        let mut protected_values = Vec::new();
        let mut affected_files = BTreeSet::new();
        for record in evidence.files {
            if record.disposition != SnapshotFileDispositionV1::Present {
                continue;
            }
            ensure_active(&graph)?;
            let relative_path = record.path;
            let authority =
                SourceEditFileAuthority::open(&self.project_root, Path::new(&relative_path))?;
            let (source, _) = authority.read_to_string(&relative_path)?;
            let metadata = authority.metadata()?;
            let readonly = metadata.permissions().readonly();
            #[cfg(unix)]
            let unix_mode = {
                use cap_std::fs::PermissionsExt;
                Some(metadata.permissions().mode())
            };
            #[cfg(not(unix))]
            let unix_mode = None;
            if ContentDigest::of_bytes(source.as_bytes()) != record.content_digest {
                hazards.push(RenameHazardV1 {
                    kind: RenameHazardKindV1::StaleEvidence,
                    blocking: true,
                    message: format!(
                        "{relative_path} no longer matches the admitted graph generation"
                    ),
                    site_id: None,
                });
            }
            if !source.contains(&binding.old_name)
                && !target_sites.contains_key(&relative_path)
                && !other_sites.contains_key(&relative_path)
            {
                continue;
            }
            let lines = source_lines(&source);
            let old_name_ranges = identifier_ranges(&source, &binding.old_name);
            let mut exact_target = BTreeMap::<(usize, usize), ResolvedRenameSite>::new();
            let mut exact_other = BTreeMap::<(usize, usize), ResolvedRenameSite>::new();
            for evidence in target_sites.remove(&relative_path).unwrap_or_default() {
                match resolve_graph_site(&source, &old_name_ranges, &binding.old_name, &evidence) {
                    EvidenceResolution::Exact(site) => {
                        if site.kind == RenameSiteKindV1::Reexport {
                            reexports.insert(site.source_qualified_name.clone());
                        }
                        let range = (site.start, site.end);
                        if exact_target
                            .get(&range)
                            .is_some_and(|prior| prior.source_node_id != site.source_node_id)
                        {
                            hazards.push(RenameHazardV1 {
                                kind: RenameHazardKindV1::AmbiguousSymbol,
                                blocking: true,
                                message: format!(
                                    "several target evidence records resolve to one site in {relative_path}"
                                ),
                                site_id: None,
                            });
                        } else {
                            exact_target.entry(range).or_insert(site);
                        }
                    }
                    EvidenceResolution::Missing => {
                        hazards.push(RenameHazardV1 {
                            kind: RenameHazardKindV1::StaleEvidence,
                            blocking: true,
                            message: format!(
                                "target graph evidence no longer resolves in {relative_path}"
                            ),
                            site_id: None,
                        });
                    }
                    EvidenceResolution::Ambiguous => {
                        hazards.push(RenameHazardV1 {
                            kind: RenameHazardKindV1::AmbiguousSymbol,
                            blocking: true,
                            message: format!(
                                "target graph region contains several candidate occurrences in {relative_path}"
                            ),
                            site_id: None,
                        });
                    }
                }
            }
            for evidence in other_sites.remove(&relative_path).unwrap_or_default() {
                match resolve_graph_site(&source, &old_name_ranges, &binding.old_name, &evidence) {
                    EvidenceResolution::Exact(site) => {
                        let range = (site.start, site.end);
                        if exact_other
                            .get(&range)
                            .is_some_and(|prior| prior.source_node_id != site.source_node_id)
                        {
                            hazards.push(RenameHazardV1 {
                                kind: RenameHazardKindV1::AmbiguousSymbol,
                                blocking: true,
                                message: format!(
                                    "several same-name symbols claim one site in {relative_path}"
                                ),
                                site_id: None,
                            });
                        } else {
                            exact_other.entry(range).or_insert(site);
                        }
                    }
                    EvidenceResolution::Missing => {
                        hazards.push(RenameHazardV1 {
                            kind: RenameHazardKindV1::StaleEvidence,
                            blocking: true,
                            message: format!(
                                "same-name symbol evidence no longer resolves in {relative_path}"
                            ),
                            site_id: None,
                        });
                    }
                    EvidenceResolution::Ambiguous => {
                        hazards.push(RenameHazardV1 {
                            kind: RenameHazardKindV1::AmbiguousSymbol,
                            blocking: true,
                            message: format!(
                                "same-name graph region contains several candidate occurrences in {relative_path}"
                            ),
                            site_id: None,
                        });
                    }
                }
            }
            if exact_target
                .keys()
                .any(|range| exact_other.contains_key(range))
            {
                hazards.push(RenameHazardV1 {
                    kind: RenameHazardKindV1::AmbiguousSymbol,
                    blocking: true,
                    message: format!(
                        "target and same-name symbol evidence overlap in {relative_path}"
                    ),
                    site_id: None,
                });
            }
            if !exact_target.is_empty()
                && lines.iter().any(|(_, line)| {
                    identifier_ranges(line, new_name)
                        .into_iter()
                        .any(|(start, _)| {
                            !string_literal_at(line, start, &relative_path)
                                && !comment_at(line, start)
                        })
                })
            {
                for kind in [
                    RenameHazardKindV1::NamespaceCollision,
                    RenameHazardKindV1::Shadowing,
                    RenameHazardKindV1::ChangedResolution,
                ] {
                    hazards.push(RenameHazardV1 {
                        kind,
                        blocking: true,
                        message: format!(
                            "`{new_name}` already occurs in {relative_path}; collision, shadowing, or changed resolution is possible"
                        ),
                        site_id: None,
                    });
                }
            }

            let mut changed_ranges = Vec::new();
            for (line_index, (line_offset, line)) in lines.iter().copied().enumerate() {
                let ranges = identifier_ranges(line, &binding.old_name);
                for (line_start, line_end) in ranges {
                    let start = line_offset + line_start;
                    let end = line_offset + line_end;
                    let mut disposition = RenameSiteDispositionV1::Blocked;
                    let mut kind = RenameSiteKindV1::UnresolvedText;
                    let mut reason = "unresolved code spelling may bind this symbol".to_owned();
                    let mut source_node_id = None;
                    let bound = exact_target.get(&(start, end));
                    let other = exact_other.get(&(start, end));
                    if bound.is_some() && other.is_some() {
                        reason = "target and same-name graph evidence conflict at this occurrence"
                            .to_owned();
                    } else if let Some(bound) = bound {
                        kind = bound.kind;
                        source_node_id = Some(bound.source_node_id.clone());
                        if is_generated_path(&relative_path) {
                            reason = "required site is generated or vendored".to_owned();
                        } else if !bound.apply_grade {
                            reason = "graph edge authority is not safe for an apply-grade rename"
                                .to_owned();
                        } else if macro_syntax_at(line, line_start) {
                            reason = "required site is inside unsupported macro syntax".to_owned();
                        } else {
                            disposition = RenameSiteDispositionV1::Changed;
                            reason = "exact graph-bound occurrence".to_owned();
                            changed_ranges.push((start, end));
                        }
                    } else if let Some(other) = other {
                        kind = other.kind;
                        source_node_id = Some(other.source_node_id.clone());
                        if other.apply_grade {
                            disposition = RenameSiteDispositionV1::Unchanged;
                            reason =
                                "same spelling belongs to a different canonical symbol".to_owned();
                        } else {
                            reason = "same-name graph edge authority cannot safely exempt this occurrence"
                                .to_owned();
                        }
                    } else if string_literal_at(line, line_start, &relative_path) {
                        disposition = RenameSiteDispositionV1::Skipped;
                        kind = RenameSiteKindV1::ProtectedValue;
                        reason = "byte-exact string or wire value is protected".to_owned();
                    } else if comment_at(line, line_start) {
                        disposition = RenameSiteDispositionV1::Skipped;
                        kind = RenameSiteKindV1::Documentation;
                        reason = "text-only prose is never guessed as a reference".to_owned();
                    } else if is_generated_path(&relative_path) {
                        disposition = RenameSiteDispositionV1::Skipped;
                        reason = "unbound occurrence is generated or vendored".to_owned();
                    } else if macro_syntax_at(line, line_start) {
                        reason = "unbound occurrence is inside unsupported macro syntax".to_owned();
                    }
                    let id = site_id(&relative_path, start, end, kind)?;
                    if kind == RenameSiteKindV1::ProtectedValue {
                        protected_values.push(RenameProtectedValueV1 {
                            site_id: id.clone(),
                            file: relative_path.clone(),
                            start_byte: start as u64,
                            end_byte: end as u64,
                            category: protected_category(line),
                            exact_bytes: binding.old_name.clone(),
                        });
                    }
                    if disposition == RenameSiteDispositionV1::Blocked {
                        hazards.push(RenameHazardV1 {
                            kind: if bound.is_some() && other.is_some() {
                                RenameHazardKindV1::AmbiguousSymbol
                            } else if is_generated_path(&relative_path) {
                                RenameHazardKindV1::GeneratedSource
                            } else if bound.is_some_and(|site| !site.apply_grade)
                                || other.is_some_and(|site| !site.apply_grade)
                            {
                                RenameHazardKindV1::UnsupportedSyntax
                            } else if macro_syntax_at(line, line_start) {
                                RenameHazardKindV1::MacroExpansion
                            } else {
                                RenameHazardKindV1::AmbiguousSymbol
                            },
                            blocking: true,
                            message: reason.clone(),
                            site_id: Some(id.clone()),
                        });
                    }
                    sites.push(RenameSiteV1 {
                        site_id: id,
                        kind,
                        disposition,
                        file: relative_path.clone(),
                        line: line_index as u32 + 1,
                        start_byte: start as u64,
                        end_byte: end as u64,
                        expected_bytes: binding.old_name.clone(),
                        replacement_bytes: if disposition == RenameSiteDispositionV1::Changed {
                            new_name.to_owned()
                        } else {
                            binding.old_name.clone()
                        },
                        reason,
                        source_node_id,
                    });
                }
            }
            changed_ranges.sort_unstable_by(|left, right| right.0.cmp(&left.0));
            for pair in changed_ranges.windows(2) {
                if pair[0].0 < pair[1].1 {
                    hazards.push(RenameHazardV1 {
                        kind: RenameHazardKindV1::OverlappingSite,
                        blocking: true,
                        message: format!("overlapping sites in {relative_path}"),
                        site_id: None,
                    });
                }
            }
            let mut modified = source.clone();
            for (start, end) in &changed_ranges {
                modified.replace_range(*start..*end, new_name);
            }
            if !changed_ranges.is_empty() {
                affected_files.insert(relative_path.clone());
                if looks_like_test(&relative_path, None) {
                    affected_tests.insert(relative_path.clone());
                }
                planned.push(PlannedRenameFile {
                    relative_path,
                    original: source,
                    modified,
                    readonly,
                    unix_mode,
                    replaced_count: changed_ranges.len(),
                });
            }
        }
        for (path, evidence) in target_sites.into_iter().chain(other_sites) {
            hazards.push(RenameHazardV1 {
                kind: RenameHazardKindV1::StaleEvidence,
                blocking: true,
                message: format!("{} site(s) refer to missing file {path}", evidence.len()),
                site_id: None,
            });
        }

        planned.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        sites.sort_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then(left.start_byte.cmp(&right.start_byte))
                .then(left.site_id.cmp(&right.site_id))
        });
        hazards.sort_by(|left, right| {
            left.message
                .cmp(&right.message)
                .then(left.site_id.cmp(&right.site_id))
        });
        protected_values.sort_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then(left.start_byte.cmp(&right.start_byte))
                .then(left.site_id.cmp(&right.site_id))
        });
        let files = planned
            .iter()
            .map(|file| RenameFileEditV1 {
                file: file.relative_path.clone(),
                replaced_count: file.replaced_count,
            })
            .collect::<Vec<_>>();
        let dispositions = disposition_counts(&sites);
        let impact = RenameImpactV1 {
            callers: callers.into_iter().collect(),
            reexports: reexports.into_iter().collect(),
            affected_files: affected_files.into_iter().collect(),
            affected_tests: affected_tests.into_iter().collect(),
        };
        let plan_digest = canonical_sha256(&(
            "tracedecay.rename-plan.v1",
            &identity,
            &repository_revision,
            &graph_revision,
            &sites,
            &hazards,
            &protected_values,
            &impact,
            planned
                .iter()
                .map(|file| {
                    (
                        &file.relative_path,
                        &file.original,
                        &file.modified,
                        file.readonly,
                        file.unix_mode,
                    )
                })
                .collect::<Vec<_>>(),
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("cannot derive rename plan digest: {error}"),
        })?;
        let accepted = binding.accepted_preview.as_ref();
        let acceptance_matches = accepted.is_some_and(|accepted| {
            accepted.preview_id == identity
                && accepted.plan_digest == plan_digest
                && accepted.repository_revision == repository_revision
                && accepted.graph_revision == graph_revision
        });
        if (!dry_run && !acceptance_matches) || (dry_run && accepted.is_some()) {
            hazards.push(RenameHazardV1 {
                kind: RenameHazardKindV1::StaleEvidence,
                blocking: true,
                message: if dry_run {
                    "rename preview must not carry a prior preview acceptance".to_owned()
                } else {
                    "rename apply requires the exact accepted preview identity, plan, repository, and graph revisions"
                        .to_owned()
                },
                site_id: None,
            });
        }
        if hazards.iter().any(|hazard| hazard.blocking) {
            return Ok(RenameResult {
                success: false,
                preview_id: Some(identity),
                preview_digest: None,
                plan_digest: Some(plan_digest),
                repository_revision,
                graph_revision: Some(graph_revision),
                symbol: binding.qualified_name.clone(),
                old_name: binding.old_name.clone(),
                new_name: new_name.to_owned(),
                files,
                reference_count,
                sites,
                dispositions,
                hazards,
                protected_values,
                impact,
                dry_run,
                rolled_back: false,
                diff: None,
                message: "rename blocked by stale, ambiguous, unsupported, or colliding evidence"
                    .to_owned(),
            });
        }

        if dry_run {
            let mut diff = String::new();
            for file in &planned {
                capture_planned_source_edit(
                    &file.relative_path,
                    Some(&file.original),
                    Some(&file.modified),
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
                preview_id: Some(identity),
                preview_digest: None,
                plan_digest: Some(plan_digest),
                repository_revision,
                graph_revision: Some(graph_revision),
                symbol: binding.qualified_name.clone(),
                old_name: binding.old_name.clone(),
                new_name: new_name.to_owned(),
                files,
                reference_count,
                sites,
                dispositions,
                hazards,
                protected_values,
                impact,
                dry_run: true,
                rolled_back: false,
                diff: Some(diff),
                message: edit_success_message(true, "rename previewed"),
            });
        }

        let rollback_files = planned
            .iter()
            .map(|file| PlannedSourceEditFile {
                relative_path: file.relative_path.clone(),
                expected: Some(file.original.clone()),
                intended: Some(file.modified.clone()),
            })
            .collect::<Vec<_>>();
        let mut outcome = RenameResult {
            success: true,
            preview_id: Some(identity),
            preview_digest: None,
            plan_digest: Some(plan_digest),
            repository_revision,
            graph_revision: Some(graph_revision),
            symbol: binding.qualified_name.clone(),
            old_name: binding.old_name.clone(),
            new_name: new_name.to_owned(),
            files,
            reference_count,
            sites,
            dispositions,
            hazards,
            protected_values,
            impact,
            dry_run: false,
            rolled_back: false,
            diff: None,
            message: "rename applied".to_owned(),
        };
        ensure_active(&graph)?;
        match publish_planned_source_edit_manifest(&self.project_root, &rollback_files)? {
            SourceEditManifestPublishOutcome::Committed => {}
            SourceEditManifestPublishOutcome::Refused { message } => {
                outcome.success = false;
                outcome.hazards.push(RenameHazardV1 {
                    kind: RenameHazardKindV1::StaleEvidence,
                    blocking: true,
                    message: message.clone(),
                    site_id: None,
                });
                outcome.message = message;
                return Ok(outcome);
            }
            SourceEditManifestPublishOutcome::RolledBack { message } => {
                outcome.success = false;
                outcome.rolled_back = true;
                outcome.hazards.push(RenameHazardV1 {
                    kind: RenameHazardKindV1::StaleEvidence,
                    blocking: true,
                    message: message.clone(),
                    site_id: None,
                });
                outcome.message = message;
                return Ok(outcome);
            }
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_application::source_edit::{
        RenameProtectedValueCategoryV1, RenameSiteDispositionV1,
    };
    use tracedecay_domain::{RelationEdgeKindV1, SourceSpan};

    use super::{
        EvidenceResolution, RenameGraphSiteV1, identifier_ranges, is_valid_identifier,
        protected_category, resolve_graph_site, string_literal_at,
    };

    #[test]
    fn identifier_matching_is_whole_word_and_unicode_safe() {
        assert_eq!(identifier_ranges("foo(foo_bar, foo)", "foo").len(), 2);
        assert_eq!(identifier_ranges("préfoo foo", "foo").len(), 1);
    }

    #[test]
    fn invalid_identifiers_and_protected_literals_fail_closed() {
        assert!(is_valid_identifier("renamed_symbol", "src/model.rs"));
        assert!(is_valid_identifier("$renamed", "src/model.ts"));
        assert!(!is_valid_identifier("$renamed", "src/model.rs"));
        assert!(!is_valid_identifier("1abc", "src/model.rs"));
        assert!(!is_valid_identifier("rename😀", "src/model.rs"));
        let line = "#[serde(rename = \"old_name\")]";
        let start = line.find("old_name").unwrap();
        assert!(string_literal_at(line, start, "src/model.rs"));
        let typescript = "const wire = 'old_name'; const template = `old_name`;";
        for start in typescript.match_indices("old_name").map(|(start, _)| start) {
            assert!(string_literal_at(typescript, start, "src/model.ts"));
        }
        let rust_lifetime = "fn borrow<'old_name>(value: &'old_name str) {}";
        for start in rust_lifetime
            .match_indices("old_name")
            .map(|(start, _)| start)
        {
            assert!(!string_literal_at(rust_lifetime, start, "src/model.rs"));
        }
        assert_eq!(
            protected_category(line),
            RenameProtectedValueCategoryV1::SerializedName
        );
        assert_eq!(
            protected_category("const WIRE_VALUE: &str = \"old_name\";"),
            RenameProtectedValueCategoryV1::WireValue
        );
        assert_eq!(
            protected_category("const STORE_KEY: &str = \"old_name\";"),
            RenameProtectedValueCategoryV1::PersistedName
        );
        assert_ne!(
            RenameSiteDispositionV1::Skipped,
            RenameSiteDispositionV1::Changed
        );
    }

    #[test]
    fn graph_region_must_resolve_one_exact_identifier() {
        let source = "fn caller() { old(); old(); }";
        let ranges = identifier_ranges(source, "old");
        let evidence = |end_byte| RenameGraphSiteV1 {
            file: "src/lib.rs".to_owned(),
            evidence_span: Some(SourceSpan {
                start_byte: 14,
                end_byte,
            }),
            line: 0,
            column: 0,
            source_occurrence: "occurrence:caller".to_owned(),
            source_qualified_name: "caller".to_owned(),
            declaration_kind: None,
            relation_kind: Some(RelationEdgeKindV1::Calls),
            apply_grade: true,
        };
        assert!(matches!(
            resolve_graph_site(source, &ranges, "old", &evidence(19)),
            EvidenceResolution::Exact(_)
        ));
        assert!(matches!(
            resolve_graph_site(source, &ranges, "old", &evidence(source.len() as u64)),
            EvidenceResolution::Ambiguous
        ));
    }
}
