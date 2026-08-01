//! Branch-scoped tools: `tracedecay_admin_branch_add`, `tracedecay_branch_list`, `tracedecay_branch_search`, `tracedecay_branch_diff`.

use super::*;

/// Daemon-only branch-add entry point used by the first-party CLI.
///
/// Branch preparation copies and syncs a graph database, so it must run inside
/// the managed daemon's database-authority scope rather than in the CLI process.
pub(crate) async fn handle_admin_branch_add(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let branch = require_admin_branch_name(&args)?;
    let outcome =
        TraceDecay::add_branch_tracking_with_options(cg.project_root(), branch, cg.open_options())
            .await?;
    let output = json!({ "outcome": admin_branch_add_outcome_name(&outcome) });
    Ok(ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&output).unwrap_or_default(),
            }]
        }),
        vec![],
    ))
}

fn require_admin_branch_name(args: &Value) -> Result<&str> {
    args.get("branch")
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: branch".to_string(),
        })
}

fn admin_branch_add_outcome_name(outcome: &crate::branch::BranchAddOutcome) -> &'static str {
    match outcome {
        crate::branch::BranchAddOutcome::NotIndexed => "not_indexed",
        crate::branch::BranchAddOutcome::AlreadyTracked => "already_tracked",
        crate::branch::BranchAddOutcome::Added => "added",
        crate::branch::BranchAddOutcome::Deferred => "deferred",
    }
}

/// Handles `tracedecay_branch_list` tool calls.
pub(crate) fn handle_branch_list(cg: &TraceDecay, args: &Value) -> ToolResult {
    let diagnostics = cg.branch_diagnostics();
    let mut result = serde_json::to_value(&diagnostics).unwrap_or(json!({}));
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "branch_count".to_string(),
            json!(diagnostics.tracked_branch_count),
        );
    }

    generic_tool_result(Some(cg.project_root()), args, &result, vec![])
}

/// Handles `tracedecay_branch_search` tool calls.
pub(crate) async fn handle_branch_search(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let branch =
        args.get("branch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: branch".to_string(),
            })?;
    let query =
        args.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            })?;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(500) as usize);

    let branch_cg = TraceDecay::open_branch_with_registered_configuration(
        cg.project_root(),
        branch,
        crate::tracedecay::TraceDecayOpenOptions::default(),
        cg.store_layout().clone(),
        cg.configuration_runtime().registered_database(),
        cg.profile_database().clone(),
        cg.store_runtime_registry().clone(),
    )
    .await?;
    let results = branch_cg.search(query, limit).await?;

    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r.node.id,
                "name": r.node.name,
                "kind": r.node.kind.as_str(),
                "file": r.node.file_path,
                "line": r.node.start_line,
                "signature": r.node.signature,
                "score": r.score,
                "branch": branch,
            })
        })
        .collect();

    let items = json!(items);
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &items,
        vec![],
    ))
}

/// Handles `tracedecay_branch_diff` tool calls.
///
/// Compares code graphs between two branches. For each symbol present in
/// either branch, reports whether it was added, removed, or changed
/// (signature differs).
pub(crate) async fn handle_branch_diff(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let project_root = cg.project_root();
    let tracedecay_dir = &cg.store_layout().data_root;

    // Resolve base and head branches
    let meta = crate::branch_meta::load_branch_meta(tracedecay_dir).ok_or_else(|| {
        TraceDecayError::Config {
            message: "no branch tracking configured — run `tracedecay branch add` first"
                .to_string(),
        }
    })?;

    let base_name = args
        .get("base")
        .and_then(|v| v.as_str())
        .unwrap_or(&meta.default_branch);
    let head_name = args
        .get("head")
        .and_then(|v| v.as_str())
        .or_else(|| cg.active_branch())
        .ok_or_else(|| TraceDecayError::Config {
            message: "cannot determine head branch — specify it explicitly".to_string(),
        })?;

    if base_name == head_name {
        // pr_context returns empty arrays for the same-ref case; do the same here
        // so callers get a consistent shape and can simply check the summary.
        let result = json!({
            "base": base_name,
            "head": head_name,
            "note": format!("base and head are the same branch: '{base_name}'"),
            "summary": { "added": 0, "removed": 0, "changed": 0 },
            "added": [],
            "removed": [],
            "changed": [],
        });
        return Ok(generic_tool_result(
            Some(cg.project_root()),
            &args,
            &result,
            vec![],
        ));
    }

    let file_filter = args.get("file").and_then(|v| v.as_str());
    let kind_filter = args.get("kind").and_then(|v| v.as_str());

    let base_cg = TraceDecay::open_branch_with_registered_configuration(
        project_root,
        base_name,
        crate::tracedecay::TraceDecayOpenOptions::default(),
        cg.store_layout().clone(),
        cg.configuration_runtime().registered_database(),
        cg.profile_database().clone(),
        cg.store_runtime_registry().clone(),
    )
    .await?;
    let head_cg = if cg.active_branch() == Some(head_name) && !cg.is_fallback() {
        None // use the already-open cg
    } else {
        Some(
            TraceDecay::open_branch_with_registered_configuration(
                project_root,
                head_name,
                crate::tracedecay::TraceDecayOpenOptions::default(),
                cg.store_layout().clone(),
                cg.configuration_runtime().registered_database(),
                cg.profile_database().clone(),
                cg.store_runtime_registry().clone(),
            )
            .await?,
        )
    };
    let head_ref = head_cg.as_ref().unwrap_or(cg);

    let base_files = base_cg.get_all_files().await?;
    let head_files = head_ref.get_all_files().await?;

    // Build file sets for filtering — only compare files present in either branch
    let base_file_set: HashSet<&str> = base_files.iter().map(|f| f.path.as_str()).collect();
    let head_file_set: HashSet<&str> = head_files.iter().map(|f| f.path.as_str()).collect();
    let all_files: HashSet<&str> = base_file_set.union(&head_file_set).copied().collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut touched = Vec::new();

    for file_path in &all_files {
        if let Some(filter) = file_filter
            && !file_path.starts_with(filter)
            && *file_path != filter
        {
            continue;
        }

        let base_nodes = base_cg.get_nodes_by_file(file_path).await?;
        let head_nodes = head_ref.get_nodes_by_file(file_path).await?;

        // Index by qualified_name for matching
        let base_map: HashMap<&str, &crate::types::Node> = base_nodes
            .iter()
            .map(|n| (n.qualified_name.as_str(), n))
            .collect();
        let head_map: HashMap<&str, &crate::types::Node> = head_nodes
            .iter()
            .map(|n| (n.qualified_name.as_str(), n))
            .collect();

        for (qn, node) in &head_map {
            if let Some(filter) = kind_filter
                && node.kind.as_str() != filter
            {
                continue;
            }
            if !base_map.contains_key(qn) {
                added.push(json!({
                    "name": node.name,
                    "qualified_name": node.qualified_name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                    "signature": node.signature,
                }));
                touched.push(node.file_path.clone());
            }
        }

        for (qn, node) in &base_map {
            if let Some(filter) = kind_filter
                && node.kind.as_str() != filter
            {
                continue;
            }
            if !head_map.contains_key(qn) {
                removed.push(json!({
                    "name": node.name,
                    "qualified_name": node.qualified_name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                    "signature": node.signature,
                }));
                touched.push(node.file_path.clone());
            }
        }

        // Changed: in both but signature differs
        for (qn, head_node) in &head_map {
            if let Some(filter) = kind_filter
                && head_node.kind.as_str() != filter
            {
                continue;
            }
            if let Some(base_node) = base_map.get(qn)
                && base_node.signature != head_node.signature
            {
                changed.push(json!({
                    "name": head_node.name,
                    "qualified_name": head_node.qualified_name,
                    "kind": head_node.kind.as_str(),
                    "file": head_node.file_path,
                    "line": head_node.start_line,
                    "base_signature": base_node.signature,
                    "head_signature": head_node.signature,
                }));
                touched.push(head_node.file_path.clone());
            }
        }
    }

    let result = json!({
        "base": base_name,
        "head": head_name,
        "summary": {
            "added": added.len(),
            "removed": removed.len(),
            "changed": changed.len(),
        },
        "added": added,
        "removed": removed,
        "changed": changed,
    });

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &result,
        touched_files,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_branch_add_requires_a_nonempty_branch_name() {
        for args in [serde_json::json!({}), serde_json::json!({ "branch": "" })] {
            let error = require_admin_branch_name(&args)
                .expect_err("branch add request without branch must fail");
            assert!(error.to_string().contains("branch"));
        }
    }

    #[test]
    fn admin_branch_add_outcomes_have_stable_wire_names() {
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::NotIndexed),
            "not_indexed"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::AlreadyTracked),
            "already_tracked"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::Added),
            "added"
        );
        assert_eq!(
            admin_branch_add_outcome_name(&crate::branch::BranchAddOutcome::Deferred),
            "deferred"
        );
    }
}
