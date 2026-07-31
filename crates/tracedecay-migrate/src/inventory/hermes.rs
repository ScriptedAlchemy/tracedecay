use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::artifacts::file_size;
use super::project::{
    InventoryScanOptions, canonicalize_lossy, inspect_data_dir_candidate, push_integrity_issue,
};
use super::sqlite::sqlite_quick_check;
use crate::root_seam::config::TRACEDECAY_DIR;
use crate::root_seam::errors::Result;
use tracedecay_automation::skill_frontmatter::decode_yaml_scalar;
use crate::inventory::{
    InventoryIntegrityMode, InventoryStoreAuthority, RegistryStatus, SkippedPath, StoreArtifact,
    StoreBrand, StoreInventory, StoreRole, StoreStatus,
};

pub(super) async fn scan_hermes_sources(
    include_default_home: bool,
    options: InventoryScanOptions,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    let mut seen_profiles = HashSet::new();
    let mut seen_state_dbs = HashSet::new();
    for hermes_home in hermes_home_candidates(include_default_home) {
        inspect_hermes_profile_dir(
            &hermes_home,
            options,
            seen_data_dirs,
            &mut seen_profiles,
            &mut seen_state_dbs,
            stores,
            skipped,
        )
        .await?;

        let profiles_dir = hermes_home.join("profiles");
        let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let mut profile_dir = entry.path();
            if file_type.is_symlink() {
                if !options.follow_symlinks {
                    skipped.push(SkippedPath {
                        path: profile_dir,
                        reason: "symlink".to_string(),
                    });
                    continue;
                }
                if !profile_dir.is_dir() {
                    continue;
                }
                profile_dir = profile_dir.canonicalize().unwrap_or(profile_dir);
            } else if !file_type.is_dir() {
                continue;
            }
            inspect_hermes_profile_dir(
                &profile_dir,
                options,
                seen_data_dirs,
                &mut seen_profiles,
                &mut seen_state_dbs,
                stores,
                skipped,
            )
            .await?;
        }
    }
    Ok(())
}

async fn inspect_hermes_profile_dir(
    profile_dir: &Path,
    options: InventoryScanOptions,
    seen_data_dirs: &mut HashSet<PathBuf>,
    seen_profiles: &mut HashSet<PathBuf>,
    seen_state_dbs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    if !profile_dir.is_dir() {
        return Ok(());
    }
    let profile_key = canonicalize_lossy(profile_dir);
    if !seen_profiles.insert(profile_key) {
        return Ok(());
    }

    inspect_data_dir_candidate(
        profile_dir,
        TRACEDECAY_DIR,
        options,
        seen_data_dirs,
        stores,
        skipped,
        StoreRole::HermesProfileStore,
    )
    .await?;
    inspect_hermes_state_db(profile_dir, seen_state_dbs, stores, options.integrity).await;

    let config_path = profile_dir.join("config.yaml");
    match read_hermes_project_pin(&config_path) {
        HermesProjectPin::Pinned(project_root) => {
            inspect_data_dir_candidate(
                &project_root,
                TRACEDECAY_DIR,
                options,
                seen_data_dirs,
                stores,
                skipped,
                StoreRole::CodeProjectStore,
            )
            .await?;
        }
        HermesProjectPin::Malformed(reason) => skipped.push(SkippedPath {
            path: config_path,
            reason,
        }),
        HermesProjectPin::Absent => {}
    }

    Ok(())
}

async fn inspect_hermes_state_db(
    profile_dir: &Path,
    seen_state_dbs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    integrity: InventoryIntegrityMode,
) {
    let db_path = profile_dir.join("state.db");
    if !db_path.is_file() {
        return;
    }
    let key = canonicalize_lossy(&db_path);
    if !seen_state_dbs.insert(key) {
        return;
    }
    let mut statuses = Vec::new();
    match integrity {
        InventoryIntegrityMode::MetadataOnly => statuses.push(StoreStatus::IntegrityUnchecked),
        InventoryIntegrityMode::Full => {
            push_integrity_issue(
                &mut statuses,
                &db_path,
                InventoryStoreAuthority::ExternalSource,
                sqlite_quick_check(&db_path).await,
            );
        }
    }
    if statuses.is_empty() {
        statuses.push(StoreStatus::Ok);
    }
    stores.push(StoreInventory {
        project_root: profile_dir.to_path_buf(),
        data_dir: profile_dir.to_path_buf(),
        db_path: db_path.clone(),
        brand: StoreBrand::TraceDecay,
        role: StoreRole::HermesStateDbSource,
        registry_status: RegistryStatus::Unregistered,
        size_bytes: file_size(&db_path),
        statuses,
        artifacts: vec![StoreArtifact {
            kind: "hermes_state_db".to_string(),
            path: db_path.clone(),
            size_bytes: file_size(&db_path),
        }],
    });
}

fn hermes_home_candidates(include_default_home: bool) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    if include_default_home {
        let Some(home) = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
        else {
            return candidates;
        };
        push_unique_path(&mut candidates, &mut seen, home.join(".hermes"));
    }
    candidates
}

fn push_unique_path(candidates: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    let key = canonicalize_lossy(&path);
    if seen.insert(key) {
        candidates.push(path);
    }
}

/// What a Hermes `config.yaml` says about the `tracedecay` plugin's pinned
/// project root.
enum HermesProjectPin {
    /// Unreadable config, no `plugins` section, no `tracedecay` block, or no
    /// `project_root` key — nothing to inventory and nothing to report.
    Absent,
    /// A `project_root` is pinned but its YAML scalar does not decode. The
    /// project store behind it is real and would be missed by a migration, so
    /// this has to surface rather than read as "no pin".
    Malformed(String),
    Pinned(PathBuf),
}

fn read_hermes_project_pin(config_path: &Path) -> HermesProjectPin {
    let Ok(config) = std::fs::read_to_string(config_path) else {
        return HermesProjectPin::Absent;
    };
    let lines = config.lines().collect::<Vec<_>>();
    let Some((plugins_start, plugins_end)) = find_top_level_section(&lines, "plugins") else {
        return HermesProjectPin::Absent;
    };
    read_project_pin_from_plugin_block(&lines, plugins_start, plugins_end, "tracedecay")
}

fn read_project_pin_from_plugin_block(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    plugin_key: &str,
) -> HermesProjectPin {
    let Some((block_start, block_end)) =
        find_indented_section(lines, plugins_start + 1, plugins_end, 2, plugin_key)
    else {
        return HermesProjectPin::Absent;
    };
    let pinned = lines
        .iter()
        .take(block_end)
        .skip(block_start + 1)
        .find_map(|line| line.trim().strip_prefix("project_root:"))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match pinned {
        None => HermesProjectPin::Absent,
        Some(value) => match decode_yaml_scalar(value) {
            Ok(decoded) => HermesProjectPin::Pinned(PathBuf::from(decoded.into_owned())),
            Err(error) => {
                HermesProjectPin::Malformed(format!("malformed tracedecay project_root: {error}"))
            }
        },
    }
}

fn find_top_level_section(lines: &[&str], key: &str) -> Option<(usize, usize)> {
    let section_start = lines
        .iter()
        .position(|line| line.trim() == format!("{key}:"))?;
    let section_end = lines
        .iter()
        .enumerate()
        .skip(section_start + 1)
        .find_map(|(index, line)| {
            (!line.trim().is_empty() && leading_spaces(line) == 0).then_some(index)
        })
        .unwrap_or(lines.len());
    Some((section_start, section_end))
}

fn find_indented_section(
    lines: &[&str],
    start: usize,
    end: usize,
    indent: usize,
    key: &str,
) -> Option<(usize, usize)> {
    let marker = format!("{key}:");
    let section_start =
        lines
            .iter()
            .enumerate()
            .take(end)
            .skip(start)
            .find_map(|(index, line)| {
                (leading_spaces(line) == indent && line.trim() == marker).then_some(index)
            })?;
    let section_end = lines
        .iter()
        .enumerate()
        .take(end)
        .skip(section_start + 1)
        .find_map(|(index, line)| {
            (!line.trim().is_empty() && leading_spaces(line) <= indent).then_some(index)
        })
        .unwrap_or(end);
    Some((section_start, section_end))
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}
