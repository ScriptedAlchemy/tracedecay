use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use tracedecay_code_extraction::{ExtractionArtifactV1, LanguageExtractor, RustExtractor};
use tracedecay_contracts::retrieval::{
    GitContextSymbolV1, IncompatibleCallSiteV1, SignatureEditStatusV1, SignatureEditV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, EdgeKind, Node, RelationEdgeKindV1, SourceSpan, UnresolvedRef,
};
use tracedecay_runtime_core::git::{GitCommandBounds, bounded_git_output};

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = bounded_git_output(root, args, &GitCommandBounds::default()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("signature baseline: {error}"),
        }
    })?;
    if !output.status.success() {
        return Err(TraceDecayError::Config {
            message: format!(
                "signature baseline: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    Ok(output.stdout)
}

fn parse(
    extractor: &dyn LanguageExtractor,
    path: &str,
    source: &str,
) -> Result<ExtractionArtifactV1> {
    let artifact = extractor.extract_artifact(path, source);
    if !artifact.result.errors.is_empty() {
        return Err(TraceDecayError::Config {
            message: format!(
                "signature source {path}: {}",
                artifact.result.errors.join("; ")
            ),
        });
    }
    Ok(artifact)
}

fn current_node<'a>(
    artifact: &'a ExtractionArtifactV1,
    symbol: &GitContextSymbolV1,
) -> Result<&'a Node> {
    let mut matches = artifact.result.nodes.iter().filter(|node| {
        node.name == symbol.name
            && node.start_line == symbol.line
            && node.kind.as_str() == symbol.kind
    });
    match (matches.next(), matches.next()) {
        (Some(node), None) => Ok(node),
        _ => Err(TraceDecayError::Config {
            message: format!(
                "signature source no longer matches {}:{}",
                symbol.file, symbol.line
            ),
        }),
    }
}

/// The sealed call edge records the callee name token. A path call's
/// reference starts at `crate::...`, before that token.
fn reference_matches_span(source: &str, reference: &UnresolvedRef, span: SourceSpan) -> bool {
    let Some(site) = line_column_offset(source, reference.line, reference.column) else {
        return false;
    };
    let (Ok(start), Ok(end)) = (
        usize::try_from(span.start_byte),
        usize::try_from(span.end_byte),
    ) else {
        return false;
    };
    if site == start {
        return true;
    }
    if start < site || end > source.len() || start > end {
        return false;
    }
    let Some(name) = source.get(start..end) else {
        return false;
    };
    !name.is_empty()
        && reference.reference_name.ends_with(name)
        && source.get(site..end) == Some(reference.reference_name.as_str())
}

fn line_column_offset(source: &str, line: u32, column: u32) -> Option<usize> {
    source
        .split_inclusive('\n')
        .take(line as usize)
        .map(str::len)
        .sum::<usize>()
        .checked_add(column as usize)
}

pub(crate) fn signature_edits(
    root: &Path,
    modified: &[GitContextSymbolV1],
    symbols: &[GitContextSymbolV1],
    edges: &[CanonicalRelationEdgeV1],
) -> Result<Vec<SignatureEditV1>> {
    let by_id = symbols
        .iter()
        .map(|symbol| (symbol.id.as_str(), symbol))
        .collect::<HashMap<_, _>>();
    let mut sources = HashMap::new();
    for symbol in modified.iter().chain(symbols) {
        if sources.contains_key(&symbol.file) {
            continue;
        }
        if !symbol.file.ends_with(".rs") {
            continue;
        }
        let source = fs::read_to_string(root.join(&symbol.file))?;
        let artifact = parse(&RustExtractor, &symbol.file, &source)?;
        sources.insert(symbol.file.clone(), (source, artifact));
    }
    let mut baselines = HashMap::new();
    let mut baseline_commit = None;
    let mut edits = Vec::new();
    for symbol in modified {
        if symbol.kind != "function" {
            continue;
        }
        let Some((_, working)) = sources.get(&symbol.file) else {
            continue;
        };
        if working.callable_arities.is_empty() {
            continue;
        }
        let node = current_node(working, symbol)?;
        let Some(new) = working
            .callable_arities
            .iter()
            .find(|row| row.node_id == node.id)
        else {
            continue;
        };
        if !baselines.contains_key(&symbol.file) {
            if baseline_commit.is_none() {
                let oid = String::from_utf8(git_bytes(
                    root,
                    &["rev-parse", "--verify", "HEAD^{commit}"],
                )?)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("signature baseline commit: {error}"),
                })?;
                baseline_commit = Some(oid.trim().to_owned());
            }
            let commit = baseline_commit
                .as_deref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "signature baseline commit unavailable".to_owned(),
                })?;
            let listed = git_bytes(root, &["ls-tree", "-z", commit, "--", &symbol.file])?;
            let baseline = if listed.is_empty() {
                None
            } else {
                let source = String::from_utf8(git_bytes(
                    root,
                    &["show", &format!("{commit}:{}", symbol.file)],
                )?)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("signature baseline {}: {error}", symbol.file),
                })?;
                Some(parse(&RustExtractor, &symbol.file, &source)?)
            };
            baselines.insert(symbol.file.clone(), baseline);
        }
        let Some(Some(baseline)) = baselines.get(&symbol.file) else {
            continue;
        };
        let mut old_nodes = baseline
            .result
            .nodes
            .iter()
            .filter(|old| old.qualified_name == node.qualified_name && old.kind == node.kind);
        let Some(old_node) = old_nodes.next() else {
            continue;
        };
        if old_nodes.next().is_some() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "signature baseline is ambiguous for {}",
                    node.qualified_name
                ),
            });
        }
        let Some(old) = baseline
            .callable_arities
            .iter()
            .find(|row| row.node_id == old_node.id)
        else {
            continue;
        };
        if old.arity == new.arity {
            continue;
        }
        let mut incompatible = Vec::new();
        let mut sites = BTreeSet::new();
        for edge in edges.iter().filter(|edge| {
            edge.kind == RelationEdgeKindV1::Calls && edge.to_occurrence.as_str() == symbol.id
        }) {
            let caller = by_id.get(edge.from_occurrence.as_str()).ok_or_else(|| {
                TraceDecayError::Config {
                    message: "signature caller has no graph symbol".to_owned(),
                }
            })?;
            let Some((source, artifact)) = sources.get(&caller.file) else {
                continue;
            };
            let caller_node = current_node(artifact, caller)?;
            for reference in artifact.result.unresolved_refs.iter().filter(|reference| {
                reference.reference_kind == EdgeKind::Calls
                    && reference.from_node_id == caller_node.id
            }) {
                if !reference_matches_span(source, reference, edge.evidence_span) {
                    continue;
                }
                let Some(arguments) = reference.argument_count else {
                    continue;
                };
                if !new.arity.accepts(arguments)
                    && sites.insert((caller.id.clone(), reference.line, reference.column))
                {
                    incompatible.push(IncompatibleCallSiteV1 {
                        name: caller.name.clone(),
                        file: caller.file.clone(),
                        line: reference.line.checked_add(1).ok_or_else(|| {
                            TraceDecayError::Config {
                                message: "signature call line exceeds supported range".to_owned(),
                            }
                        })?,
                        arguments,
                    });
                }
            }
        }
        incompatible.sort_by(|left, right| {
            (&left.file, left.line, &left.name).cmp(&(&right.file, right.line, &right.name))
        });
        edits.push(SignatureEditV1 {
            symbol: symbol.name.clone(),
            file: symbol.file.clone(),
            status: SignatureEditStatusV1::ContractChange,
            old_parameters: Some(old.arity.parameters),
            new_parameters: Some(new.arity.parameters),
            incompatible,
        });
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::{reference_matches_span, signature_edits};
    use tracedecay_code_extraction::{LanguageExtractor, RustExtractor};
    use tracedecay_contracts::retrieval::GitContextSymbolV1;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_domain::{EdgeKind, SourceSpan};

    #[test]
    fn path_call_evidence_preserves_unicode_raw_names_and_exact_sites() {
        for path in ["crate::target", "crate::café", "crate::r#type"] {
            let source = format!(
                "fn cross_first() {{\n    {path}::remote_target((1, 2));\n    {path}::remote_target((3, 4), 5);\n}}\n"
            );
            let artifact = RustExtractor.extract_artifact("src/calls.rs", &source);
            let reference = artifact
                .result
                .unresolved_refs
                .iter()
                .find(|reference| {
                    reference.line == 1 && reference.reference_kind == EdgeKind::Calls
                })
                .expect("parser-owned first call");
            let first = source.find("remote_target").expect("first name");
            let second = source.rfind("remote_target").expect("second name");
            assert!(
                reference_matches_span(
                    &source,
                    reference,
                    SourceSpan {
                        start_byte: first as u64,
                        end_byte: (first + "remote_target".len()) as u64
                    }
                ),
                "{path}"
            );
            assert!(
                !reference_matches_span(
                    &source,
                    reference,
                    SourceSpan {
                        start_byte: second as u64,
                        end_byte: (second + "remote_target".len()) as u64
                    }
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn a_missing_known_source_refuses_signature_analysis() {
        for missing in ["lib.rs", "caller.rs"] {
            let root = tempfile::tempdir().unwrap();
            for (file, source) in [
                ("lib.rs", "fn target() {}\n"),
                ("caller.rs", "fn caller() { target(); }\n"),
            ] {
                if file != missing {
                    std::fs::write(root.path().join(file), source).unwrap();
                }
            }
            let target = GitContextSymbolV1 {
                id: "target".to_owned(),
                name: "target".to_owned(),
                kind: "function".to_owned(),
                file: "lib.rs".to_owned(),
                line: 0,
            };
            let caller = GitContextSymbolV1 {
                id: "caller".to_owned(),
                name: "caller".to_owned(),
                kind: "function".to_owned(),
                file: "caller.rs".to_owned(),
                line: 0,
            };
            let error = signature_edits(root.path(), &[target.clone()], &[target, caller], &[])
                .unwrap_err();
            assert!(
                matches!(error, TraceDecayError::Io(ref error) if error.kind() == std::io::ErrorKind::NotFound),
                "{missing}: {error}"
            );
        }
    }
}
