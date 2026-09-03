//! Public Hermes sweep entry points and profile `state.db` discovery.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tracedecay_domain::{ObservationScopeV1, ProjectId};

use crate::admission::HostAdmission;
use crate::host_ports::hermes_profile_pin::resolve as read_config_pinned_project_root;
use crate::observation::ObservationCancellation;
use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::shared::{TranscriptIngestStats, path_belongs_to_project};
use crate::runtime::source::run_blocking_transcript_section;

use super::DEFAULT_HERMES_SWEEP_BYTES;
use super::coverage::{
    drain_hermes_projections_with_admission,
    drain_hermes_projections_with_admission_and_cancellation,
};
use super::state_db::{
    try_ingest_state_db_bounded_with_admission, try_ingest_state_db_for_projects,
    try_ingest_user_state_db_bounded_with_admission,
};

fn new_sweep_budget(max_new_bytes: Option<u64>) -> IngestByteBudget {
    IngestByteBudget::bounded(max_new_bytes.unwrap_or(DEFAULT_HERMES_SWEEP_BYTES))
}

/// Default Hermes profile homes under the resolved user home.
///
/// Missing home is a typed absence (`None`), never an empty successful sweep.
fn hermes_homes() -> Option<Vec<PathBuf>> {
    hermes_homes_from(crate::runtime::home_dir())
}

fn hermes_homes_from(home: Option<PathBuf>) -> Option<Vec<PathBuf>> {
    Some(vec![home?.join(".hermes")])
}

/// Result of a Hermes sweep with one aggregate logical source-byte budget.
#[derive(Debug, Default, Clone)]
pub struct HermesSweepOutcome {
    pub stats: TranscriptIngestStats,
    pub bytes_consumed: u64,
    pub deferred_by_byte_cap: bool,
}

/// Ingests Hermes sessions proven to belong to `project_root` into the
/// daemon-authorized canonical `project_id` scope in `db`.
///
/// Discovery is bounded to the default user integration (`~/.hermes`) and its
/// immediate named-profile children; environment overrides are ignored.
pub async fn ingest_for_project(
    admission: &dyn HostAdmission,
    project_root: &Path,
    project_id: ProjectId,
) -> Option<TranscriptIngestStats> {
    Some(
        ingest_for_project_capped(admission, project_root, project_id, None)
            .await?
            .stats,
    )
}

/// [`ingest_for_project`] with one aggregate logical source-byte budget shared
/// across every discovered Hermes profile.
pub async fn ingest_for_project_capped(
    admission: &dyn HostAdmission,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> Option<HermesSweepOutcome> {
    ingest_for_project_capped_with_admission(project_root, project_id, admission, max_new_bytes)
        .await
}

/// Project ingestion with the already-prepared central host-admission facade.
///
/// The project provider composes repository provenance once from the
/// authoritative project root and passes it through this path. Direct and
/// profile entrypoints intentionally continue to construct their own facade.
pub async fn ingest_for_project_capped_with_admission(
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> Option<HermesSweepOutcome> {
    ingest_for_project_capped_with_admission_and_cancellation(
        project_root,
        project_id,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn ingest_for_project_capped_with_admission_and_cancellation(
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> Option<HermesSweepOutcome> {
    let homes = hermes_homes()?;
    Some(
        ingest_homes_capped_with_admission_and_cancellation(
            &homes,
            project_root,
            project_id,
            admission,
            max_new_bytes,
            cancellation,
        )
        .await,
    )
}

/// One project-store destination for a shared Hermes source sweep.
#[derive(Clone)]
pub struct ProjectIngestDestination<'a> {
    pub admission: &'a dyn HostAdmission,
    pub project_root: &'a Path,
    pub project_id: ProjectId,
}

/// Ingests Hermes history for several registered projects while opening and
/// scanning each profile `state.db` only once. Every destination retains its
/// own authoritative source cursor, advanced atomically with canonical
/// observation persistence or typed complete-record coverage.
pub async fn ingest_for_projects(
    destinations: &[ProjectIngestDestination<'_>],
) -> Option<TranscriptIngestStats> {
    let homes = hermes_homes()?;
    Some(ingest_homes_for_projects(&homes, destinations).await)
}

/// Test seam for [`ingest_for_projects`].
#[hotpath::measure(label = "sessions.hosts.hermes.ingest_projects", future = true)]
pub async fn ingest_homes_for_projects(
    hermes_homes: &[PathBuf],
    destinations: &[ProjectIngestDestination<'_>],
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    let mut budget = new_sweep_budget(None);
    let sources = hotpath::measure_block!(
        "sessions.hosts.hermes.discover_blocking",
        run_blocking_transcript_section(|| all_profile_sources(hermes_homes))
    );
    for source in sources {
        if budget.exhausted() {
            budget.defer();
            break;
        }
        let eligible = hotpath::measure_block!(
            "sessions.hosts.hermes.scope_profiles_blocking",
            run_blocking_transcript_section(|| {
                destinations
                    .iter()
                    .filter(|destination| {
                        source_is_candidate_for_project(&source, destination.project_root)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
        );
        if eligible.is_empty() {
            continue;
        }
        match try_ingest_state_db_for_projects(&source, &eligible, &mut budget).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping shared Hermes transcript source"
            ),
        }
    }
    for destination in destinations {
        let scope = ObservationScopeV1::Project {
            project_id: destination.project_id.clone(),
        };
        if let Err(error) =
            drain_hermes_projections_with_admission(destination.admission, &scope).await
        {
            tracing::debug!(error, "Hermes shared projection drain deferred");
        }
    }
    stats
}

/// [`ingest_for_project`] with explicit Hermes home directories — the test
/// seam for pointing the sweep at a temporary home instead of the real
/// `~/.hermes`.
pub async fn ingest_homes(
    admission: &dyn HostAdmission,
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_id: ProjectId,
) -> TranscriptIngestStats {
    ingest_homes_capped(admission, hermes_homes, project_root, project_id, None)
        .await
        .stats
}

pub async fn ingest_homes_capped(
    admission: &dyn HostAdmission,
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    ingest_homes_capped_with_admission(
        hermes_homes,
        project_root,
        project_id,
        admission,
        max_new_bytes,
    )
    .await
}

pub async fn ingest_homes_capped_with_admission(
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    ingest_homes_capped_with_admission_and_cancellation(
        hermes_homes,
        project_root,
        project_id,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

#[hotpath::measure(label = "sessions.hosts.hermes.ingest_project", future = true)]
pub(super) async fn ingest_homes_capped_with_admission_and_cancellation(
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> HermesSweepOutcome {
    let mut outcome = HermesSweepOutcome::default();
    if cancellation.is_cancelled() {
        return outcome;
    }
    let mut budget = new_sweep_budget(max_new_bytes);
    let sources = hotpath::measure_block!(
        "sessions.hosts.hermes.discover_blocking",
        run_blocking_transcript_section(|| candidate_state_dbs(hermes_homes, project_root))
    );
    for source in sources {
        if cancellation.is_cancelled() {
            break;
        }
        if budget.exhausted() {
            budget.defer();
            break;
        }
        match try_ingest_state_db_bounded_with_admission(
            &source,
            project_root,
            project_id.clone(),
            admission,
            &mut budget,
            cancellation,
        )
        .await
        {
            Ok(source_stats) => outcome.stats = outcome.stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping Hermes transcript source"
            ),
        }
    }
    let scope = ObservationScopeV1::Project { project_id };
    if !cancellation.is_cancelled()
        && let Err(error) = drain_hermes_projections_with_admission_and_cancellation(
            admission,
            &scope,
            cancellation,
        )
        .await
    {
        tracing::debug!(error, "Hermes project projection drain deferred");
    }
    outcome.bytes_consumed = budget.consumed();
    outcome.deferred_by_byte_cap = budget.deferred();
    outcome
}

/// Ingests canonical historical Hermes observations into the profile scope with
/// one aggregate logical source-byte budget shared across every discovered
/// Hermes profile. Project ingestion separately admits each turn to every
/// registered project it touched using the same stable message IDs.
pub async fn ingest_user_sessions_capped(
    admission: &dyn HostAdmission,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> Option<HermesSweepOutcome> {
    let homes = hermes_homes()?;
    Some(ingest_user_homes_capped(admission, &homes, registered_roots, max_new_bytes).await)
}

pub async fn ingest_user_sessions_capped_with_admission(
    admission: &dyn HostAdmission,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> Option<HermesSweepOutcome> {
    let homes = hermes_homes()?;
    Some(
        ingest_user_homes_capped_with_admission(
            admission,
            &homes,
            registered_roots,
            max_new_bytes,
            cancellation,
        )
        .await,
    )
}

pub async fn ingest_user_homes(
    admission: &dyn HostAdmission,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
) -> TranscriptIngestStats {
    ingest_user_homes_capped(admission, hermes_homes, registered_roots, None)
        .await
        .stats
}

pub async fn ingest_user_homes_capped(
    admission: &dyn HostAdmission,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    ingest_user_homes_capped_with_admission(
        admission,
        hermes_homes,
        registered_roots,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

#[hotpath::measure(label = "sessions.hosts.hermes.ingest_user", future = true)]
async fn ingest_user_homes_capped_with_admission(
    admission: &dyn HostAdmission,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> HermesSweepOutcome {
    let mut outcome = HermesSweepOutcome::default();
    if cancellation.is_cancelled() {
        return outcome;
    }
    let mut budget = new_sweep_budget(max_new_bytes);
    let sources = hotpath::measure_block!(
        "sessions.hosts.hermes.discover_blocking",
        run_blocking_transcript_section(|| all_profile_sources(hermes_homes))
    );
    for source in sources {
        if cancellation.is_cancelled() {
            break;
        }
        if budget.exhausted() {
            budget.defer();
            break;
        }
        match try_ingest_user_state_db_bounded_with_admission(
            admission,
            &source,
            registered_roots,
            &mut budget,
            cancellation,
        )
        .await
        {
            Ok(source_stats) => outcome.stats = outcome.stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping projectless Hermes transcript source"
            ),
        }
    }
    if !cancellation.is_cancelled()
        && let Err(error) = drain_hermes_projections_with_admission_and_cancellation(
            admission,
            &ObservationScopeV1::Profile,
            cancellation,
        )
        .await
    {
        tracing::debug!(error, "Hermes profile projection drain deferred");
    }
    outcome.bytes_consumed = budget.consumed();
    outcome.deferred_by_byte_cap = budget.deferred();
    outcome
}

/// Strict one-time import for a legacy profile whose project pin was already
/// resolved by the migration layer. Unlike the normal catch-up sweep, any
/// open/query/write failure is returned so callers retain the pin and source.
#[hotpath::measure(label = "sessions.hosts.hermes.ingest_legacy", future = true)]
pub async fn ingest_legacy_pinned_profile(
    admission: &dyn HostAdmission,
    profile_dir: &Path,
    project_root: &Path,
    project_id: ProjectId,
) -> Result<TranscriptIngestStats, String> {
    let source = hotpath::measure_block!(
        "sessions.hosts.hermes.prepare_legacy_profile_blocking",
        run_blocking_transcript_section(|| {
            let state_db = profile_dir.join("state.db");
            if !state_db.is_file() {
                return Ok::<Option<HermesProfileSource>, String>(None);
            }
            let legacy_project_pin =
                read_config_pinned_project_root(&profile_dir.join("config.yaml"))
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        format!(
                            "legacy Hermes state store '{}' has no project pin",
                            state_db.display()
                        )
                    })?;
            Ok(Some(HermesProfileSource {
                state_db,
                legacy_project_pin: Some(legacy_project_pin),
                profile: profile_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string),
            }))
        })
    )?;
    let Some(source) = source else {
        return Ok(TranscriptIngestStats::default());
    };
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let mut budget = new_sweep_budget(None);
    let stats = try_ingest_state_db_bounded_with_admission(
        &source,
        project_root,
        project_id,
        admission,
        &mut budget,
        &ObservationCancellation::default(),
    )
    .await?;
    if budget.deferred() {
        return Err(format!(
            "legacy Hermes state store '{}' exceeded the bounded import sweep",
            source.state_db.display()
        ));
    }
    drain_hermes_projections_with_admission(admission, &scope).await?;
    Ok(stats)
}

/// Locates the `state.db` of every profile that maps to `project_root`.
///
/// A legacy project pin may associate an entire profile. Otherwise the
/// profile is only a bounded candidate source and each session must carry a
/// matching code-project cwd.
///
pub(super) struct HermesProfileSource {
    pub state_db: PathBuf,
    pub legacy_project_pin: Option<PathBuf>,
    pub profile: Option<String>,
}

#[hotpath::measure(label = "sessions.hosts.hermes.discover_profiles")]
fn all_profile_sources(hermes_homes: &[PathBuf]) -> Vec<HermesProfileSource> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for home in hermes_homes {
        let mut profiles = vec![(home.clone(), None)];
        if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
            profiles.extend(entries.filter_map(|entry| {
                let path = entry.ok()?.path();
                path.is_dir().then(|| {
                    let name = path.file_name()?.to_str()?.to_string();
                    Some((path, Some(name)))
                })?
            }));
        }
        for (profile_dir, profile) in profiles {
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    legacy_project_pin: read_config_pinned_project_root(
                        &profile_dir.join("config.yaml"),
                    )
                    .map(PathBuf::from),
                    profile,
                });
            }
        }
    }
    out
}

#[hotpath::measure(label = "sessions.hosts.hermes.discover_candidates")]
fn candidate_state_dbs(hermes_homes: &[PathBuf], project_root: &Path) -> Vec<HermesProfileSource> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let project_is_real = tracedecay_runtime_core::worktree::git_worktree_root(project_root)
        .is_some()
        || tracedecay_runtime_core::config::has_project_database(project_root);
    for home in hermes_homes {
        let mut candidates: Vec<(PathBuf, Option<String>)> = vec![(home.clone(), None)];
        if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
            let mut profiles = entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    entry.file_type().ok()?.is_dir().then(|| entry.path())
                })
                .collect::<Vec<_>>();
            profiles.sort();
            for profile_dir in profiles {
                let name = profile_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
                candidates.push((profile_dir, name));
            }
        }
        for (profile_dir, profile) in candidates {
            let legacy_project_pin =
                read_config_pinned_project_root(&profile_dir.join("config.yaml"))
                    .map(PathBuf::from);
            if legacy_project_pin
                .as_deref()
                .is_some_and(|pin| !path_belongs_to_project(pin, project_root))
                || (legacy_project_pin.is_none() && !project_is_real)
            {
                continue;
            }
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    legacy_project_pin,
                    profile,
                });
            }
        }
    }
    out
}

fn source_is_candidate_for_project(source: &HermesProfileSource, project_root: &Path) -> bool {
    if source
        .legacy_project_pin
        .as_deref()
        .is_some_and(|pin| !path_belongs_to_project(pin, project_root))
    {
        return false;
    }
    source.legacy_project_pin.is_some()
        || tracedecay_runtime_core::worktree::git_worktree_root(project_root).is_some()
        || tracedecay_runtime_core::config::has_project_database(project_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_sweeps_have_a_finite_aggregate_budget() {
        let default = new_sweep_budget(None);
        assert_eq!(default.remaining(), Some(DEFAULT_HERMES_SWEEP_BYTES));

        let explicit = new_sweep_budget(Some(17));
        assert_eq!(explicit.remaining(), Some(17));
    }

    #[test]
    fn missing_home_is_typed_absence_not_empty_homes() {
        assert_eq!(hermes_homes_from(None), None);
    }

    #[test]
    fn resolved_home_points_at_the_default_hermes_profile() {
        let home = PathBuf::from("/tmp/operator-home");
        assert_eq!(
            hermes_homes_from(Some(home.clone())),
            Some(vec![home.join(".hermes")])
        );
    }
}
