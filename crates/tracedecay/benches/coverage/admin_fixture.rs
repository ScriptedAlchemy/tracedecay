//! Admin read assertions against the isolated fixture's native and durable state.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay_automation_runtime::automation::managed_skills::managed_skill_dir;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunLedgerRecord, run_ledger_path,
};

use crate::queries::QueryContext;

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "admin fixture Git read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(
        &std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?,
    )
    .map_err(|error| format!("{}: {error}", path.display()))
}

fn same_path(value: &Value, expected: &Path) -> bool {
    value
        .as_str()
        .is_some_and(|path| Path::new(path) == expected)
}

fn check_project(project: &Value, root: &Path, project_id: &str) -> Result<(), String> {
    if project["project_id"] != project_id
        || !same_path(&project["canonical_root"], root)
        || !same_path(
            &project["git_common_dir"],
            &PathBuf::from(git(
                root,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )?),
        )
        || project["head_branch"] != git(root, &["branch", "--show-current"])?
        || project["is_active"] != true
    {
        return Err("project registry did not identify the actual active fixture checkout".into());
    }
    Ok(())
}

pub(crate) fn verify_admin_fixture(
    ctx: &QueryContext,
    root: &Path,
    tool: &str,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    if !matches!(
        tool,
        "tracedecay_status"
            | "tracedecay_active_project"
            | "tracedecay_project_context"
            | "tracedecay_project_list"
            | "tracedecay_project_search"
            | "tracedecay_config"
            | "tracedecay_runtime"
            | "tracedecay_storage_status"
            | "tracedecay_health_read"
            | "tracedecay_skill_list"
            | "tracedecay_automation_run_list"
            | "tracedecay_hermes_skill_bridge"
    ) {
        return None;
    }
    // A truncated dispatch must be hydrated through the public retrieve journey.
    if response["truncated"] == true {
        return None;
    }
    if tool == "tracedecay_project_search"
        && !root
            .file_name()?
            .to_str()?
            .to_lowercase()
            .contains(&args["query"].as_str()?.to_lowercase())
    {
        return None;
    }
    Some((|| {
        let payload = response
            .pointer("/outcome/value/payload")
            .or_else(|| response.pointer("/application/outcome/value/payload"))
            .unwrap_or(response);
        let project_id = ctx
            .seeds
            .project_id
            .as_deref()
            .ok_or("admin fixture project identity is absent")?;
        let profile = root
            .parent()
            .ok_or("fixture isolation root is absent")?
            .join("profile");
        let store = profile.join("projects").join(project_id);
        let database = store.join("tracedecay.db");
        let branch = git(root, &["branch", "--show-current"])?;
        match tool {
            "tracedecay_active_project" => {
                if payload["project_id"] != project_id
                    || payload["repository_id"].as_str() != ctx.seeds.repository_id.as_deref()
                    || !same_path(&payload["project_root"], root)
                    || payload["branch"]["current_branch"] != branch
                    || payload["branch"]["serving_branch"] != branch
                    || payload["branch"]["branch_drifted"] != false
                    || !same_path(&payload["storage"]["graph_db_path"], &database)
                    || !database.is_file()
                {
                    return Err("active project did not match the fixture's native branch and durable store".into());
                }
            }
            "tracedecay_project_context" => {
                check_project(&payload["project"], root, project_id)?;
                if !same_path(&payload["registry_path"], &profile.join("global.db"))
                    || !payload["aliases"].as_array().is_some_and(|aliases| {
                        aliases.iter().any(|alias| {
                            alias["project_id"] == project_id
                                && same_path(&alias["alias_path"], root)
                        })
                    })
                {
                    return Err("project context omitted the actual fixture registry/alias".into());
                }
            }
            "tracedecay_project_list" | "tracedecay_project_search" => {
                let projects = payload["projects"]
                    .as_array()
                    .ok_or("registry omitted projects")?;
                let matching: Vec<_> = projects
                    .iter()
                    .filter(|project| project["project_id"] == project_id)
                    .collect();
                if matching.len() != 1
                    || !same_path(&payload["registry_path"], &profile.join("global.db"))
                {
                    return Err("registry listing omitted or duplicated the fixture project".into());
                }
                check_project(matching[0], root, project_id)?;
                if !payload["project_tree"].as_array().is_some_and(|groups| {
                    groups.iter().any(|group| {
                        group["projects"].as_array().is_some_and(|entries| {
                            entries.iter().any(|entry| {
                                entry["project_id"] == project_id
                                    && same_path(&entry["canonical_root"], root)
                            })
                        })
                    })
                }) {
                    return Err("registry tree did not retain the exact fixture project".into());
                }
            }
            "tracedecay_config" => {
                let path = args["path"]
                    .as_str()
                    .ok_or("config fixture path is absent")?;
                let key = args["key"].as_str().ok_or("config fixture key is absent")?;
                let bytes =
                    std::fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
                let document: Value = if path.ends_with(".json") {
                    serde_json::from_str(&bytes).map_err(|error| error.to_string())?
                } else {
                    toml::from_str(&bytes).map_err(|error| error.to_string())?
                };
                let expected = key
                    .split('.')
                    .try_fold(&document, |value, part| value.get(part))
                    .ok_or("literal fixture config key is absent")?;
                let matches = payload["matches"]
                    .as_array()
                    .ok_or("config result omitted matches")?;
                if matches.len() != 1
                    || matches[0]["file"] != path
                    || matches[0]["key"] != key
                    || matches[0]["value"] != *expected
                {
                    return Err("config lookup changed the literal fixture file/key/value".into());
                }
            }
            "tracedecay_runtime" => {
                if payload["process"]["pid"] != json!(std::process::id())
                    || payload["host_os"] != std::env::consts::OS
                    || !same_path(&payload["database"]["project_root"], root)
                    || !same_path(&payload["database"]["canonical_db_path"], &database)
                    || !database.is_file()
                    || payload["database"]["dirty_marker"]["exists"]
                        != json!(database.with_extension("db.dirty").exists())
                    || !payload["database"]["runtime_registry"]["shards"]
                        .as_array()
                        .is_some_and(|shards| {
                            shards.iter().any(|shard| {
                                shard.pointer("/binding/shard_id/scope/project_id")
                                    == Some(&json!(project_id))
                                    && shard["state"] == "ready"
                            })
                        })
                {
                    return Err("runtime telemetry did not identify the actual fixture process and mounted project shard".into());
                }
            }
            "tracedecay_status" => {
                if !same_path(&payload["project_root"], root)
                    || payload["active_branch"] != branch
                    || payload["serving_branch"] != branch
                    || payload["retrieval_serving"]["status"] != "serving"
                    || payload["graph_statistics"]["state"] != "observed"
                    || !payload["memory"]["owners"]
                        .as_array()
                        .is_some_and(|owners| {
                            owners.iter().any(|owner| {
                                owner["project_id"] == project_id
                                    && owner["kind"] == "graph_catalog"
                                    && owner["holders"].as_array().is_some_and(|holders| {
                                        holders.iter().any(|holder| {
                                            holder["holding"]
                                                == payload["graph_statistics"]["generation_id"]
                                        })
                                    })
                            })
                        })
                {
                    return Err("status did not bind the live fixture branch and retained serving generation".into());
                }
            }
            "tracedecay_storage_status" => {
                let pages = payload["page_count"]
                    .as_u64()
                    .ok_or("storage page count absent")?;
                let size = payload["page_size_bytes"]
                    .as_u64()
                    .ok_or("storage page size absent")?;
                if payload["project_id"] != project_id
                    || !same_path(&payload["store_path"], &database)
                    || !database.is_file()
                    || payload["read_only"] != false
                    || payload["status"] != "ok"
                    || pages.checked_mul(size) != payload["database_bytes"].as_u64()
                    || !payload["history"].as_array().is_some_and(|history| {
                        history
                            .iter()
                            .any(|sample| sample["database_bytes"] == payload["database_bytes"])
                    })
                {
                    return Err("storage status did not describe the actual writable fixture store and its observed page census".into());
                }
            }
            "tracedecay_health_read" => {
                if payload["status"] != "ok"
                    || response["scope"]["project_id"] != project_id
                    || response["scope"]["repository_id"].as_str()
                        != ctx.seeds.repository_id.as_deref()
                    || response["scope"]["reference"] != format!("refs/heads/{branch}")
                    || !database.is_file()
                    || std::fs::metadata(&database)
                        .map_err(|error| error.to_string())?
                        .permissions()
                        .readonly()
                {
                    return Err(
                        "health read did not identify the existing writable fixture database"
                            .into(),
                    );
                }
            }
            "tracedecay_skill_list" => {
                let id = ctx
                    .seeds
                    .skill_id
                    .as_deref()
                    .ok_or("managed skill fixture is absent")?;
                let record = read_json(
                    &managed_skill_dir(&profile, id)
                        .map_err(|error| error.to_string())?
                        .join("skill.json"),
                )?;
                let skill = payload["skills"]
                    .as_array()
                    .and_then(|skills| skills.iter().find(|skill| skill["metadata"]["id"] == id))
                    .ok_or("skill list omitted the durable fixture skill")?;
                let paths: Vec<_> = record["support_files"]
                    .as_array()
                    .ok_or("durable skill omitted support files")?
                    .iter()
                    .map(|file| file["path"].clone())
                    .collect();
                if skill["metadata"] != record["metadata"]
                    || skill["support_file_paths"] != json!(paths)
                    || record["body_markdown"] != "Benchmark managed skill body marker."
                    || !same_path(&payload["profile_root"], &profile)
                {
                    return Err(
                        "skill inventory changed the durable fixture metadata or support files"
                            .into(),
                    );
                }
            }
            "tracedecay_automation_run_list" => {
                let id = ctx
                    .seeds
                    .automation_run_id
                    .as_deref()
                    .ok_or("automation ledger fixture is absent")?;
                let ledger = std::fs::read_to_string(run_ledger_path(&store.join("dashboard")))
                    .map_err(|error| error.to_string())?;
                let records = ledger
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(serde_json::from_str::<AutomationRunLedgerRecord>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?;
                let record = records
                    .iter()
                    .rev()
                    .find(|record| record.run_id == id)
                    .ok_or("durable automation fixture record is absent")?;
                let expected = serde_json::to_value(record).map_err(|error| error.to_string())?;
                let actual = payload["runs"]
                    .as_array()
                    .and_then(|runs| runs.iter().find(|run| run["run_id"] == id))
                    .ok_or("run listing omitted the durable automation fixture")?;
                for field in [
                    "run_id",
                    "status",
                    "backend",
                    "model",
                    "started_at",
                    "completed_at",
                    "accepted_count",
                    "rejected_count",
                    "reviewed_count",
                    "skipped_count",
                ] {
                    if actual[field] != expected[field] {
                        return Err(format!(
                            "automation listing changed durable fixture field {field}"
                        ));
                    }
                }
                let kinds: Vec<_> = expected["artifacts"]
                    .as_array()
                    .ok_or("durable automation artifacts absent")?
                    .iter()
                    .map(|artifact| artifact["kind"].clone())
                    .collect();
                if actual["artifact_kinds"] != json!(kinds) {
                    return Err("automation listing changed durable artifact kinds".into());
                }
            }
            "tracedecay_hermes_skill_bridge" => {
                let home = root.parent().ok_or("fixture isolation root is absent")?;
                let skills = home.join(".hermes/skills");
                let skill = skills.join("fixture-inventory");
                let bytes = std::fs::read_to_string(skill.join("SKILL.md"))
                    .map_err(|error| error.to_string())?;
                let expected = "---\nname: fixture-inventory\ndescription: Inspect the isolated runtime fixture.\n---\nRead the fixture catalog before editing it.\n";
                let bridge = &payload["bridge"];
                let entries = bridge["skills"]
                    .as_array()
                    .ok_or("Hermes bridge omitted skills")?;
                if bytes != expected
                    || entries.len() != 1
                    || bridge["skill_count"] != 1
                    || !same_path(&bridge["skills_dir"], &skills)
                    || !same_path(&entries[0]["path"], &skill)
                    || entries[0]["name"] != "fixture-inventory"
                    || entries[0]["description"] != "Inspect the isolated runtime fixture."
                    || args["include_skill_bodies"] != true
                    || entries[0]["body_markdown"] != bytes
                    || bridge["pending_skill_count"] != 0
                    || bridge["usage_record_count"] != 0
                    || bridge["pending_skills"] != json!([])
                    || bridge["usage_records"] != json!({})
                {
                    return Err(
                        "Hermes inventory did not match the literal isolated native skill file"
                            .into(),
                    );
                }
            }
            _ => return Err("unsupported admin fixture operation".into()),
        }
        Ok(())
    })())
}
