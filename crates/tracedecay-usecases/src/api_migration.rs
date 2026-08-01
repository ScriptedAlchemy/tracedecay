use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tracedecay_application::{
    ApiMigrationFilePlanV1, ApiMigrationOperationRequestV1, ApiMigrationPlanRequestV1,
    ApiMigrationPlanV1, ApiMigrationSiteDispositionV1, ApiMigrationSiteV1, ApiMigrationSymbolV1,
    api_migration_definition_digest, api_migration_file_digest,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use crate::tracedecay::TraceDecay;
use tracedecay_code_index::ast_grep_search::{AstGrepSearchMatch, search_tree};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::types::Node;

const MAX_API_MIGRATION_AST_MATCHES: usize = 16_384;

#[derive(Clone, Debug)]
struct PendingSite {
    operation_id: String,
    path: String,
    start: usize,
    end: usize,
    expected: String,
    replacement: String,
    disposition: ApiMigrationSiteDispositionV1,
    reason: String,
    caller_node_id: Option<String>,
}

struct PlanWorkspace<'a> {
    graph: &'a TraceDecay,
    sources: BTreeMap<String, String>,
    sites: Vec<PendingSite>,
    graph_evidence: Vec<(String, String, String, String)>,
}

pub async fn plan_api_migration(
    graph: &TraceDecay,
    request: ApiMigrationPlanRequestV1,
) -> Result<ApiMigrationPlanV1> {
    request.validate().map_err(contract_error)?;
    let repository_revision = current_repository_revision(graph.project_root())?;
    let mut workspace = PlanWorkspace {
        graph,
        sources: BTreeMap::new(),
        sites: Vec::new(),
        graph_evidence: Vec::new(),
    };

    for operation in &request.operations {
        match operation {
            ApiMigrationOperationRequestV1::PromotePrimary {
                operation_id,
                symbol,
                expected_definition_digest,
                replacement_definition,
                ..
            }
            | ApiMigrationOperationRequestV1::ReplaceDefinition {
                operation_id,
                symbol,
                expected_definition_digest,
                replacement_definition,
                ..
            } => {
                plan_definition_replacement(
                    &mut workspace,
                    operation_id,
                    symbol,
                    expected_definition_digest,
                    replacement_definition,
                )
                .await?;
            }
            ApiMigrationOperationRequestV1::RenameBoundSymbol {
                operation_id,
                symbol,
                new_name,
                ..
            } => {
                plan_bound_rename(&mut workspace, operation_id, symbol, new_name).await?;
            }
            ApiMigrationOperationRequestV1::InsertCompatibility {
                operation_id,
                anchor,
                position,
                definition,
                ..
            } => {
                let node = resolve_exact_symbol(workspace.graph, anchor).await?;
                workspace.graph_evidence.push(graph_tuple(&node));
                let source = source_for(workspace.graph, &node.file_path, &mut workspace.sources)?;
                let (start, end) = node_definition_span(source, &node)?;
                let offset = match position {
                    tracedecay_application::ApiDefinitionInsertionV1::Before => start,
                    tracedecay_application::ApiDefinitionInsertionV1::After => end,
                };
                let insertion = match position {
                    tracedecay_application::ApiDefinitionInsertionV1::Before => {
                        format!("{}\n", definition.trim_end())
                    }
                    tracedecay_application::ApiDefinitionInsertionV1::After => {
                        format!("\n{}", definition.trim_end())
                    }
                };
                let already_satisfied = match position {
                    tracedecay_application::ApiDefinitionInsertionV1::Before => source[..offset]
                        .strip_suffix(&insertion)
                        .map(|_| (offset - insertion.len(), offset)),
                    tracedecay_application::ApiDefinitionInsertionV1::After => source[offset..]
                        .starts_with(&insertion)
                        .then_some((offset, offset + insertion.len())),
                };
                if let Some((start, end)) = already_satisfied {
                    workspace.sites.push(PendingSite {
                        operation_id: operation_id.clone(),
                        path: node.file_path,
                        start,
                        end,
                        expected: insertion.clone(),
                        replacement: insertion,
                        disposition: ApiMigrationSiteDispositionV1::Unchanged,
                        reason: "compatibility definition already satisfies migration".to_owned(),
                        caller_node_id: None,
                    });
                } else {
                    workspace.sites.push(PendingSite {
                        operation_id: operation_id.clone(),
                        path: node.file_path,
                        start: offset,
                        end: offset,
                        expected: String::new(),
                        replacement: insertion,
                        disposition: ApiMigrationSiteDispositionV1::Changed,
                        reason: "deliberate compatibility definition".to_owned(),
                        caller_node_id: None,
                    });
                }
            }
            ApiMigrationOperationRequestV1::ReplaceSelectedTerminology {
                operation_id,
                enclosing_symbol,
                old_term,
                new_term,
                occurrence_indexes,
                ..
            } => {
                plan_selected_ast_occurrences(
                    &mut workspace,
                    operation_id,
                    enclosing_symbol,
                    old_term,
                    Some(new_term),
                    occurrence_indexes,
                    "selected AST terminology replacement",
                )
                .await?;
            }
            ApiMigrationOperationRequestV1::AssertStableValue {
                operation_id,
                enclosing_symbol,
                category,
                exact_bytes,
                occurrence_indexes,
                ..
            } => {
                plan_selected_ast_occurrences(
                    &mut workspace,
                    operation_id,
                    enclosing_symbol,
                    exact_bytes,
                    None,
                    occurrence_indexes,
                    &format!("protected {category} remains byte-identical"),
                )
                .await?;
            }
        }
    }

    let PlanWorkspace {
        graph: _,
        sources,
        mut sites,
        mut graph_evidence,
    } = workspace;
    block_overlapping_sites(&mut sites);
    block_protected_value_changes(&request.operations, &sources, &mut sites)?;
    let files = materialize_file_plans(&sources, &sites)?;
    let blocked = sites
        .iter()
        .any(|site| site.disposition == ApiMigrationSiteDispositionV1::Blocked);
    let sites = sites
        .into_iter()
        .map(finalize_site)
        .collect::<Result<Vec<_>>>()?;
    graph_evidence.sort();
    graph_evidence.dedup();
    let graph_revision = canonical_sha256(&(
        "tracedecay.api-migration.graph-evidence.v1",
        &repository_revision,
        &graph_evidence,
    ))
    .map_err(domain_error)?;
    let mut plan = ApiMigrationPlanV1 {
        family_id: request.family_id,
        repository_revision,
        graph_revision,
        operations: request.operations,
        sites,
        files,
        blocked,
        plan_digest: api_migration_file_digest("pending").map_err(contract_error)?,
    };
    plan.plan_digest = plan.compute_digest().map_err(contract_error)?;
    plan.validate().map_err(contract_error)?;
    Ok(plan)
}

async fn plan_definition_replacement(
    workspace: &mut PlanWorkspace<'_>,
    operation_id: &str,
    identity: &ApiMigrationSymbolV1,
    expected_definition_digest: &ManifestDigest,
    replacement: &str,
) -> Result<()> {
    let PlanWorkspace {
        graph,
        sources,
        sites,
        graph_evidence,
    } = workspace;
    let node = resolve_exact_symbol(graph, identity).await?;
    graph_evidence.push(graph_tuple(&node));
    let source = source_for(graph, &node.file_path, sources)?;
    let (start, end) = node_definition_span(source, &node)?;
    let expected = source[start..end].to_owned();
    let observed_digest = api_migration_definition_digest(&expected).map_err(contract_error)?;
    let (disposition, reason) = if observed_digest == *expected_definition_digest {
        (
            ApiMigrationSiteDispositionV1::Changed,
            "whole definition replacement",
        )
    } else if expected.trim() == replacement.trim() {
        (
            ApiMigrationSiteDispositionV1::Unchanged,
            "definition already satisfies migration",
        )
    } else {
        (
            ApiMigrationSiteDispositionV1::Blocked,
            "definition digest is stale",
        )
    };
    sites.push(PendingSite {
        operation_id: operation_id.to_owned(),
        path: node.file_path,
        start,
        end,
        replacement: if disposition == ApiMigrationSiteDispositionV1::Unchanged {
            expected.clone()
        } else {
            replacement.to_owned()
        },
        expected,
        disposition,
        reason: reason.to_owned(),
        caller_node_id: None,
    });
    Ok(())
}

async fn plan_bound_rename(
    workspace: &mut PlanWorkspace<'_>,
    operation_id: &str,
    identity: &ApiMigrationSymbolV1,
    new_name: &str,
) -> Result<()> {
    let PlanWorkspace {
        graph,
        sources,
        sites,
        graph_evidence,
    } = workspace;
    let node = match resolve_exact_symbol(graph, identity).await {
        Ok(node) => node,
        Err(_) => {
            let new_qualified_name = identity
                .qualified_name
                .strip_suffix(&identity.old_name)
                .map_or_else(
                    || new_name.to_owned(),
                    |prefix| format!("{prefix}{new_name}"),
                );
            let already = graph
                .get_nodes_by_qualified_name(&new_qualified_name)
                .await?
                .into_iter()
                .find(|candidate| {
                    candidate.file_path == identity.file
                        && candidate.kind.as_str() == identity.kind
                        && candidate.name == new_name
                });
            if let Some(already) = already {
                graph_evidence.push(graph_tuple(&already));
                let source = source_for(graph, &already.file_path, sources)?;
                let (start, _) = node_definition_span(source, &already)?;
                sites.push(PendingSite {
                    operation_id: operation_id.to_owned(),
                    path: already.file_path,
                    start,
                    end: start,
                    expected: String::new(),
                    replacement: String::new(),
                    disposition: ApiMigrationSiteDispositionV1::Unchanged,
                    reason: "bound symbol rename already satisfied".to_owned(),
                    caller_node_id: None,
                });
                return Ok(());
            }
            return Err(config_error("API migration symbol identity is stale"));
        }
    };
    graph_evidence.push(graph_tuple(&node));
    let incoming = graph.get_incoming_edges(&node.id).await?;
    let mut expected_calls = BTreeMap::<(String, u32), BTreeSet<String>>::new();
    for edge in incoming {
        let Some(caller) = graph.get_node(&edge.source).await? else {
            continue;
        };
        graph_evidence.push(graph_tuple(&caller));
        if let Some(line) = edge.line {
            expected_calls
                .entry((caller.file_path.clone(), line))
                .or_default()
                .insert(caller.id);
        }
    }

    let matches = search_tree(
        graph.project_root(),
        &identity.old_name,
        None,
        None,
        MAX_API_MIGRATION_AST_MATCHES,
    )
    .map_err(|error| config_error(format!("AST rename planning failed: {error}")))?;
    if matches.truncated {
        return Err(config_error(
            "AST rename planning exceeded its bounded match budget",
        ));
    }
    let source = source_for(graph, &node.file_path, sources)?;
    let (definition_start, definition_end) = node_definition_span(source, &node)?;
    let declaration = matches
        .matches
        .iter()
        .filter(|matched| {
            matched.file == node.file_path
                && matched.start_byte >= definition_start
                && matched.end_byte <= definition_end
                && is_identifier_kind(&matched.node_kind)
                && matched.matched_text == identity.old_name
        })
        .min_by_key(|matched| matched.start_byte);
    match declaration {
        Some(matched) => push_ast_changed_site(
            operation_id,
            matched,
            new_name,
            "bound declaration rename",
            None,
            sites,
        ),
        None => sites.push(blocked_site(
            operation_id,
            &node.file_path,
            definition_start,
            "declaration AST identity was not found",
        )),
    }

    for ((path, line), caller_ids) in expected_calls {
        let candidates = matches
            .matches
            .iter()
            .filter(|matched| {
                matched.file == path
                    && matched.line.saturating_sub(1) == line
                    && is_identifier_kind(&matched.node_kind)
                    && matched.matched_text == identity.old_name
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            push_ast_changed_site(
                operation_id,
                candidates[0],
                new_name,
                "graph-bound caller rename",
                Some(caller_ids.iter().cloned().collect::<Vec<_>>().join(",")),
                sites,
            );
        } else {
            sites.push(blocked_site(
                operation_id,
                &path,
                0,
                if candidates.is_empty() {
                    "graph caller has no matching AST identifier"
                } else {
                    "graph caller line has ambiguous AST identifiers"
                },
            ));
        }
        source_for(graph, &path, sources)?;
    }
    Ok(())
}

async fn plan_selected_ast_occurrences(
    workspace: &mut PlanWorkspace<'_>,
    operation_id: &str,
    identity: &ApiMigrationSymbolV1,
    pattern: &str,
    replacement: Option<&str>,
    indexes: &[u32],
    reason: &str,
) -> Result<()> {
    let PlanWorkspace {
        graph,
        sources,
        sites,
        graph_evidence,
    } = workspace;
    let node = resolve_exact_symbol(graph, identity).await?;
    graph_evidence.push(graph_tuple(&node));
    let source = source_for(graph, &node.file_path, sources)?;
    let (start, end) = node_definition_span(source, &node)?;
    let result = search_tree(
        graph.project_root(),
        pattern,
        None,
        Some(&node.file_path),
        MAX_API_MIGRATION_AST_MATCHES,
    )
    .map_err(|error| config_error(format!("AST occurrence planning failed: {error}")))?;
    if result.truncated {
        return Err(config_error(
            "selected AST occurrence planning exceeded its bounded match budget",
        ));
    }
    let candidates = result
        .matches
        .iter()
        .filter(|matched| {
            matched.file == node.file_path
                && matched.start_byte >= start
                && matched.end_byte <= end
                && matched.matched_text == pattern
        })
        .collect::<Vec<_>>();
    if let Some(replacement) = replacement
        && candidates.is_empty()
    {
        let replacement_result = search_tree(
            graph.project_root(),
            replacement,
            None,
            Some(&node.file_path),
            MAX_API_MIGRATION_AST_MATCHES,
        )
        .map_err(|error| {
            config_error(format!("AST satisfied-occurrence planning failed: {error}"))
        })?;
        if replacement_result.truncated {
            return Err(config_error(
                "satisfied AST occurrence planning exceeded its bounded match budget",
            ));
        }
        let replacement_candidates = replacement_result
            .matches
            .iter()
            .filter(|matched| {
                matched.file == node.file_path
                    && matched.start_byte >= start
                    && matched.end_byte <= end
                    && matched.matched_text == replacement
            })
            .collect::<Vec<_>>();
        for index in indexes {
            let Some(matched) = replacement_candidates.get(*index as usize) else {
                sites.push(blocked_site(
                    operation_id,
                    &node.file_path,
                    start,
                    "selected AST occurrence index is stale",
                ));
                continue;
            };
            sites.push(PendingSite {
                operation_id: operation_id.to_owned(),
                path: matched.file.clone(),
                start: matched.start_byte,
                end: matched.end_byte,
                expected: matched.matched_text.clone(),
                replacement: matched.matched_text.clone(),
                disposition: ApiMigrationSiteDispositionV1::Unchanged,
                reason: "selected AST terminology already satisfies migration".to_owned(),
                caller_node_id: Some(node.id.clone()),
            });
        }
        return Ok(());
    }
    for index in indexes {
        let Some(matched) = candidates.get(*index as usize) else {
            sites.push(blocked_site(
                operation_id,
                &node.file_path,
                start,
                "selected AST occurrence index is stale",
            ));
            continue;
        };
        if let Some(replacement) = replacement {
            push_ast_changed_site(
                operation_id,
                matched,
                replacement,
                reason,
                Some(node.id.clone()),
                sites,
            );
        } else {
            sites.push(PendingSite {
                operation_id: operation_id.to_owned(),
                path: matched.file.clone(),
                start: matched.start_byte,
                end: matched.end_byte,
                expected: matched.matched_text.clone(),
                replacement: matched.matched_text.clone(),
                disposition: ApiMigrationSiteDispositionV1::Skipped,
                reason: reason.to_owned(),
                caller_node_id: Some(node.id.clone()),
            });
        }
    }
    Ok(())
}

async fn resolve_exact_symbol(graph: &TraceDecay, expected: &ApiMigrationSymbolV1) -> Result<Node> {
    let Some(node) = graph.get_node(&expected.node_id).await? else {
        return Err(config_error("API migration symbol node no longer exists"));
    };
    if node.qualified_name != expected.qualified_name
        || node.kind.as_str() != expected.kind
        || node.file_path != expected.file
        || node.name != expected.old_name
    {
        return Err(config_error("API migration symbol identity changed"));
    }
    Ok(node)
}

fn source_for<'a>(
    graph: &TraceDecay,
    path: &str,
    sources: &'a mut BTreeMap<String, String>,
) -> Result<&'a str> {
    if !sources.contains_key(path) {
        let bytes =
            crate::tracedecay::read_source_edit_candidate(graph.project_root(), Path::new(path))?
                .ok_or_else(|| config_error(format!("API migration source is missing: {path}")))?;
        let source = String::from_utf8(bytes)
            .map_err(|_| config_error(format!("API migration source is not UTF-8: {path}")))?;
        sources.insert(path.to_owned(), source);
    }
    sources
        .get(path)
        .map(String::as_str)
        .ok_or_else(|| config_error(format!("API migration source missing after insert: {path}")))
}

fn node_definition_span(source: &str, node: &Node) -> Result<(usize, usize)> {
    let offsets = line_offsets(source);
    let start_line = node.attrs_start_line.min(node.start_line) as usize;
    let end_line = node.end_line as usize;
    let start = offsets
        .get(start_line)
        .copied()
        .ok_or_else(|| config_error("API migration symbol start is outside source"))?;
    let end = offsets
        .get(end_line.saturating_add(1))
        .copied()
        .unwrap_or(source.len());
    if start > end || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return Err(config_error("API migration symbol span is invalid"));
    }
    Ok((start, end))
}

fn line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    offsets.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    offsets
}

fn push_ast_changed_site(
    operation_id: &str,
    matched: &AstGrepSearchMatch,
    replacement: &str,
    reason: &str,
    caller_node_id: Option<String>,
    sites: &mut Vec<PendingSite>,
) {
    sites.push(PendingSite {
        operation_id: operation_id.to_owned(),
        path: matched.file.clone(),
        start: matched.start_byte,
        end: matched.end_byte,
        expected: matched.matched_text.clone(),
        replacement: replacement.to_owned(),
        disposition: ApiMigrationSiteDispositionV1::Changed,
        reason: reason.to_owned(),
        caller_node_id,
    });
}

fn blocked_site(operation_id: &str, path: &str, offset: usize, reason: &str) -> PendingSite {
    PendingSite {
        operation_id: operation_id.to_owned(),
        path: path.to_owned(),
        start: offset,
        end: offset,
        expected: String::new(),
        replacement: String::new(),
        disposition: ApiMigrationSiteDispositionV1::Blocked,
        reason: reason.to_owned(),
        caller_node_id: None,
    }
}

fn block_overlapping_sites(sites: &mut [PendingSite]) {
    let mut order = (0..sites.len())
        .filter(|index| sites[*index].disposition == ApiMigrationSiteDispositionV1::Changed)
        .collect::<Vec<_>>();
    order.sort_by(|left, right| {
        sites[*left]
            .path
            .cmp(&sites[*right].path)
            .then(sites[*left].start.cmp(&sites[*right].start))
            .then(sites[*left].end.cmp(&sites[*right].end))
    });
    let mut active = None;
    for index in order {
        let Some(previous) = active else {
            active = Some(index);
            continue;
        };
        if sites[previous].path != sites[index].path {
            active = Some(index);
            continue;
        }
        if sites[index].start < sites[previous].end
            || (sites[previous].start == sites[previous].end
                && sites[index].start == sites[index].end
                && sites[previous].start == sites[index].start)
        {
            sites[previous].disposition = ApiMigrationSiteDispositionV1::Blocked;
            "overlapping API migration sites".clone_into(&mut sites[previous].reason);
            sites[index].disposition = ApiMigrationSiteDispositionV1::Blocked;
            "overlapping API migration sites".clone_into(&mut sites[index].reason);
        }
        if sites[index].end > sites[previous].end {
            active = Some(index);
        }
    }
}

fn block_protected_value_changes(
    operations: &[ApiMigrationOperationRequestV1],
    sources: &BTreeMap<String, String>,
    sites: &mut [PendingSite],
) -> Result<()> {
    let predicted_files = materialize_file_plans(sources, sites)?;
    let protected_operations = operations
        .iter()
        .filter_map(|operation| match operation {
            ApiMigrationOperationRequestV1::AssertStableValue {
                operation_id,
                category,
                exact_bytes,
                ..
            } => Some((
                operation_id.as_str(),
                (category.as_str(), exact_bytes.as_str()),
            )),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut blocked = BTreeSet::new();
    for (protected_index, protected) in sites.iter().enumerate() {
        let Some((_category, exact_bytes)) =
            protected_operations.get(protected.operation_id.as_str())
        else {
            continue;
        };
        if protected.disposition == ApiMigrationSiteDispositionV1::Blocked {
            continue;
        }
        let predicted_preserves_value = predicted_files
            .iter()
            .find(|file| file.path == protected.path)
            .is_some_and(|file| file.intended_content.contains(*exact_bytes));
        let overlapping_changes = sites
            .iter()
            .enumerate()
            .filter(|(_, changed)| {
                changed.path == protected.path
                    && changed.disposition == ApiMigrationSiteDispositionV1::Changed
                    && changed.start < protected.end
                    && protected.start < changed.end
            })
            .collect::<Vec<_>>();
        let overlapping_changes_preserve_value = overlapping_changes
            .iter()
            .all(|(_, changed)| changed.replacement.contains(*exact_bytes));
        if !predicted_preserves_value || !overlapping_changes_preserve_value {
            blocked.insert(protected_index);
            blocked.extend(overlapping_changes.into_iter().map(|(index, _)| index));
        }
    }
    for index in blocked {
        sites[index].disposition = ApiMigrationSiteDispositionV1::Blocked;
        if let Some((category, _)) = protected_operations.get(sites[index].operation_id.as_str()) {
            sites[index].reason = format!("protected {category} would change byte identity");
        } else {
            "operation would change protected stable bytes".clone_into(&mut sites[index].reason);
        }
    }
    Ok(())
}

fn materialize_file_plans(
    sources: &BTreeMap<String, String>,
    sites: &[PendingSite],
) -> Result<Vec<ApiMigrationFilePlanV1>> {
    let mut files = Vec::with_capacity(sources.len());
    for (path, expected_content) in sources {
        let mut intended_content = expected_content.clone();
        let mut edits = sites
            .iter()
            .filter(|site| {
                site.path == *path && site.disposition == ApiMigrationSiteDispositionV1::Changed
            })
            .collect::<Vec<_>>();
        edits.sort_by(|left, right| right.start.cmp(&left.start).then(right.end.cmp(&left.end)));
        for edit in edits {
            if edit.end > intended_content.len()
                || !intended_content.is_char_boundary(edit.start)
                || !intended_content.is_char_boundary(edit.end)
                || intended_content[edit.start..edit.end] != edit.expected
            {
                return Err(config_error(
                    "API migration AST/code-index site no longer matches source",
                ));
            }
            intended_content.replace_range(edit.start..edit.end, &edit.replacement);
        }
        files.push(ApiMigrationFilePlanV1 {
            path: path.clone(),
            expected_digest: api_migration_file_digest(expected_content).map_err(contract_error)?,
            predicted_digest: api_migration_file_digest(&intended_content)
                .map_err(contract_error)?,
            expected_content: expected_content.clone(),
            intended_content,
        });
    }
    Ok(files)
}

fn finalize_site(site: PendingSite) -> Result<ApiMigrationSiteV1> {
    let digest = canonical_sha256(&(
        "tracedecay.api-migration.site.v1",
        &site.operation_id,
        &site.path,
        site.start,
        site.end,
        &site.expected,
        &site.replacement,
    ))
    .map_err(domain_error)?;
    Ok(ApiMigrationSiteV1 {
        site_id: format!(
            "site.api-migration.{}",
            digest.as_str().trim_start_matches("sha256:")
        ),
        operation_id: site.operation_id,
        path: site.path,
        start_byte: site.start as u64,
        end_byte: site.end as u64,
        expected_bytes: site.expected,
        replacement_bytes: site.replacement,
        disposition: site.disposition,
        reason: site.reason,
        caller_node_id: site.caller_node_id,
    })
}

fn current_repository_revision(project_root: &Path) -> Result<String> {
    let repository = gix::open(project_root)
        .map_err(|error| config_error(format!("cannot open repository: {error}")))?;
    repository
        .head_commit()
        .map(|commit| commit.id().to_hex().to_string())
        .map_err(|error| config_error(format!("cannot resolve repository HEAD: {error}")))
}

fn graph_tuple(node: &Node) -> (String, String, String, String) {
    (
        node.id.clone(),
        node.qualified_name.clone(),
        node.kind.as_str().to_owned(),
        node.file_path.clone(),
    )
}

fn is_identifier_kind(kind: &str) -> bool {
    kind == "identifier" || kind.ends_with("_identifier")
}

fn contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    config_error(format!("API migration contract rejected input: {error}"))
}

fn domain_error(error: impl std::fmt::Display) -> TraceDecayError {
    config_error(format!("API migration digest failed: {error}"))
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_sites_fail_closed() {
        let mut sites = vec![
            PendingSite {
                operation_id: "one".to_owned(),
                path: "src/lib.rs".to_owned(),
                start: 1,
                end: 4,
                expected: "abc".to_owned(),
                replacement: "x".to_owned(),
                disposition: ApiMigrationSiteDispositionV1::Changed,
                reason: "test".to_owned(),
                caller_node_id: None,
            },
            PendingSite {
                operation_id: "two".to_owned(),
                path: "src/lib.rs".to_owned(),
                start: 3,
                end: 5,
                expected: "cd".to_owned(),
                replacement: "y".to_owned(),
                disposition: ApiMigrationSiteDispositionV1::Changed,
                reason: "test".to_owned(),
                caller_node_id: None,
            },
        ];
        block_overlapping_sites(&mut sites);
        assert!(
            sites
                .iter()
                .all(|site| { site.disposition == ApiMigrationSiteDispositionV1::Blocked })
        );
    }
}
