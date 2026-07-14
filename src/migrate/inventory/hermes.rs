use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::artifacts::file_size;
use super::model::{
    RegistryStatus, SkippedPath, StoreArtifact, StoreBrand, StoreInventory, StoreRole, StoreStatus,
};
use super::project::{canonicalize_lossy, inspect_data_dir_candidate};
use super::sqlite::sqlite_quick_check;
use crate::config::TRACEDECAY_DIR;
use crate::errors::Result;

pub(super) async fn scan_hermes_sources(
    include_default_home: bool,
    follow_symlinks: bool,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    let mut seen_profiles = HashSet::new();
    let mut seen_state_dbs = HashSet::new();
    for hermes_home in hermes_home_candidates(include_default_home) {
        inspect_hermes_profile_dir(
            &hermes_home,
            follow_symlinks,
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
                if !follow_symlinks {
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
                follow_symlinks,
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
    follow_symlinks: bool,
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
        follow_symlinks,
        seen_data_dirs,
        stores,
        skipped,
        StoreRole::HermesProfileStore,
    )
    .await?;
    inspect_hermes_state_db(profile_dir, seen_state_dbs, stores).await;

    if let Some(project_root) = read_hermes_project_pin(&profile_dir.join("config.yaml")) {
        inspect_data_dir_candidate(
            &project_root,
            TRACEDECAY_DIR,
            follow_symlinks,
            seen_data_dirs,
            stores,
            skipped,
            StoreRole::CodeProjectStore,
        )
        .await?;
    }

    Ok(())
}

async fn inspect_hermes_state_db(
    profile_dir: &Path,
    seen_state_dbs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
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
    if !sqlite_quick_check(&db_path).await {
        statuses.push(StoreStatus::Corrupt);
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

fn read_hermes_project_pin(config_path: &Path) -> Option<PathBuf> {
    let config = std::fs::read_to_string(config_path).ok()?;
    let lines = config.lines().collect::<Vec<_>>();
    let (plugins_start, plugins_end) = find_top_level_section(&lines, "plugins")?;
    read_project_pin_from_plugin_block(&lines, plugins_start, plugins_end, "tracedecay")
        .map(PathBuf::from)
}

fn read_project_pin_from_plugin_block(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    plugin_key: &str,
) -> Option<String> {
    let (block_start, block_end) =
        find_indented_section(lines, plugins_start + 1, plugins_end, 2, plugin_key)?;
    lines
        .iter()
        .take(block_end)
        .skip(block_start + 1)
        .find_map(|line| line.trim().strip_prefix("project_root:"))
        .and_then(parse_yaml_scalar)
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

fn parse_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value).ok();
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return Some(value[1..value.len() - 1].replace("''", "'"));
    }
    Some(value.to_string())
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}
