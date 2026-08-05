//! Turn-level project routing: structured tool-call project paths, session
//! cwd fallbacks, and multi-destination matching.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::runtime::shared::{ProjectMembership, ProjectRootMatcher};

use super::ingest::HermesProfileSource;
use super::rows::HermesRow;

pub(super) fn user_turn_locations(
    rows: &[HermesRow],
    source: &HermesProfileSource,
) -> HashSet<i64> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashSet::new();
    for session_rows in by_session.into_values() {
        let has_fallback = source.legacy_project_pin.is_some()
            || session_rows
                .iter()
                .any(|row| Path::new(row.session_cwd.as_deref().unwrap_or_default()).is_absolute())
            || source.state_db.parent().is_some();
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_user_turn(&turn, has_fallback, &mut locations);
                turn.clear();
            }
            turn.push(row);
        }
        assign_user_turn(&turn, has_fallback, &mut locations);
    }
    locations
}

fn assign_user_turn(rows: &[&HermesRow], has_fallback: bool, locations: &mut HashSet<i64>) {
    if rows
        .iter()
        .flat_map(|row| structured_tool_project_paths(row))
        .next_back()
        .is_none()
        && !has_fallback
    {
        return;
    }
    locations.extend(rows.iter().map(|row| row.id));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HermesRoutingDeferred {
    pub reason: tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown,
}

pub(super) fn turn_project_locations(
    rows: &[HermesRow],
    project_matcher: &ProjectRootMatcher,
    source: &HermesProfileSource,
) -> Result<HashMap<i64, &'static str>, HermesRoutingDeferred> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashMap::new();
    for session_rows in by_session.into_values() {
        let has_fallback =
            session_membership(&session_rows, project_matcher, source)?.definitive() == Some(true);
        let fallback_provenance = source
            .legacy_project_pin
            .as_ref()
            .map_or("session_cwd", |_| "profile_pin");
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_location(
                    &turn,
                    project_matcher,
                    has_fallback,
                    fallback_provenance,
                    &mut locations,
                )?;
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_location(
            &turn,
            project_matcher,
            has_fallback,
            fallback_provenance,
            &mut locations,
        )?;
    }
    Ok(locations)
}

pub(super) struct DestinationTurnLocations {
    pub by_row_id: HashMap<i64, &'static str>,
}

pub(super) fn turn_project_locations_for_destinations(
    rows: &[HermesRow],
    destination_matchers: &[ProjectRootMatcher],
    source: &HermesProfileSource,
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Result<Vec<DestinationTurnLocations>, HermesRoutingDeferred> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = (0..destination_matchers.len())
        .map(|_| DestinationTurnLocations {
            by_row_id: HashMap::new(),
        })
        .collect::<Vec<_>>();
    for session_rows in by_session.into_values() {
        let fallback_provenance = source
            .legacy_project_pin
            .as_ref()
            .map_or("session_cwd", |_| "profile_pin");
        let fallback_candidates = if let Some(pin) = source.legacy_project_pin.as_ref() {
            vec![pin.clone()]
        } else {
            let mut seen = BTreeSet::new();
            session_rows
                .iter()
                .filter_map(|row| {
                    let cwd = PathBuf::from(row.session_cwd.as_deref()?.trim());
                    (cwd.is_absolute() && seen.insert(cwd.clone())).then_some(cwd)
                })
                .collect::<Vec<_>>()
        };
        let mut fallbacks = vec![false; destination_matchers.len()];
        for cwd in fallback_candidates {
            for destination_index in
                matching_destinations(&cwd, destination_matchers, destination_routes)?
            {
                fallbacks[destination_index] = true;
            }
        }
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_locations_for_destinations(
                    &turn,
                    destination_matchers,
                    &fallbacks,
                    fallback_provenance,
                    &mut locations,
                    destination_routes,
                )?;
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_locations_for_destinations(
            &turn,
            destination_matchers,
            &fallbacks,
            fallback_provenance,
            &mut locations,
            destination_routes,
        )?;
    }
    Ok(locations)
}

fn assign_turn_locations_for_destinations(
    rows: &[&HermesRow],
    destination_matchers: &[ProjectRootMatcher],
    fallbacks: &[bool],
    fallback_provenance: &'static str,
    locations: &mut [DestinationTurnLocations],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Result<(), HermesRoutingDeferred> {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let mut selected = vec![false; destination_matchers.len()];
    let has_explicit_paths = !explicit_paths.is_empty();
    if has_explicit_paths {
        for path in explicit_paths {
            for destination_index in
                matching_destinations(&path, destination_matchers, destination_routes)?
            {
                selected[destination_index] = true;
            }
        }
    } else {
        selected.copy_from_slice(fallbacks);
    }
    let provenance = if has_explicit_paths {
        "tool_project_path"
    } else {
        fallback_provenance
    };
    for (selected, destination) in selected.into_iter().zip(locations) {
        if selected {
            destination
                .by_row_id
                .extend(rows.iter().map(|row| (row.id, provenance)));
        }
    }
    Ok(())
}

fn matching_destinations(
    path: &Path,
    destination_matchers: &[ProjectRootMatcher],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Result<Vec<usize>, HermesRoutingDeferred> {
    if let Some(indices) = destination_routes.get(path) {
        return Ok(indices.clone());
    }
    let mut indices = Vec::new();
    for (index, matcher) in destination_matchers.iter().enumerate() {
        match matcher.contains(path) {
            ProjectMembership::Match => indices.push(index),
            ProjectMembership::NoMatch => {}
            ProjectMembership::Unknown(reason) => return Err(HermesRoutingDeferred { reason }),
        }
    }
    destination_routes.insert(path.to_path_buf(), indices.clone());
    Ok(indices)
}

fn assign_turn_location(
    rows: &[&HermesRow],
    project_matcher: &ProjectRootMatcher,
    has_fallback: bool,
    fallback_provenance: &'static str,
    locations: &mut HashMap<i64, &'static str>,
) -> Result<(), HermesRoutingDeferred> {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let explicit = if explicit_paths.is_empty() {
        false
    } else {
        definitive_match(explicit_paths.iter().map(PathBuf::as_path), project_matcher)?
    };
    if explicit || (explicit_paths.is_empty() && has_fallback) {
        let provenance = if explicit {
            "tool_project_path"
        } else {
            fallback_provenance
        };
        locations.extend(rows.iter().map(|row| (row.id, provenance)));
    }
    Ok(())
}

fn structured_tool_project_paths(row: &HermesRow) -> Vec<PathBuf> {
    let Some(raw) = row.tool_calls.as_deref() else {
        return Vec::new();
    };
    let Ok(calls) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let calls = calls.as_array().map_or(&[] as &[Value], Vec::as_slice);
    for call in calls {
        let arguments = call
            .pointer("/function/arguments")
            .or_else(|| call.get("arguments"));
        let parsed;
        let arguments = match arguments {
            Some(Value::String(raw)) => {
                parsed = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
                &parsed
            }
            Some(value) => value,
            None => continue,
        };
        for value in [
            arguments.get("project_root"),
            arguments.get("project_path"),
            arguments.pointer("/project_selector/path"),
            arguments.get("cwd"),
            arguments.get("workdir"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                paths.push(path);
            }
        }
    }
    paths
}

fn session_membership(
    rows: &[&HermesRow],
    project_matcher: &ProjectRootMatcher,
    source: &HermesProfileSource,
) -> Result<ProjectMembership, HermesRoutingDeferred> {
    if source.legacy_project_pin.is_some() {
        return Ok(ProjectMembership::Match);
    }
    definitive_membership(
        rows.iter().filter_map(|row| {
            let cwd = Path::new(row.session_cwd.as_deref()?.trim());
            cwd.is_absolute().then_some(cwd)
        }),
        project_matcher,
    )
}

fn definitive_match<'a>(
    paths: impl Iterator<Item = &'a Path>,
    project_matcher: &ProjectRootMatcher,
) -> Result<bool, HermesRoutingDeferred> {
    Ok(matches!(
        definitive_membership(paths, project_matcher)?,
        ProjectMembership::Match
    ))
}

fn definitive_membership<'a>(
    paths: impl Iterator<Item = &'a Path>,
    project_matcher: &ProjectRootMatcher,
) -> Result<ProjectMembership, HermesRoutingDeferred> {
    let mut unknown = None;
    for path in paths {
        match project_matcher.contains(path) {
            ProjectMembership::Match => return Ok(ProjectMembership::Match),
            ProjectMembership::NoMatch => {}
            ProjectMembership::Unknown(reason) => {
                unknown.get_or_insert(reason);
            }
        }
    }
    match unknown {
        Some(reason) => Err(HermesRoutingDeferred { reason }),
        None => Ok(ProjectMembership::NoMatch),
    }
}
