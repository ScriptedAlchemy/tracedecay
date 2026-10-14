use super::*;

/// Whether a recorded root could be proven present or absent.
///
/// Deletion authority requires proof of *absence*. An inspection that fails,
/// an unreadable parent directory, a stale mount, any I/O error, proves
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

/// Liveness of a fully-resolved registry identity: every root of the project
/// row plus every registered alias path.
///
/// A registered store instance keeps the identity live on its own. Deleting the
/// `code_projects` row cascades its aliases and store instances away, so
/// registry cleanup retires an identity only when this proves it absent.
pub fn project_context_liveness(context: &ProjectRegistryContext) -> RootLivenessV1 {
    if !context.stores.is_empty() {
        return RootLivenessV1::Live;
    }
    context.aliases.iter().fold(
        code_project_root_liveness(&context.project),
        |liveness, alias| liveness.merge(probe_root(Path::new(&alias.alias_path))),
    )
}

/// Registry rows whose canonical root was proven absent, restricted to
/// canonical roots under one of `prefixes` (an empty slice means no
/// restriction). Each is either retirable or protected, see
/// [`project_context_liveness`].
fn canonical_root_missing(
    projects: Vec<CodeProjectRecord>,
    prefixes: &[PathBuf],
) -> Vec<CodeProjectRecord> {
    projects
        .into_iter()
        .filter(|project| {
            let canonical_root = Path::new(&project.canonical_root);
            (prefixes.is_empty()
                || prefixes
                    .iter()
                    .any(|prefix| canonical_root.starts_with(prefix)))
                && probe_root(canonical_root).permits_retirement()
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
) -> tracedecay_domain::errors::Result<RegistryGcReport> {
    let prefixes = prefix.iter().map(PathBuf::from).collect::<Vec<_>>();
    let projects = canonical_root_missing(db.list_code_projects(usize::MAX).await?, &prefixes);
    // Deleting a `code_projects` row cascades its aliases and store instances,
    // so a missing canonical root alone never retires an identity that a
    // linked worktree, its git common directory, or a store still uses.
    let mut candidates = Vec::new();
    let mut protected_code_projects = Vec::new();
    for context in db.project_registry_contexts_for_projects(&projects).await? {
        if project_context_liveness(&context).permits_retirement() {
            candidates.push(context.project);
        } else {
            protected_code_projects.push(context.project);
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
) -> tracedecay_domain::errors::Result<RegistryGcReport> {
    let transaction = db.begin_write_transaction().await?;
    let mut report = registry_gc_report(db, profile_root, prefix).await?;
    // Re-probe with the planning rule: an alias or store that appeared since
    // the plan would otherwise be cascaded away with its identity.
    for context in db
        .project_registry_contexts_for_projects(&report.candidates)
        .await?
    {
        if !project_context_liveness(&context).permits_retirement() {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "registry cleanup candidate '{}' became live while applying the plan",
                    context.project.project_id
                ),
            });
        }
    }
    for project_path in &report.storage_project_candidates {
        if project_path.exists() {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
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
) -> tracedecay_domain::errors::Result<(usize, usize)> {
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

/// Rows removed by one `projects forget` registry retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ForgetRegistryProjectRows {
    /// Deleted `code_projects` identity rows; aliases, store instances, graph
    /// scopes, and store artifacts cascade with them.
    pub code_projects_deleted: usize,
    /// Deleted path-keyed `projects` token-ledger rows.
    pub path_ledger_rows_deleted: usize,
}

/// Retires one registered project identity in a single transaction: the
/// `code_projects` row (its aliases, store instances, graph scopes, and store
/// artifacts cascade away with it) plus the path-keyed `projects` ledger rows
/// for every root and alias the identity records. Sibling identities are
/// untouched, this is the row authority behind `tracedecay projects forget`.
pub async fn forget_registry_project(
    db: &RegisteredGlobalDb,
    project_id: &str,
    project_paths: &[PathBuf],
) -> tracedecay_domain::errors::Result<ForgetRegistryProjectRows> {
    let transaction = db.begin_write_transaction().await?;
    let (code_projects_deleted, path_ledger_rows_deleted) =
        delete_registry_gc_candidates_in_transaction(
            &transaction,
            &[project_id.to_string()],
            project_paths,
        )
        .await?;
    transaction.commit().await?;
    Ok(ForgetRegistryProjectRows {
        code_projects_deleted,
        path_ledger_rows_deleted,
    })
}

/// Retires exact registry rows for project roots already removed by an
/// explicit operator action.
pub async fn retire_registry_project_paths(
    db: &RegisteredGlobalDb,
    project_paths: &[PathBuf],
) -> tracedecay_domain::errors::Result<usize> {
    let transaction = db.begin_write_transaction().await?;
    let (_, retired) =
        delete_registry_gc_candidates_in_transaction(&transaction, &[], project_paths).await?;
    transaction.commit().await?;
    Ok(retired)
}
