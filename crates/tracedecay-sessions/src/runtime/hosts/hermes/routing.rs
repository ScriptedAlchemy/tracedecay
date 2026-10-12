//! Turn-level project routing: structured tool-call project paths and session
//! cwd fallbacks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::runtime::shared::ProjectRootMatcher;

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
        let has_fallback = session_rows
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

pub(super) fn turn_project_locations(
    rows: &[HermesRow],
    project_root: &Path,
) -> HashMap<i64, &'static str> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let project_matcher = ProjectRootMatcher::new(project_root);
    let mut locations = HashMap::new();
    for session_rows in by_session.into_values() {
        let has_fallback = session_rows
            .iter()
            .any(|row| session_is_candidate_for_project(row, &project_matcher));
        let fallback_provenance = "session_cwd";
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_location(
                    &turn,
                    &project_matcher,
                    has_fallback,
                    fallback_provenance,
                    &mut locations,
                );
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_location(
            &turn,
            &project_matcher,
            has_fallback,
            fallback_provenance,
            &mut locations,
        );
    }
    locations
}

fn assign_turn_location(
    rows: &[&HermesRow],
    project_matcher: &ProjectRootMatcher,
    has_fallback: bool,
    fallback_provenance: &'static str,
    locations: &mut HashMap<i64, &'static str>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let explicit = !explicit_paths.is_empty()
        && explicit_paths
            .iter()
            .any(|path| project_matcher.contains(path));
    if explicit || (explicit_paths.is_empty() && has_fallback) {
        let provenance = if explicit {
            "tool_project_path"
        } else {
            fallback_provenance
        };
        locations.extend(rows.iter().map(|row| (row.id, provenance)));
    }
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

fn session_is_candidate_for_project(row: &HermesRow, project_matcher: &ProjectRootMatcher) -> bool {
    row.session_cwd.as_deref().is_some_and(|cwd| {
        let cwd = Path::new(cwd.trim());
        cwd.is_absolute() && project_matcher.contains(cwd)
    })
}
