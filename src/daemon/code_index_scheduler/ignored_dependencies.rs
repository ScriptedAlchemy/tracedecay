//! Verified admission of one exact ignored dependency entrypoint.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use gix::bstr::ByteSlice;
use serde_json::Value;
use tracedecay_application::ResolvedScope;
use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::production::{
    CodeIndexBuildRequestV1, CodeIndexExecutionControlV1, CodeIndexIgnoredSourceAdmissionV1,
    CodeIndexInterruptionV1, CodeIndexProductionErrorV1,
    MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1,
};
use tracedecay_domain::{CodeGenerationId, canonical_sha256};

use super::{
    CapturedSnapshotV1, CodeIndexPublishEvidenceV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, LatestCompleteCodeIndexV1, StaticLanguageRegistry, now_micros,
    projection_key,
};
use crate::code_index::languages::LanguageRegistry;

pub(super) const ADMITTED_SOURCE_READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::daemon) struct CodeIndexIgnoredDependencyRequestV1 {
    pub scope: ResolvedScope,
    pub expected_generation: CodeGenerationId,
    pub verified_imports: Vec<CodeIndexImportEvidenceV1>,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub(in crate::daemon) enum CodeIndexIgnoredDependencyRefusalV1 {
    #[error("the import evidence is not present byte-exactly in the pinned serving generation")]
    UnverifiedImportEvidence,
    #[error("only parser-verified external type imports support ignored dependency admission")]
    UnsupportedImport,
    #[error("the dependency entrypoint escapes its package root")]
    PathEscape,
    #[error("the dependency entrypoint resolves through a symlink outside the mounted root")]
    SymlinkEscape,
    #[error("the dependency entrypoint language is unsupported")]
    UnsupportedLanguage,
    #[error("the dependency entrypoint exceeds the per-file byte limit")]
    ByteLimitExceeded,
    #[error("the dependency entrypoint was refused by the privacy boundary")]
    PrivacyRefused,
    #[error("the ignored dependency admission was cancelled")]
    Cancelled,
    #[error("the ignored dependency admission deadline was exceeded")]
    DeadlineExceeded,
    #[error("the resolved dependency entrypoint is not ignored by gix")]
    NotIgnored,
    #[error("one request may admit exactly one dependency entrypoint")]
    EntryPointLimitExceeded,
    #[error("the requested generation is no longer the activated serving generation")]
    StaleGeneration,
    #[error("the request scope does not identify this exact mounted worktree")]
    ScopeMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::daemon) struct CodeIndexIgnoredDependencyIndexOutcomeV1 {
    pub generation_id: CodeGenerationId,
    pub admission: CodeIndexIgnoredSourceAdmissionV1,
}

pub(super) struct CodeIndexIgnoredDependencyBuildV1 {
    pub outcome: CodeIndexIgnoredDependencyIndexOutcomeV1,
    pub latest: LatestCompleteCodeIndexV1,
    pub publication: CodeIndexPublishEvidenceV1,
}

impl CodeIndexWorktreeSchedulerV1 {
    pub(super) fn index_verified_ignored_dependency(
        &mut self,
        serving: &LatestCompleteCodeIndexV1,
        request: CodeIndexIgnoredDependencyRequestV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexIgnoredDependencyBuildV1, CodeIndexSchedulerErrorV1> {
        checkpoint(control)?;
        self.validate_ignored_dependency_scope(serving, &request)?;
        self.validate_serving_is_active(serving)?;
        let import = one_verified_external_type_import(serving, &request)?;
        let admission = self.resolve_ignored_dependency_admission(import, control)?;
        checkpoint(control)?;

        let previous_roster = self.ignored_source_admissions.clone();
        let mut paths = previous_roster
            .iter()
            .map(|entry| entry.logical_path.clone())
            .collect::<BTreeSet<_>>();
        paths.insert(admission.logical_path.clone());
        self.ignored_source_admissions = paths
            .into_iter()
            .map(|logical_path| CodeIndexIgnoredSourceAdmissionV1 { logical_path })
            .collect();

        self.publish_ignored_dependency_generation(admission, previous_roster, control)
    }

    fn validate_ignored_dependency_scope(
        &self,
        serving: &LatestCompleteCodeIndexV1,
        request: &CodeIndexIgnoredDependencyRequestV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let snapshot = serving.generation().snapshot();
        if request.scope.validate().is_err()
            || request.scope.project_id != self.project_id
            || request.scope.repository_id != self.repository_id
            || request.scope.worktree_id != self.worktree_id
            || request.scope.reference != snapshot.reference
        {
            return Err(CodeIndexIgnoredDependencyRefusalV1::ScopeMismatch.into());
        }
        if request.expected_generation != serving.generation().manifest().generation_id {
            return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
        }
        Ok(())
    }

    fn validate_serving_is_active(
        &self,
        serving: &LatestCompleteCodeIndexV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        if !self.active_publication_matches(serving)? {
            return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
        }
        Ok(())
    }

    pub(super) fn active_publication_matches(
        &self,
        candidate: &LatestCompleteCodeIndexV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let active = self
            .publication
            .load_active_shared()
            .map_err(CodeIndexProductionErrorV1::Publication)?;
        Ok(active.is_some_and(|active| {
            active.manifest() == candidate.generation().manifest()
                && active.snapshot() == candidate.generation().snapshot()
                && active.ignored_source_admissions()
                    == candidate.generation().ignored_source_admissions()
                && active.ignored_source_admissions_digest()
                    == candidate.generation().ignored_source_admissions_digest()
        }))
    }

    fn resolve_ignored_dependency_admission(
        &self,
        import: &CodeIndexImportEvidenceV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexIgnoredSourceAdmissionV1, CodeIndexSchedulerErrorV1> {
        checkpoint(control)?;
        let package_relative = validated_module_path(&import.module_specifier)?;
        let package_root = self
            .project_root
            .join("node_modules")
            .join(package_relative);
        let canonical_package = canonical_package_root(&self.project_root, &package_root)?;
        let entrypoint =
            resolve_package_entrypoint(&package_root, &canonical_package, Some(control))?;
        let logical_path = logical_path_for(&self.project_root, &entrypoint)?;
        validate_admitted_source(
            &self.project_root,
            &canonical_package,
            &entrypoint,
            &logical_path,
            Some(control),
        )?;
        checkpoint(control)?;
        Ok(CodeIndexIgnoredSourceAdmissionV1 { logical_path })
    }

    fn publish_ignored_dependency_generation(
        &mut self,
        admission: CodeIndexIgnoredSourceAdmissionV1,
        previous_roster: Vec<CodeIndexIgnoredSourceAdmissionV1>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexIgnoredDependencyBuildV1, CodeIndexSchedulerErrorV1> {
        let resolved = match super::identity::IndexingIdentityV1::resolve(&self.project_root) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.ignored_source_admissions = previous_roster;
                return Err(CodeIndexSchedulerErrorV1::Identity(error.to_string()));
            }
        };
        if !resolved.authorizes_reuse_of(&self.identity) {
            self.ignored_source_admissions = previous_roster;
            return Err(CodeIndexIgnoredDependencyRefusalV1::ScopeMismatch.into());
        }
        self.identity = resolved;
        let sampled_metadata =
            super::identity::GitMetadataFingerprintV1::capture(&self.project_root);
        let captured = match self.capture_authoritative_snapshot(Some(control)) {
            Ok(captured) => captured,
            Err(error) => {
                self.ignored_source_admissions = previous_roster;
                return Err(error);
            }
        };
        if let Err(error) = checkpoint(control) {
            self.ignored_source_admissions = previous_roster;
            return Err(error);
        }
        let CapturedSnapshotV1 {
            snapshot,
            repository_parse_identity,
            captured_files,
            changed_paths,
            retained_bytes,
        } = captured;
        let reextracted_files = changed_paths.len();
        let roster = self.ignored_source_admissions.clone();
        let target_projection_key = match projection_key() {
            Ok(key) => key,
            Err(error) => {
                self.ignored_source_admissions = previous_roster;
                return Err(error);
            }
        };
        let generation = match self.owner.build_and_publish(
            CodeIndexBuildRequestV1 {
                snapshot: snapshot.clone(),
                captured_files,
                changed_files: changed_paths,
                invalidations: BTreeSet::new(),
                repository_parse_identity,
                ignored_source_admissions: roster,
                sealed_at: now_micros(),
                target_projection_key,
            },
            control,
        ) {
            Ok(generation) => generation,
            Err(error) => {
                self.ignored_source_admissions = previous_roster;
                return Err(map_production_interruption(error));
            }
        };
        let generation_id = generation.manifest().generation_id.clone();
        self.retained_snapshot_bytes = retained_bytes;
        self.latest_content_identity = Some(generation.snapshot().content_identity.clone());
        let sampled_signature = self.worktree_stat_signature().ok();
        self.mark_reconciled(sampled_metadata, sampled_signature);
        let latest = self.bind_latest_complete(Arc::clone(&generation));
        let publication = publication_evidence(reextracted_files, &generation)?;
        Ok(CodeIndexIgnoredDependencyBuildV1 {
            outcome: CodeIndexIgnoredDependencyIndexOutcomeV1 {
                generation_id,
                admission,
            },
            latest,
            publication,
        })
    }

    pub(super) fn adopt_ignored_source_roster(
        &mut self,
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) {
        self.ignored_source_admissions = generation.ignored_source_admissions().to_vec();
    }

    pub(super) fn ignored_source_roster_matches_generation(
        &self,
        generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
    ) -> bool {
        let registry = StaticLanguageRegistry::new();
        generation
            .ignored_source_admissions()
            .iter()
            .all(|admission| {
                let absolute = self.project_root.join(&admission.logical_path);
                let Ok(canonical) = absolute.canonicalize() else {
                    return false;
                };
                if !canonical.starts_with(&self.project_root) {
                    return false;
                }
                let Some(expected) = generation
                    .snapshot()
                    .files
                    .iter()
                    .find(|file| file.logical_path == admission.logical_path)
                else {
                    return false;
                };
                self.capture_candidate(&registry, &admission.logical_path, None)
                    .ok()
                    .flatten()
                    .is_some_and(|captured| captured.file == *expected)
            })
    }
}

fn one_verified_external_type_import<'a>(
    serving: &'a LatestCompleteCodeIndexV1,
    request: &'a CodeIndexIgnoredDependencyRequestV1,
) -> Result<&'a CodeIndexImportEvidenceV1, CodeIndexSchedulerErrorV1> {
    if request.verified_imports.len() != 1 {
        return Err(CodeIndexIgnoredDependencyRefusalV1::EntryPointLimitExceeded.into());
    }
    let import = &request.verified_imports[0];
    if import.namespace != ImportNamespaceV1::Type
        || import.module_kind != ImportModuleKindV1::BareModule
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport.into());
    }
    if !serving.generation().imports().contains(import) {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnverifiedImportEvidence.into());
    }
    Ok(import)
}

fn validated_module_path(module: &str) -> Result<PathBuf, CodeIndexSchedulerErrorV1> {
    if module.is_empty() || module.contains('\\') || module.contains('\0') {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let path = Path::new(module);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(component) => component.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let supported_package_name = match components.as_slice() {
        [package] => !package.is_empty() && !package.starts_with('@'),
        [scope, package] => scope.starts_with('@') && scope.len() > 1 && !package.is_empty(),
        _ => false,
    };
    if !supported_package_name {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport.into());
    }
    Ok(path.to_path_buf())
}

fn resolve_package_entrypoint(
    package_root: &Path,
    canonical_package: &Path,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<PathBuf, CodeIndexSchedulerErrorV1> {
    checkpoint_if_present(control)?;
    let package_json = package_root.join("package.json");
    if package_json.is_file() {
        let canonical_package_json = package_json
            .canonicalize()
            .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport)?;
        if !canonical_package_json.starts_with(canonical_package) {
            return Err(CodeIndexIgnoredDependencyRefusalV1::SymlinkEscape.into());
        }
        let package_json_bytes = read_bounded_source(&package_json, control)?;
        let value: Value = serde_json::from_slice(&package_json_bytes)
            .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport)?;
        if let Some(types) = value.get("types").and_then(Value::as_str) {
            let relative = validated_package_entrypoint(types)?;
            let entrypoint = package_root.join(relative);
            return entrypoint
                .is_file()
                .then_some(entrypoint)
                .ok_or_else(|| CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport.into());
        }
    }
    [
        "index.d.ts",
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
    ]
    .into_iter()
    .map(|name| package_root.join(name))
    .find(|candidate| candidate.is_file())
    .ok_or_else(|| CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport.into())
}

pub(super) fn read_explicitly_admitted_source(
    project_root: &Path,
    logical_path: &str,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    if is_ignored_dependency_admission_path(logical_path) {
        read_bounded_admitted_source(project_root, logical_path, control)
    } else {
        read_contained_project_source(project_root, logical_path, control)
    }
}

pub(super) fn read_bounded_admitted_source(
    project_root: &Path,
    logical_path: &str,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    checkpoint_if_present(control)?;
    let package_root = package_root_for_admitted_path(project_root, logical_path)?;
    let canonical_package = canonical_package_root(project_root, &package_root)?;
    let entrypoint = resolve_package_entrypoint(&package_root, &canonical_package, control)?;
    if logical_path_for(project_root, &entrypoint)? != logical_path {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport.into());
    }
    validate_admitted_source(
        project_root,
        &canonical_package,
        &entrypoint,
        logical_path,
        control,
    )
}

fn is_ignored_dependency_admission_path(logical_path: &str) -> bool {
    logical_path == "node_modules" || logical_path.starts_with("node_modules/")
}

fn read_contained_project_source(
    project_root: &Path,
    logical_path: &str,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    checkpoint_if_present(control)?;
    if logical_path.is_empty() || logical_path.contains(['\\', '\0']) {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let relative = Path::new(logical_path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let canonical = project_root
        .join(relative)
        .canonicalize()
        .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::PathEscape)?;
    if !canonical.starts_with(project_root) {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    if logical_path_for(project_root, &canonical)? != logical_path {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    read_bounded_snapshot_source(&canonical, control)
}

fn canonical_package_root(
    project_root: &Path,
    package_root: &Path,
) -> Result<PathBuf, CodeIndexSchedulerErrorV1> {
    let canonical = package_root
        .canonicalize()
        .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport)?;
    if !canonical.starts_with(project_root) || canonical != package_root {
        return Err(CodeIndexIgnoredDependencyRefusalV1::SymlinkEscape.into());
    }
    Ok(canonical)
}

fn package_root_for_admitted_path(
    project_root: &Path,
    logical_path: &str,
) -> Result<PathBuf, CodeIndexSchedulerErrorV1> {
    if logical_path.is_empty() || logical_path.contains(['\\', '\0']) {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let components = Path::new(logical_path)
        .components()
        .map(|component| match component {
            Component::Normal(component) => component
                .to_str()
                .ok_or(CodeIndexIgnoredDependencyRefusalV1::PathEscape),
            _ => Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.first() != Some(&"node_modules") {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let package_components = if components
        .get(1)
        .is_some_and(|value| value.starts_with('@'))
    {
        3
    } else {
        2
    };
    if components.len() <= package_components
        || components
            .get(1)
            .is_none_or(|component| component.is_empty() || component == &"@")
        || (package_components == 3
            && components
                .get(2)
                .is_none_or(|component| component.is_empty()))
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    Ok(components
        .into_iter()
        .take(package_components)
        .fold(project_root.to_path_buf(), |path, component| {
            path.join(component)
        }))
}

fn validate_admitted_source(
    project_root: &Path,
    canonical_package: &Path,
    entrypoint: &Path,
    logical_path: &str,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    checkpoint_if_present(control)?;
    let canonical_entrypoint = entrypoint
        .canonicalize()
        .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport)?;
    if !canonical_entrypoint.starts_with(project_root)
        || !canonical_entrypoint.starts_with(canonical_package)
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::SymlinkEscape.into());
    }
    prove_gix_ignored(project_root, logical_path)?;
    let metadata = std::fs::metadata(&canonical_entrypoint)?;
    if metadata.len() > MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1 as u64 {
        return Err(CodeIndexIgnoredDependencyRefusalV1::ByteLimitExceeded.into());
    }
    let Some(extension) = entrypoint
        .extension()
        .and_then(|extension| extension.to_str())
    else {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnsupportedLanguage.into());
    };
    let registry = StaticLanguageRegistry::new();
    let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnsupportedLanguage.into());
    };
    let language = descriptor.language.clone();
    let bytes = read_bounded_source(&canonical_entrypoint, control)?;
    checkpoint_if_present(control)?;
    super::privacy::sanitize_code_file(&language, &bytes)
        .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::PrivacyRefused)?;
    checkpoint_if_present(control)?;
    Ok(bytes)
}

fn read_bounded_source(
    path: &Path,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    let limit = MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1
        .checked_add(1)
        .ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "ignored-dependency byte limit cannot be represented".to_owned(),
            )
        })?;
    let mut bytes = Vec::new();
    let mut source = std::fs::File::open(path)?.take(limit as u64);
    let mut chunk = vec![0_u8; ADMITTED_SOURCE_READ_CHUNK_BYTES];
    loop {
        checkpoint_if_present(control)?;
        let read = source.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() > MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1 {
        return Err(CodeIndexIgnoredDependencyRefusalV1::ByteLimitExceeded.into());
    }
    Ok(bytes)
}

pub(super) fn read_bounded_snapshot_source(
    path: &Path,
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<Vec<u8>, CodeIndexSchedulerErrorV1> {
    let mut bytes = Vec::new();
    let mut source = std::fs::File::open(path)?;
    let mut chunk = vec![0_u8; ADMITTED_SOURCE_READ_CHUNK_BYTES];
    loop {
        checkpoint_if_present(control)?;
        let read = source.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn validated_package_entrypoint(value: &str) -> Result<PathBuf, CodeIndexSchedulerErrorV1> {
    if value.is_empty() || value.contains('\\') || value.contains('\0') {
        return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
    }
    let mut relative = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => relative.push(component),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(CodeIndexIgnoredDependencyRefusalV1::UnsupportedImport.into());
    }
    Ok(relative)
}

fn logical_path_for(root: &Path, path: &Path) -> Result<String, CodeIndexSchedulerErrorV1> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::PathEscape)?;
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(CodeIndexIgnoredDependencyRefusalV1::PathEscape.into());
        };
        components.push(
            component
                .to_str()
                .ok_or(CodeIndexIgnoredDependencyRefusalV1::PathEscape)?,
        );
    }
    Ok(components.join("/"))
}

fn prove_gix_ignored(root: &Path, logical_path: &str) -> Result<(), CodeIndexSchedulerErrorV1> {
    let repository =
        gix::open(root).map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let index = repository
        .index_or_empty()
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    if index
        .entry_by_path(logical_path.as_bytes().as_bstr())
        .is_some()
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::NotIgnored.into());
    }
    let mut excludes = repository
        .excludes(
            &index,
            None,
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let ignored = excludes
        .at_path(Path::new(logical_path), None)
        .map_err(CodeIndexSchedulerErrorV1::Io)?
        .is_excluded();
    if ignored {
        Ok(())
    } else {
        Err(CodeIndexIgnoredDependencyRefusalV1::NotIgnored.into())
    }
}

fn checkpoint(control: &dyn CodeIndexExecutionControlV1) -> Result<(), CodeIndexSchedulerErrorV1> {
    if control.is_cancelled() {
        Err(CodeIndexIgnoredDependencyRefusalV1::Cancelled.into())
    } else if control.is_deadline_exceeded() {
        Err(CodeIndexIgnoredDependencyRefusalV1::DeadlineExceeded.into())
    } else {
        Ok(())
    }
}

pub(super) fn checkpoint_if_present(
    control: Option<&dyn CodeIndexExecutionControlV1>,
) -> Result<(), CodeIndexSchedulerErrorV1> {
    match control {
        Some(control) => checkpoint(control),
        None => Ok(()),
    }
}

fn map_production_interruption(error: CodeIndexProductionErrorV1) -> CodeIndexSchedulerErrorV1 {
    match error {
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled) => {
            CodeIndexIgnoredDependencyRefusalV1::Cancelled.into()
        }
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded) => {
            CodeIndexIgnoredDependencyRefusalV1::DeadlineExceeded.into()
        }
        other => other.into(),
    }
}

fn publication_evidence(
    reextracted_files: usize,
    generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1,
) -> Result<CodeIndexPublishEvidenceV1, CodeIndexSchedulerErrorV1> {
    let changes = &generation.projection().request().changes;
    let lane_digest = canonical_sha256(&(
        generation.snapshot().content_identity.clone(),
        generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, &chunk.content_digest))
            .collect::<Vec<_>>(),
        generation.edges(),
    ))
    .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
    Ok(CodeIndexPublishEvidenceV1 {
        generation_id: generation.manifest().generation_id.clone(),
        repository_id: generation.snapshot().repository.clone(),
        snapshot_content_identity: generation.snapshot().content_identity.clone(),
        _lane_digest: lane_digest,
        _file_occurrence_ids: generation
            .snapshot()
            .files
            .iter()
            .map(|file| file.file_occurrence_id.clone())
            .collect(),
        reextracted_files,
        changed_chunks: changes.added_or_changed.len() + changes.deleted.len(),
        reused_chunks: changes.reused.len(),
        overflow_reconciled: false,
    })
}
