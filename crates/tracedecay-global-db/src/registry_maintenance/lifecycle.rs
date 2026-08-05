use super::*;

/// How dead a registry row's project root must be before the row counts as
/// stale. This is the single definition of both GC scopes, so a reader never
/// has to reassemble the effective condition from scattered half-checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleRootScope {
    /// Manual registry cleanup scope: the canonical root is
    /// gone (the user reviews candidates before applying).
    CanonicalRootMissing,
    /// Post-update auto-GC scope: both the canonical and display roots are
    /// gone — stricter, because nobody reviews the deletion.
    AllRootsMissing,
}

/// Whether a recorded root could be proven present or absent.
///
/// Deletion authority requires proof of *absence*. An inspection that fails —
/// an unreadable parent directory, a stale mount, any I/O error — proves
/// nothing, so it is [`RootLivenessV1::Unverifiable`] and never absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLivenessV1 {
    Live,
    Absent,
    Unverifiable,
}

impl RootLivenessV1 {
    /// Whether this liveness permits retiring the identity that owns the root.
    /// Only proven absence does.
    pub fn permits_retirement(self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Combine sibling roots of one identity: any live root wins, then any
    /// unverifiable root, and absence only when every root was proven absent.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Live, _) | (_, Self::Live) => Self::Live,
            (Self::Unverifiable, _) | (_, Self::Unverifiable) => Self::Unverifiable,
            (Self::Absent, Self::Absent) => Self::Absent,
        }
    }
}

/// Probe one root with typed existence, so a permission or I/O failure is
/// reported as unverifiable rather than silently read as "gone".
pub fn probe_root(root: &Path) -> RootLivenessV1 {
    match root.try_exists() {
        Ok(true) => RootLivenessV1::Live,
        Ok(false) => RootLivenessV1::Absent,
        Err(_) => RootLivenessV1::Unverifiable,
    }
}

/// Liveness across every root one registry row records: canonical, display, and
/// the git common directory. A linked worktree shares its common directory with
/// the primary checkout, so a live common directory means the repository
/// identity is still in use even when this row's working tree is gone.
pub fn code_project_root_liveness(project: &CodeProjectRecord) -> RootLivenessV1 {
    let mut liveness = probe_root(Path::new(&project.canonical_root))
        .merge(probe_root(Path::new(&project.display_root)));
    if let Some(git_common_dir) = project.git_common_dir.as_deref() {
        liveness = liveness.merge(probe_root(Path::new(git_common_dir)));
    }
    liveness
}

/// Returns true unless every root this row records was *proven* absent.
pub fn code_project_root_exists(project: &CodeProjectRecord) -> bool {
    !code_project_root_liveness(project).permits_retirement()
}

/// Liveness of a fully-resolved registry identity: every root of the project
/// row plus every registered alias path.
///
/// A registered store instance keeps the identity live on its own. Deleting the
/// `code_projects` row cascades its aliases and store instances away, so a row
/// that still owns a store must never be retired by an unreviewed pass.
pub fn project_context_liveness(context: &ProjectRegistryContext) -> RootLivenessV1 {
    if !context.stores.is_empty() {
        return RootLivenessV1::Live;
    }
    context.aliases.iter().fold(
        code_project_root_liveness(&context.project),
        |liveness, alias| liveness.merge(probe_root(Path::new(&alias.alias_path))),
    )
}

/// Registry identities that are stale under `scope` across every root and alias
/// they record, restricted to canonical roots under one of `prefixes`.
///
/// This is the context-aware counterpart of [`stale_code_projects`]: an
/// unreviewed pass must resolve aliases and store instances before retiring an
/// identity, or it will retire a project another checkout is still using.
pub fn stale_project_contexts<'a>(
    contexts: &'a [ProjectRegistryContext],
    prefixes: &[PathBuf],
    scope: StaleRootScope,
) -> Vec<&'a ProjectRegistryContext> {
    contexts
        .iter()
        .filter(|context| {
            let canonical_root = Path::new(&context.project.canonical_root);
            prefixes.is_empty()
                || prefixes
                    .iter()
                    .any(|prefix| canonical_root.starts_with(prefix))
        })
        .filter(|context| match scope {
            StaleRootScope::CanonicalRootMissing => {
                probe_root(Path::new(&context.project.canonical_root)).permits_retirement()
                    && project_context_liveness(context).permits_retirement()
            }
            StaleRootScope::AllRootsMissing => {
                project_context_liveness(context).permits_retirement()
            }
        })
        .collect()
}

/// Filters registry rows that are stale under `scope`, restricted to
/// canonical roots under one of `prefixes` (an empty slice means no
/// restriction). Shared by explicit registry cleanup and the
/// post-update health pass so both agree on what counts as a GC candidate.
pub fn stale_code_projects<'a>(
    projects: &'a [CodeProjectRecord],
    prefixes: &[PathBuf],
    scope: StaleRootScope,
) -> Vec<&'a CodeProjectRecord> {
    projects
        .iter()
        .filter(|project| {
            let canonical_root = Path::new(&project.canonical_root);
            prefixes.is_empty()
                || prefixes
                    .iter()
                    .any(|prefix| canonical_root.starts_with(prefix))
        })
        .filter(|project| match scope {
            StaleRootScope::CanonicalRootMissing => {
                probe_root(Path::new(&project.canonical_root)).permits_retirement()
            }
            StaleRootScope::AllRootsMissing => !code_project_root_exists(project),
        })
        .collect()
}

/// Builds the exact registry cleanup plan without mutating registry state.
/// Both daemon-owned and offline maintenance paths must hold their mutation
/// authority before applying this plan.
pub async fn registry_gc_report(
    db: &RegisteredGlobalDb,
    _profile_root: &Path,
    prefix: Option<String>,
) -> tracedecay_runtime_core::errors::Result<RegistryGcReport> {
    let prefixes = prefix.iter().map(PathBuf::from).collect::<Vec<_>>();
    let projects = db.list_code_projects(usize::MAX).await?;
    let mut candidates = Vec::new();
    let mut protected_code_projects = Vec::new();
    for project in stale_code_projects(&projects, &prefixes, StaleRootScope::CanonicalRootMissing) {
        if db
            .try_list_store_instances_for_project(&project.project_id)
            .await?
            .is_empty()
        {
            candidates.push(project.clone());
        } else {
            protected_code_projects.push(project.clone());
        }
    }

    let mut storage_project_candidates = Vec::new();
    for project_path in db.try_list_project_paths().await? {
        if !prefixes.is_empty()
            && !prefixes
                .iter()
                .any(|prefix| project_path.starts_with(prefix))
        {
            continue;
        }
        if !project_path.exists() {
            storage_project_candidates.push(project_path);
        }
    }

    let candidate_paths = candidates
        .iter()
        .map(|project| {
            RegisteredGlobalDb::canonical_project_key(Path::new(&project.canonical_root))
        })
        .chain(
            storage_project_candidates
                .iter()
                .map(|path| RegisteredGlobalDb::canonical_project_key(path)),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(RegistryGcReport {
        apply: false,
        prefix,
        candidate_count: candidate_paths.len(),
        metadata_candidate_count: candidates.len() + storage_project_candidates.len(),
        code_project_candidate_count: candidates.len(),
        storage_project_candidate_count: storage_project_candidates.len(),
        protected_code_project_count: protected_code_projects.len(),
        deleted_count: 0,
        deleted_code_project_count: 0,
        deleted_storage_project_count: 0,
        candidate_paths,
        candidates,
        protected_code_projects,
        storage_project_candidates,
    })
}

pub async fn apply_registry_gc(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    prefix: Option<String>,
) -> tracedecay_runtime_core::errors::Result<RegistryGcReport> {
    let transaction = db.begin_write_transaction().await?;
    let mut report = registry_gc_report(db, profile_root, prefix).await?;
    for project in &report.candidates {
        if Path::new(&project.canonical_root).exists() {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!(
                    "registry cleanup candidate '{}' became live while applying the plan",
                    project.project_id
                ),
            });
        }
    }
    for project_path in &report.storage_project_candidates {
        if project_path.exists() {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!(
                    "registry cleanup candidate '{}' became live while applying the plan",
                    project_path.display()
                ),
            });
        }
    }
    let project_ids = report
        .candidates
        .iter()
        .map(|project| project.project_id.clone())
        .collect::<Vec<_>>();
    let (deleted_code_projects, deleted_storage_projects) =
        delete_registry_gc_candidates_in_transaction(
            &transaction,
            &project_ids,
            &report.storage_project_candidates,
        )
        .await?;
    transaction.commit().await?;
    report.record_deletions(deleted_code_projects, deleted_storage_projects);
    Ok(report)
}

pub(super) async fn delete_registry_gc_candidates_in_transaction(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_ids: &[String],
    project_paths: &[PathBuf],
) -> tracedecay_runtime_core::errors::Result<(usize, usize)> {
    const CHUNK: usize = 256;
    let mut code_projects = 0_usize;
    for chunk in project_ids.chunks(CHUNK) {
        let sql = format!(
            "DELETE FROM code_projects WHERE project_id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let values = chunk
            .iter()
            .cloned()
            .map(tracedecay_runtime_core::db::engine::Value::Text)
            .collect::<Vec<_>>();
        code_projects =
            code_projects.saturating_add(transaction.execute(&sql, values).await? as usize);
    }

    let mut storage_projects = 0_usize;
    for chunk in project_paths.chunks(CHUNK) {
        let sql = format!(
            "DELETE FROM projects WHERE path IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let values = chunk
            .iter()
            .map(|path| {
                tracedecay_runtime_core::db::engine::Value::Text(
                    RegisteredGlobalDb::project_path_alias_key(path),
                )
            })
            .collect::<Vec<_>>();
        storage_projects =
            storage_projects.saturating_add(transaction.execute(&sql, values).await? as usize);
    }
    Ok((code_projects, storage_projects))
}

/// Retires exact registry rows for project roots already removed by an
/// explicit operator action.
pub async fn retire_registry_project_paths(
    db: &RegisteredGlobalDb,
    project_paths: &[PathBuf],
) -> tracedecay_runtime_core::errors::Result<usize> {
    let transaction = db.begin_write_transaction().await?;
    let (_, retired) =
        delete_registry_gc_candidates_in_transaction(&transaction, &[], project_paths).await?;
    transaction.commit().await?;
    Ok(retired)
}
