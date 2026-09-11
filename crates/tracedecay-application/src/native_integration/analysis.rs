use std::collections::{BTreeMap, BTreeSet};

use tracedecay_code_extraction::{
    ExtractedSchemaEvidenceV1, ExtractedSchemaFactV1, SchemaEvidenceIssueV1, SchemaEvidenceStatusV1,
};
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_code_index::provider::GenerationTestAttributionJoinReadPort;
use tracedecay_code_index::test_attribution::GenerationTestJoinCoverageV1;
use tracedecay_contracts::{CancellationSignal, Deadline, NativeIntegrationPortError};
use tracedecay_domain::{
    GitOidV1, ManifestDigest, NativeIntegrationAnalysisAnchorV1,
    NativeIntegrationAnalysisCoverageV1, NativeIntegrationAnalysisGapV1,
    NativeIntegrationAnalysisLaneV1, NativeIntegrationAnalysisReportV1,
    NativeIntegrationGenerationBindingV1, NativeIntegrationSelectionV1,
    NativeIntegrationSemanticConflictKindV1, NativeIntegrationSemanticConflictV1,
    SymbolIdentityDigest, SymbolOccurrenceId,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git_repository::{GitNativeCandidateTreeV1, GitNativePreflight};

use super::NativeIntegrationAnalysisRevalidationV1;

/// Daemon-owned semantic authority over one exact native candidate tree.
pub trait NativeIntegrationAnalysisPort: Send + Sync {
    fn analyze(
        &self,
        selection: &NativeIntegrationSelectionV1,
        native: &GitNativePreflight,
        candidate: &GitNativeCandidateTreeV1<'_>,
        deadline: &Deadline,
        cancellation_signal: &CancellationSignal,
        cancellation: &CancellationToken,
    ) -> Result<NativeIntegrationAnalysisReportV1, NativeIntegrationPortError>;

    fn revalidate(
        &self,
        report: &NativeIntegrationAnalysisReportV1,
        deadline: &Deadline,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationAnalysisRevalidationV1, NativeIntegrationPortError>;
}

pub struct NativeIntegrationGenerationAnalysisV1 {
    pub graph: NativeIntegrationAnalysisLaneV1,
    pub tests: NativeIntegrationAnalysisLaneV1,
    pub schema: NativeIntegrationAnalysisLaneV1,
    pub migrations: NativeIntegrationAnalysisLaneV1,
    pub conflicts: Vec<NativeIntegrationSemanticConflictV1>,
}

pub fn native_integration_generation_binding(
    generation: &CodeIndexPublishedGenerationV1,
    source_tree: GitOidV1,
) -> Result<NativeIntegrationGenerationBindingV1, tracedecay_domain::DomainError> {
    let snapshot = generation.snapshot();
    Ok(NativeIntegrationGenerationBindingV1 {
        generation_id: generation.manifest().generation_id.clone(),
        project_id: generation.manifest().project_id.clone(),
        repository_id: snapshot.repository.clone(),
        worktree_id: snapshot.worktree.clone(),
        reference: snapshot.reference.clone(),
        snapshot_digest: generation.manifest().snapshot_digest.clone(),
        content_identity: snapshot.content_identity.clone(),
        source_revision: snapshot
            .source_revision
            .as_ref()
            .map(|revision| GitOidV1::new(revision.as_str().to_owned()))
            .transpose()?,
        source_tree,
        seal_digest: generation.manifest().seal.expected_digest.clone(),
    })
}

struct GenerationView<'a> {
    generation: &'a CodeIndexPublishedGenerationV1,
    by_identity:
        BTreeMap<SymbolIdentityDigest, &'a tracedecay_code_index::lineage::LineageSymbolRecordV1>,
    identity_by_occurrence: BTreeMap<SymbolOccurrenceId, SymbolIdentityDigest>,
    anchor_by_occurrence: BTreeMap<SymbolOccurrenceId, (&'a str, tracedecay_domain::SourceSpan)>,
    related_identities: BTreeMap<SymbolIdentityDigest, BTreeSet<SymbolIdentityDigest>>,
}

impl<'a> GenerationView<'a> {
    fn new(generation: &'a CodeIndexPublishedGenerationV1) -> Self {
        let file_paths = generation
            .snapshot()
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, file.logical_path.as_str()))
            .collect::<BTreeMap<_, _>>();
        let mut anchor_by_occurrence = BTreeMap::new();
        for chunk in generation.chunks().chunks() {
            if let (Some(occurrence), Some(path)) = (
                chunk.anchor.symbol_occurrence_id.as_ref(),
                file_paths.get(&chunk.anchor.file_occurrence_id),
            ) {
                anchor_by_occurrence
                    .entry(occurrence.clone())
                    .or_insert((*path, chunk.anchor.source_span));
            }
        }
        let identity_by_occurrence = generation
            .symbols()
            .symbols
            .iter()
            .map(|symbol| (symbol.occurrence.clone(), symbol.identity.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut related_identities = BTreeMap::<_, BTreeSet<_>>::new();
        for edge in generation.edges() {
            let (Some(left), Some(right)) = (
                identity_by_occurrence.get(&edge.from_occurrence),
                identity_by_occurrence.get(&edge.to_occurrence),
            ) else {
                continue;
            };
            related_identities
                .entry(left.clone())
                .or_default()
                .insert(right.clone());
            related_identities
                .entry(right.clone())
                .or_default()
                .insert(left.clone());
        }
        Self {
            generation,
            by_identity: generation
                .symbols()
                .symbols
                .iter()
                .map(|symbol| (symbol.identity.clone(), symbol.as_ref()))
                .collect(),
            identity_by_occurrence,
            anchor_by_occurrence,
            related_identities,
        }
    }

    fn anchor(&self, identity: &SymbolIdentityDigest) -> Option<NativeIntegrationAnalysisAnchorV1> {
        let symbol = self.by_identity.get(identity)?;
        let (path, source_span) = self.anchor_by_occurrence.get(&symbol.occurrence)?;
        Some(NativeIntegrationAnalysisAnchorV1 {
            generation_id: self.generation.manifest().generation_id.clone(),
            logical_path: (*path).to_owned(),
            source_span: *source_span,
        })
    }

    fn anchor_or_base(
        &self,
        base: &Self,
        identity: &SymbolIdentityDigest,
    ) -> Option<NativeIntegrationAnalysisAnchorV1> {
        self.anchor(identity).or_else(|| base.anchor(identity))
    }

    fn changed_from(&self, base: &Self) -> BTreeSet<SymbolIdentityDigest> {
        self.by_identity
            .keys()
            .chain(base.by_identity.keys())
            .filter(|identity| {
                let current = self.by_identity.get(*identity);
                let prior = base.by_identity.get(*identity);
                match (current, prior) {
                    (Some(current), Some(prior)) => {
                        current.content_digest != prior.content_digest
                            || current.signature != prior.signature
                    }
                    (None, None) => false,
                    _ => true,
                }
            })
            .cloned()
            .collect()
    }

    fn related_to(
        &self,
        identity: &SymbolIdentityDigest,
    ) -> impl Iterator<Item = &SymbolIdentityDigest> {
        self.related_identities
            .get(identity)
            .into_iter()
            .flat_map(BTreeSet::iter)
    }

    fn identities_in_paths(&self, paths: &BTreeSet<String>) -> BTreeSet<SymbolIdentityDigest> {
        self.by_identity
            .iter()
            .filter(|(_, symbol)| {
                self.anchor_by_occurrence
                    .get(&symbol.occurrence)
                    .is_some_and(|(path, _)| paths.contains(*path))
            })
            .map(|(identity, _)| identity.clone())
            .collect()
    }

    fn test_coverage_pairs(&self) -> Vec<(SymbolIdentityDigest, SymbolIdentityDigest)> {
        let Ok(authority) = self.generation.test_attribution_authority() else {
            return Vec::new();
        };
        let read = authority.read_test_attribution(&self.generation.manifest().generation_id);
        let Some(join) = read.evidence else {
            return Vec::new();
        };
        join.records
            .iter()
            .flat_map(|record| {
                let test = record
                    .test_occurrence
                    .as_ref()
                    .and_then(|occurrence| {
                        self.identity_by_occurrence.get(&occurrence.occurrence_id)
                    })
                    .cloned();
                record
                    .covered_occurrences
                    .iter()
                    .filter_map(move |covered| {
                        Some((
                            test.clone()?,
                            self.identity_by_occurrence
                                .get(&covered.occurrence_id)?
                                .clone(),
                        ))
                    })
            })
            .collect()
    }

    fn has_unresolved_required_edge(
        &self,
        changed_sources: &BTreeSet<SymbolIdentityDigest>,
        target_names: &BTreeSet<&str>,
    ) -> bool {
        self.generation
            .unresolved_references()
            .any(|(_, reference)| {
                self.identity_by_occurrence
                    .get(&reference.from_occurrence)
                    .is_some_and(|identity| changed_sources.contains(identity))
                    && target_names.iter().any(|name| {
                        reference.reference_name == *name
                            || reference.reference_name.ends_with(&format!("::{name}"))
                    })
            })
    }
}

fn lane(
    coverage: NativeIntegrationAnalysisCoverageV1,
    mut gaps: Vec<NativeIntegrationAnalysisGapV1>,
) -> NativeIntegrationAnalysisLaneV1 {
    gaps.sort();
    gaps.dedup();
    NativeIntegrationAnalysisLaneV1 { coverage, gaps }
}

fn test_lane(view: &GenerationView<'_>) -> NativeIntegrationAnalysisLaneV1 {
    let read = match view.generation.test_attribution_authority() {
        Ok(authority) => authority.read_test_attribution(&view.generation.manifest().generation_id),
        Err(_) => {
            return lane(
                NativeIntegrationAnalysisCoverageV1::Partial,
                vec![NativeIntegrationAnalysisGapV1::AuthorityUnavailable],
            );
        }
    };
    match read.evidence.as_ref().map(|evidence| &evidence.coverage) {
        Some(GenerationTestJoinCoverageV1::Complete) if read.coverage.is_complete() => {
            lane(NativeIntegrationAnalysisCoverageV1::Complete, Vec::new())
        }
        Some(GenerationTestJoinCoverageV1::Partial { .. })
            if read.evidence.as_ref().is_some_and(|evidence| {
                evidence.records.is_empty()
                    && !view
                        .generation
                        .snapshot()
                        .files
                        .iter()
                        .any(|file| tracedecay_code_index::is_test_file(&file.logical_path))
            }) =>
        {
            // The generation-bound producer inspected every retained file. A
            // repository with no test paths has no test root whose write/read
            // interaction could be hidden by unrelated graph abstentions.
            lane(NativeIntegrationAnalysisCoverageV1::Complete, Vec::new())
        }
        Some(_) => lane(
            NativeIntegrationAnalysisCoverageV1::Partial,
            vec![NativeIntegrationAnalysisGapV1::UnresolvedRequiredEdge],
        ),
        None => lane(
            NativeIntegrationAnalysisCoverageV1::Unsupported,
            vec![NativeIntegrationAnalysisGapV1::AuthorityUnavailable],
        ),
    }
}

fn changed_paths(base: &GenerationView<'_>, head: &GenerationView<'_>) -> BTreeSet<String> {
    let base_files = base
        .generation
        .snapshot()
        .files
        .iter()
        .map(|file| (file.logical_path.as_str(), &file.content_digest))
        .collect::<BTreeMap<_, _>>();
    let head_files = head
        .generation
        .snapshot()
        .files
        .iter()
        .map(|file| (file.logical_path.as_str(), &file.content_digest))
        .collect::<BTreeMap<_, _>>();
    base_files
        .keys()
        .chain(head_files.keys())
        .filter(|path| base_files.get(**path) != head_files.get(**path))
        .map(|path| (*path).to_owned())
        .collect()
}

fn schema_evidence_by_path<'a>(
    view: &'a GenerationView<'a>,
) -> BTreeMap<&'a str, &'a ExtractedSchemaEvidenceV1> {
    view.generation
        .schema_evidence()
        .map(|evidence| (evidence.logical_path.as_str(), evidence))
        .collect()
}

fn schema_lanes(
    base: &GenerationView<'_>,
    source: &GenerationView<'_>,
    destination: &GenerationView<'_>,
    candidate: &GenerationView<'_>,
) -> Result<
    (
        NativeIntegrationAnalysisLaneV1,
        NativeIntegrationAnalysisLaneV1,
        Vec<NativeIntegrationSemanticConflictV1>,
    ),
    tracedecay_domain::DomainError,
> {
    let paths = changed_paths(base, source)
        .into_iter()
        .chain(changed_paths(base, destination))
        .collect::<BTreeSet<_>>();
    let mut schema_paths = BTreeSet::new();
    let mut sql_paths = BTreeSet::new();
    for view in [base, source, destination, candidate] {
        for file in &view.generation.snapshot().files {
            if !paths.contains(&file.logical_path) {
                continue;
            }
            match file.language.as_ref().map(|language| language.as_str()) {
                Some("protobuf" | "proto") => {
                    schema_paths.insert(file.logical_path.as_str());
                }
                Some("sql") => {
                    schema_paths.insert(file.logical_path.as_str());
                    sql_paths.insert(file.logical_path.as_str());
                }
                _ => {}
            }
        }
        for evidence in view.generation.schema_evidence() {
            if paths.contains(&evidence.logical_path) {
                schema_paths.insert(evidence.logical_path.as_str());
                if evidence.language == tracedecay_code_extraction::SchemaEvidenceLanguageV1::Sql {
                    sql_paths.insert(evidence.logical_path.as_str());
                }
            }
        }
    }
    let base_evidence = schema_evidence_by_path(base);
    let source_evidence = schema_evidence_by_path(source);
    let destination_evidence = schema_evidence_by_path(destination);
    let candidate_evidence = schema_evidence_by_path(candidate);
    let mut gaps = Vec::new();
    for path in &schema_paths {
        for (view, evidence, missing_is_withheld) in [
            (base, base_evidence.get(path).copied(), false),
            (
                source,
                source_evidence.get(path).copied(),
                base_evidence.contains_key(path),
            ),
            (
                destination,
                destination_evidence.get(path).copied(),
                base_evidence.contains_key(path),
            ),
            (
                candidate,
                candidate_evidence.get(path).copied(),
                base_evidence.contains_key(path)
                    || source_evidence.contains_key(path)
                    || destination_evidence.contains_key(path),
            ),
        ] {
            let file = view
                .generation
                .snapshot()
                .files
                .iter()
                .find(|file| file.logical_path.as_str() == *path);
            let Some(evidence) = evidence else {
                use tracedecay_domain::SnapshotFileDispositionV1;
                if file.is_some_and(|file| {
                    matches!(
                        file.disposition,
                        SnapshotFileDispositionV1::Present | SnapshotFileDispositionV1::Renamed
                    )
                }) {
                    gaps.push(NativeIntegrationAnalysisGapV1::AuthorityUnavailable);
                } else if file.is_none() && missing_is_withheld {
                    gaps.push(NativeIntegrationAnalysisGapV1::WithheldSource);
                }
                continue;
            };
            match evidence.status {
                SchemaEvidenceStatusV1::Complete => {}
                SchemaEvidenceStatusV1::Partial => {
                    gaps.push(NativeIntegrationAnalysisGapV1::ParserFailure)
                }
                SchemaEvidenceStatusV1::Unsupported => {
                    gaps.push(NativeIntegrationAnalysisGapV1::DynamicSchema)
                }
            }
            for issue in &evidence.issues {
                gaps.push(match issue {
                    SchemaEvidenceIssueV1::DynamicIdentity
                    | SchemaEvidenceIssueV1::UnsupportedSyntax => {
                        NativeIntegrationAnalysisGapV1::DynamicSchema
                    }
                    SchemaEvidenceIssueV1::MigrationOrderUnknown => {
                        NativeIntegrationAnalysisGapV1::UnboundMigrationOrder
                    }
                    SchemaEvidenceIssueV1::ParseError => {
                        NativeIntegrationAnalysisGapV1::ParserFailure
                    }
                    SchemaEvidenceIssueV1::SourceTruncated => {
                        NativeIntegrationAnalysisGapV1::WithheldSource
                    }
                });
            }
        }
    }
    let schema = if gaps.is_empty() {
        lane(NativeIntegrationAnalysisCoverageV1::Complete, Vec::new())
    } else if gaps.contains(&NativeIntegrationAnalysisGapV1::DynamicSchema) {
        lane(NativeIntegrationAnalysisCoverageV1::Unsupported, gaps)
    } else {
        lane(NativeIntegrationAnalysisCoverageV1::Partial, gaps)
    };
    let migrations = if sql_paths.len() <= 1
        && schema.coverage == NativeIntegrationAnalysisCoverageV1::Complete
    {
        lane(NativeIntegrationAnalysisCoverageV1::Complete, Vec::new())
    } else {
        lane(
            NativeIntegrationAnalysisCoverageV1::Partial,
            vec![NativeIntegrationAnalysisGapV1::UnboundMigrationOrder],
        )
    };
    let conflicts = schema_conflicts(
        source,
        destination,
        &base_evidence,
        &source_evidence,
        &destination_evidence,
    )?;
    Ok((schema, migrations, conflicts))
}

fn schema_facts_overlap(left: &ExtractedSchemaFactV1, right: &ExtractedSchemaFactV1) -> bool {
    match (left, right) {
        (
            ExtractedSchemaFactV1::ProtobufMessage {
                qualified_name: left,
                ..
            },
            ExtractedSchemaFactV1::ProtobufMessage {
                qualified_name: right,
                ..
            },
        )
        | (
            ExtractedSchemaFactV1::ProtobufService {
                qualified_name: left,
                ..
            },
            ExtractedSchemaFactV1::ProtobufService {
                qualified_name: right,
                ..
            },
        ) => left == right,
        (
            ExtractedSchemaFactV1::ProtobufField {
                message_qualified_name: left_message,
                name: left_name,
                tag: left_tag,
                ..
            },
            ExtractedSchemaFactV1::ProtobufField {
                message_qualified_name: right_message,
                name: right_name,
                tag: right_tag,
                ..
            },
        ) => left_message == right_message && (left_name == right_name || left_tag == right_tag),
        (
            ExtractedSchemaFactV1::ProtobufRpc {
                service_qualified_name: left_service,
                name: left_name,
                ..
            },
            ExtractedSchemaFactV1::ProtobufRpc {
                service_qualified_name: right_service,
                name: right_name,
                ..
            },
        ) => left_service == right_service && left_name == right_name,
        (
            ExtractedSchemaFactV1::SqlObjectChange {
                object_kind: left_kind,
                qualified_name: left_name,
                ..
            },
            ExtractedSchemaFactV1::SqlObjectChange {
                object_kind: right_kind,
                qualified_name: right_name,
                ..
            },
        ) => left_kind == right_kind && left_name == right_name,
        _ => false,
    }
}

fn schema_facts_semantically_equal(
    left: &ExtractedSchemaFactV1,
    right: &ExtractedSchemaFactV1,
) -> bool {
    match (left, right) {
        (
            ExtractedSchemaFactV1::ProtobufMessage {
                qualified_name: left,
                ..
            },
            ExtractedSchemaFactV1::ProtobufMessage {
                qualified_name: right,
                ..
            },
        )
        | (
            ExtractedSchemaFactV1::ProtobufService {
                qualified_name: left,
                ..
            },
            ExtractedSchemaFactV1::ProtobufService {
                qualified_name: right,
                ..
            },
        ) => left == right,
        (
            ExtractedSchemaFactV1::ProtobufField {
                message_qualified_name: left_message,
                name: left_name,
                type_name: left_type,
                tag: left_tag,
                ..
            },
            ExtractedSchemaFactV1::ProtobufField {
                message_qualified_name: right_message,
                name: right_name,
                type_name: right_type,
                tag: right_tag,
                ..
            },
        ) => {
            (left_message, left_name, left_type, left_tag)
                == (right_message, right_name, right_type, right_tag)
        }
        (
            ExtractedSchemaFactV1::ProtobufRpc {
                service_qualified_name: left_service,
                name: left_name,
                request_type: left_request,
                response_type: left_response,
                ..
            },
            ExtractedSchemaFactV1::ProtobufRpc {
                service_qualified_name: right_service,
                name: right_name,
                request_type: right_request,
                response_type: right_response,
                ..
            },
        ) => {
            (left_service, left_name, left_request, left_response)
                == (right_service, right_name, right_request, right_response)
        }
        (
            ExtractedSchemaFactV1::SqlObjectChange {
                statement_order: left_order,
                action: left_action,
                object_kind: left_kind,
                qualified_name: left_name,
                ..
            },
            ExtractedSchemaFactV1::SqlObjectChange {
                statement_order: right_order,
                action: right_action,
                object_kind: right_kind,
                qualified_name: right_name,
                ..
            },
        ) => {
            (left_order, left_action, left_kind, left_name)
                == (right_order, right_action, right_kind, right_name)
        }
        _ => false,
    }
}

fn schema_conflicts(
    source: &GenerationView<'_>,
    destination: &GenerationView<'_>,
    base_evidence: &BTreeMap<&str, &ExtractedSchemaEvidenceV1>,
    source_evidence: &BTreeMap<&str, &ExtractedSchemaEvidenceV1>,
    destination_evidence: &BTreeMap<&str, &ExtractedSchemaEvidenceV1>,
) -> Result<Vec<NativeIntegrationSemanticConflictV1>, tracedecay_domain::DomainError> {
    let mut conflicts = Vec::new();
    let base_facts = base_evidence
        .values()
        .flat_map(|file| &file.facts)
        .collect::<Vec<_>>();
    for (source_path, source_file) in source_evidence {
        for source_fact in &source_file.facts {
            for (destination_path, destination_file) in destination_evidence {
                for destination_fact in &destination_file.facts {
                    if !schema_facts_overlap(source_fact, destination_fact)
                        || schema_facts_semantically_equal(source_fact, destination_fact)
                        || base_facts.iter().any(|base_fact| {
                            schema_facts_overlap(base_fact, source_fact)
                                && (schema_facts_semantically_equal(base_fact, source_fact)
                                    || schema_facts_semantically_equal(base_fact, destination_fact))
                        })
                    {
                        continue;
                    }
                    conflicts.push(
                        NativeIntegrationSemanticConflictV1 {
                            kind: if matches!(
                                source_fact,
                                ExtractedSchemaFactV1::SqlObjectChange { .. }
                            ) {
                                NativeIntegrationSemanticConflictKindV1::MigrationOrder
                            } else {
                                NativeIntegrationSemanticConflictKindV1::DivergentSchema
                            },
                            source: NativeIntegrationAnalysisAnchorV1 {
                                generation_id: source.generation.manifest().generation_id.clone(),
                                logical_path: (*source_path).to_owned(),
                                source_span: source_fact.span(),
                            },
                            destination: NativeIntegrationAnalysisAnchorV1 {
                                generation_id: destination
                                    .generation
                                    .manifest()
                                    .generation_id
                                    .clone(),
                                logical_path: (*destination_path).to_owned(),
                                source_span: destination_fact.span(),
                            },
                            evidence_digest: ManifestDigest::zero()?,
                        }
                        .seal()?,
                    );
                }
            }
        }
    }
    Ok(conflicts)
}

pub fn analyze_native_integration_generations(
    merge_base: &CodeIndexPublishedGenerationV1,
    source: &CodeIndexPublishedGenerationV1,
    destination: &CodeIndexPublishedGenerationV1,
    candidate: &CodeIndexPublishedGenerationV1,
    deadline: &Deadline,
    cancellation: &CancellationSignal,
) -> Result<NativeIntegrationGenerationAnalysisV1, NativeIntegrationPortError> {
    let ensure_active = || {
        if cancellation.is_cancelled() {
            Err(NativeIntegrationPortError::Cancelled)
        } else if deadline.is_elapsed_at(tracedecay_contracts::clock::now_micros()) {
            Err(NativeIntegrationPortError::Unavailable)
        } else {
            Ok(())
        }
    };
    let domain = |error: tracedecay_domain::DomainError| {
        NativeIntegrationPortError::Native(error.to_string())
    };
    ensure_active()?;
    let base = GenerationView::new(merge_base);
    let source = GenerationView::new(source);
    let destination = GenerationView::new(destination);
    let candidate = GenerationView::new(candidate);
    let source_changed = source.changed_from(&base);
    let destination_changed = destination.changed_from(&base);
    let candidate_changed = candidate.changed_from(&base);
    let affected_paths = changed_paths(&base, &source)
        .into_iter()
        .chain(changed_paths(&base, &destination))
        .collect::<BTreeSet<_>>();
    let mut conflicts = Vec::new();
    for identity in source_changed.intersection(&destination_changed) {
        ensure_active()?;
        let source_symbol = source.by_identity.get(identity);
        let destination_symbol = destination.by_identity.get(identity);
        let semantically_equal = match (source_symbol, destination_symbol) {
            (Some(source_symbol), Some(destination_symbol)) => {
                source_symbol.content_digest == destination_symbol.content_digest
                    && source_symbol.signature == destination_symbol.signature
            }
            (None, None) => true,
            _ => false,
        };
        if !semantically_equal
            && let (Some(source_anchor), Some(destination_anchor)) = (
                source.anchor_or_base(&base, identity),
                destination.anchor_or_base(&base, identity),
            )
        {
            conflicts.push(
                NativeIntegrationSemanticConflictV1 {
                    kind: NativeIntegrationSemanticConflictKindV1::DivergentSymbol,
                    source: source_anchor,
                    destination: destination_anchor,
                    evidence_digest: ManifestDigest::zero().map_err(domain)?,
                }
                .seal()
                .map_err(domain)?,
            );
        }
    }
    for source_identity in &source_changed {
        ensure_active()?;
        let signature_changed = source
            .by_identity
            .get(source_identity)
            .and_then(|symbol| symbol.signature.as_ref())
            != base
                .by_identity
                .get(source_identity)
                .and_then(|symbol| symbol.signature.as_ref());
        if !signature_changed {
            continue;
        }
        let related = base
            .related_to(source_identity)
            .chain(source.related_to(source_identity))
            .chain(destination.related_to(source_identity))
            .cloned()
            .collect::<BTreeSet<_>>();
        for destination_identity in destination_changed.intersection(&related) {
            ensure_active()?;
            if let (Some(source_anchor), Some(destination_anchor)) = (
                source.anchor_or_base(&base, source_identity),
                destination.anchor_or_base(&base, destination_identity),
            ) {
                conflicts.push(
                    NativeIntegrationSemanticConflictV1 {
                        kind: NativeIntegrationSemanticConflictKindV1::SignatureDependent,
                        source: source_anchor,
                        destination: destination_anchor,
                        evidence_digest: ManifestDigest::zero().map_err(domain)?,
                    }
                    .seal()
                    .map_err(domain)?,
                );
            }
        }
    }
    for destination_identity in &destination_changed {
        ensure_active()?;
        let signature_changed = destination
            .by_identity
            .get(destination_identity)
            .and_then(|symbol| symbol.signature.as_ref())
            != base
                .by_identity
                .get(destination_identity)
                .and_then(|symbol| symbol.signature.as_ref());
        if !signature_changed {
            continue;
        }
        let related = base
            .related_to(destination_identity)
            .chain(source.related_to(destination_identity))
            .chain(destination.related_to(destination_identity))
            .cloned()
            .collect::<BTreeSet<_>>();
        for source_identity in source_changed.intersection(&related) {
            ensure_active()?;
            if let (Some(source_anchor), Some(destination_anchor)) = (
                source.anchor_or_base(&base, source_identity),
                destination.anchor_or_base(&base, destination_identity),
            ) {
                conflicts.push(
                    NativeIntegrationSemanticConflictV1 {
                        kind: NativeIntegrationSemanticConflictKindV1::SignatureDependent,
                        source: source_anchor,
                        destination: destination_anchor,
                        evidence_digest: ManifestDigest::zero().map_err(domain)?,
                    }
                    .seal()
                    .map_err(domain)?,
                );
            }
        }
    }
    for (test, covered) in source.test_coverage_pairs() {
        ensure_active()?;
        if source_changed.contains(&test)
            && destination_changed.contains(&covered)
            && let (Some(source_anchor), Some(destination_anchor)) = (
                source.anchor_or_base(&base, &test),
                destination.anchor_or_base(&base, &covered),
            )
        {
            conflicts.push(
                NativeIntegrationSemanticConflictV1 {
                    kind: NativeIntegrationSemanticConflictKindV1::TestWriteInteraction,
                    source: source_anchor,
                    destination: destination_anchor,
                    evidence_digest: ManifestDigest::zero().map_err(domain)?,
                }
                .seal()
                .map_err(domain)?,
            );
        }
    }
    for (test, covered) in destination.test_coverage_pairs() {
        ensure_active()?;
        if destination_changed.contains(&test)
            && source_changed.contains(&covered)
            && let (Some(source_anchor), Some(destination_anchor)) = (
                source.anchor_or_base(&base, &covered),
                destination.anchor_or_base(&base, &test),
            )
        {
            conflicts.push(
                NativeIntegrationSemanticConflictV1 {
                    kind: NativeIntegrationSemanticConflictKindV1::TestWriteInteraction,
                    source: source_anchor,
                    destination: destination_anchor,
                    evidence_digest: ManifestDigest::zero().map_err(domain)?,
                }
                .seal()
                .map_err(domain)?,
            );
        }
    }
    conflicts.sort_by(|left, right| {
        (&left.kind, &left.source, &left.destination).cmp(&(
            &right.kind,
            &right.source,
            &right.destination,
        ))
    });
    conflicts.dedup_by(|left, right| {
        left.kind == right.kind
            && left.source == right.source
            && left.destination == right.destination
    });
    let mut graph_gaps = Vec::new();
    for view in [&base, &source, &destination, &candidate] {
        ensure_active()?;
        let analysis_coverage = view
            .generation
            .analysis_coverage()
            .collect::<BTreeMap<_, _>>();
        for path in &affected_paths {
            ensure_active()?;
            use tracedecay_domain::SnapshotFileDispositionV1;
            let Some(file) = view
                .generation
                .snapshot()
                .files
                .iter()
                .find(|file| &file.logical_path == path)
            else {
                let existed_at_base = base
                    .generation
                    .snapshot()
                    .files
                    .iter()
                    .any(|file| &file.logical_path == path);
                let exists_in_either_head = [&source, &destination].into_iter().any(|head| {
                    head.generation
                        .snapshot()
                        .files
                        .iter()
                        .any(|file| &file.logical_path == path)
                });
                let expected = if std::ptr::eq(view, &candidate) {
                    existed_at_base || exists_in_either_head
                } else {
                    !std::ptr::eq(view, &base) && existed_at_base
                };
                if expected {
                    graph_gaps.push(NativeIntegrationAnalysisGapV1::WithheldSource);
                }
                continue;
            };
            match file.disposition {
                SnapshotFileDispositionV1::Present
                | SnapshotFileDispositionV1::Deleted
                | SnapshotFileDispositionV1::Renamed => {}
                SnapshotFileDispositionV1::UnsupportedLanguage
                | SnapshotFileDispositionV1::Binary => {
                    graph_gaps.push(NativeIntegrationAnalysisGapV1::UnsupportedLanguage)
                }
                SnapshotFileDispositionV1::Ignored | SnapshotFileDispositionV1::Generated => {
                    graph_gaps.push(NativeIntegrationAnalysisGapV1::WithheldSource)
                }
            }
            if matches!(
                file.disposition,
                SnapshotFileDispositionV1::Present | SnapshotFileDispositionV1::Renamed
            ) {
                let Some(extraction) = analysis_coverage.get(path.as_str()) else {
                    graph_gaps.push(NativeIntegrationAnalysisGapV1::AuthorityUnavailable);
                    continue;
                };
                use tracedecay_code_index::extract::ParseOutcomeV1;
                if extraction.parse_outcome != ParseOutcomeV1::Complete
                    || extraction.coverage.error_bytes > 0
                    || !extraction.error_ranges.is_empty()
                {
                    graph_gaps.push(NativeIntegrationAnalysisGapV1::ParserFailure);
                }
                if extraction.coverage.unsupported_bytes > 0
                    || !extraction.unsupported_ranges.is_empty()
                {
                    graph_gaps.push(NativeIntegrationAnalysisGapV1::UnsupportedLanguage);
                }
            }
        }
    }
    let affected_identities = source_changed
        .iter()
        .chain(&destination_changed)
        .chain(&candidate_changed)
        .collect::<BTreeSet<_>>();
    let candidate_affected_sources = candidate
        .identities_in_paths(&affected_paths)
        .into_iter()
        .chain(base.identities_in_paths(&affected_paths))
        .collect::<BTreeSet<_>>();
    let mut target_names = BTreeSet::new();
    for view in [&base, &source, &destination, &candidate] {
        for identity in &affected_identities {
            if let Some(symbol) = view.by_identity.get(*identity) {
                target_names.insert(symbol.simple_name.as_str());
                target_names.insert(symbol.qualified_name.as_str());
            }
        }
    }
    if source.has_unresolved_required_edge(&source_changed, &target_names)
        || destination.has_unresolved_required_edge(&destination_changed, &target_names)
        || candidate.has_unresolved_required_edge(&candidate_affected_sources, &target_names)
    {
        graph_gaps.push(NativeIntegrationAnalysisGapV1::UnresolvedRequiredEdge);
    }
    let graph = if graph_gaps.is_empty() {
        lane(NativeIntegrationAnalysisCoverageV1::Complete, Vec::new())
    } else {
        lane(NativeIntegrationAnalysisCoverageV1::Partial, graph_gaps)
    };
    let tests = [&base, &source, &destination, &candidate]
        .iter()
        .map(|view| test_lane(view))
        .find(|lane| lane.coverage != NativeIntegrationAnalysisCoverageV1::Complete)
        .unwrap_or_else(|| lane(NativeIntegrationAnalysisCoverageV1::Complete, Vec::new()));
    ensure_active()?;
    let (schema, migrations, schema_conflicts) =
        schema_lanes(&base, &source, &destination, &candidate).map_err(domain)?;
    ensure_active()?;
    conflicts.extend(schema_conflicts);
    conflicts.sort_by(|left, right| {
        (&left.kind, &left.source, &left.destination).cmp(&(
            &right.kind,
            &right.source,
            &right.destination,
        ))
    });
    conflicts.dedup_by(|left, right| {
        left.kind == right.kind
            && left.source == right.source
            && left.destination == right.destination
    });
    Ok(NativeIntegrationGenerationAnalysisV1 {
        graph,
        tests,
        schema,
        migrations,
        conflicts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::SourceSpan;

    fn protobuf_field(name: &str, tag: u32, start_byte: u64) -> ExtractedSchemaFactV1 {
        ExtractedSchemaFactV1::ProtobufField {
            message_qualified_name: "example.Record".to_owned(),
            name: name.to_owned(),
            type_name: "string".to_owned(),
            tag,
            span: SourceSpan {
                start_byte,
                end_byte: start_byte + 1,
            },
        }
    }

    #[test]
    fn protobuf_schema_comparison_ignores_spans_and_detects_tag_reuse() {
        let original = protobuf_field("name", 1, 10);
        let moved = protobuf_field("name", 1, 40);
        let reused_tag = protobuf_field("display_name", 1, 70);

        assert!(schema_facts_semantically_equal(&original, &moved));
        assert!(schema_facts_overlap(&original, &reused_tag));
        assert!(!schema_facts_semantically_equal(&original, &reused_tag));
    }
}
