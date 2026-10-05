//! Git family: status/diff/hunks/blame/history/preview/apply plus the branch
//! and git-adjacent reads.

use serde_json::{Value, json};

use crate::queries::{EffectCleanup, PrimeStep, QueryContext, ToolGroup, five};

use super::{eq, eqc, rq};

pub(crate) fn verify_git_fixture(
    tool: &str,
    root: &std::path::Path,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    if !crate::repos::small_fixture_enabled() {
        return None;
    }
    let payload = response
        .pointer("/outcome/value/payload")
        .unwrap_or(response);
    let read_git = |arguments: &[&str]| -> Result<String, String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "Git verification failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        String::from_utf8(output.stdout)
            .map(|text| text.trim().to_owned())
            .map_err(|error| error.to_string())
    };
    let strings = |value: &Value, field: &str| -> Result<Vec<String>, String> {
        value
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("missing {field}"))?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("non-string {field} entry"))
            })
            .collect()
    };
    let git_lines = |arguments: &[&str]| -> Result<Vec<String>, String> {
        Ok(read_git(arguments)?.lines().map(str::to_owned).collect())
    };
    let result = payload.pointer("/result/value").unwrap_or(payload);
    let require = |condition: bool, detail: &str| -> Result<(), String> {
        condition.then_some(()).ok_or_else(|| detail.to_owned())
    };
    let check = || -> Result<(), String> {
        match tool {
            "tracedecay_git_status" => {
                let head = read_git(&["rev-parse", "HEAD"])?;
                let branch = read_git(&["branch", "--show-current"])?;
                let staged = git_lines(&["diff", "--cached", "--name-only"])?;
                let unstaged = git_lines(&["diff", "--name-only"])?;
                let untracked = git_lines(&["ls-files", "--others", "--exclude-standard"])?;
                let changed = strings(result, "changed_paths")?;
                require(
                    result.pointer("/head/commit") == Some(&json!(head))
                        && result.pointer("/head/branch") == Some(&json!(branch))
                        && result.pointer("/head/state") == Some(&json!("attached"))
                        && result.get("staged") == Some(&json!(staged.len()))
                        && result.get("unstaged") == Some(&json!(unstaged.len()))
                        && result.get("untracked") == Some(&json!(untracked.len()))
                        && result.get("conflicted") == Some(&json!(0))
                        && staged
                            .iter()
                            .chain(&unstaged)
                            .chain(&untracked)
                            .all(|path| changed.contains(path)),
                    "Git status lost a live changed path, count, or HEAD identity",
                )?;
            }
            "tracedecay_git_diff" => {
                let mut command = vec!["diff", "--numstat"];
                let scope = args.get("scope").and_then(Value::as_str);
                if scope == Some("commit_range") {
                    command.push(
                        args.get("base")
                            .and_then(Value::as_str)
                            .ok_or("missing diff base")?,
                    );
                    command.push(
                        args.get("head")
                            .and_then(Value::as_str)
                            .ok_or("missing diff head")?,
                    );
                }
                let expected = git_lines(&command)?;
                let files = result
                    .get("files")
                    .and_then(Value::as_array)
                    .ok_or("missing diff files")?;
                require(
                    files.len() == expected.len(),
                    "Git diff changed file count differs",
                )?;
                for line in expected {
                    let parts: Vec<_> = line.split('\t').collect();
                    let [insertions, deletions, path] = parts.as_slice() else {
                        return Err("invalid native Git numstat".to_owned());
                    };
                    let file = files
                        .iter()
                        .find(|file| file.get("path") == Some(&json!(path)))
                        .ok_or_else(|| format!("Git diff omitted {path}"))?;
                    require(
                        file.get("insertions")
                            == Some(&json!(
                                insertions
                                    .parse::<u64>()
                                    .map_err(|error| error.to_string())?
                            ))
                            && file.get("deletions")
                                == Some(&json!(
                                    deletions
                                        .parse::<u64>()
                                        .map_err(|error| error.to_string())?
                                ))
                            && file.get("binary") == Some(&json!(false)),
                        "Git diff does not match actual added/deleted fixture lines",
                    )?;
                    let base = if scope == Some("commit_range") {
                        args["base"].as_str().ok_or("invalid diff base")?
                    } else {
                        ""
                    };
                    let blob = read_git(&["rev-parse", &format!("{base}:{path}")])?;
                    require(
                        file.get("old_blob") == Some(&json!(blob)),
                        "Git diff reports a different base blob",
                    )?;
                    if scope == Some("commit_range") {
                        let head = args["head"].as_str().ok_or("invalid diff head")?;
                        let blob = read_git(&["rev-parse", &format!("{head}:{path}")])?;
                        require(
                            file.get("new_blob") == Some(&json!(blob)),
                            "Git diff reports a different committed head blob",
                        )?;
                    }
                }
            }
            "tracedecay_git_hunks" => {
                let staged = args.get("scope") == Some(&json!("staged"));
                let mut command = vec!["diff", "--name-only"];
                if staged {
                    command.push("--cached");
                }
                let expected = git_lines(&command)?;
                let hunks = result
                    .get("hunks")
                    .and_then(Value::as_array)
                    .ok_or("missing hunk entries")?;
                for path in &expected {
                    require(
                        hunks
                            .iter()
                            .any(|entry| entry.pointer("/hunk/path") == Some(&json!(path))),
                        "Git hunks omitted a live changed fixture file",
                    )?;
                }
                for entry in hunks {
                    let hunk = entry.get("hunk").ok_or("missing hunk evidence")?;
                    let path = hunk
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or("missing hunk path")?;
                    let blob = read_git(&["rev-parse", &format!(":{path}")])?;
                    require(
                        expected.iter().any(|expected| expected == path)
                            && hunk.pointer("/expected_index_entry/blob/present")
                                == Some(&json!(blob))
                            && hunk.get("preview_id") == result.get("preview_input_id")
                            && hunk.get("snapshot_digest")
                                == result.get("repository_snapshot_digest"),
                        "Git hunk identity is unrelated to the live index or preview",
                    )?;
                    if !staged {
                        let blob = read_git(&["hash-object", "--", path])?;
                        require(
                            hunk.pointer("/expected_worktree_blob/present") == Some(&json!(blob))
                                && hunk.get("direction") == Some(&json!("working_tree_to_index")),
                            "Git hunk does not bind the actual dirty fixture bytes",
                        )?;
                    }
                }
                require(
                    !expected.is_empty() || hunks.is_empty(),
                    "Git hunks fabricated a staged change",
                )?;
            }
            "tracedecay_git_blame" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("missing blame path")?;
                let native = read_git(&["blame", "--line-porcelain", "--", path])?;
                let expected: Vec<_> = native
                    .lines()
                    .filter_map(|line| {
                        let parts: Vec<_> = line.split_whitespace().collect();
                        (parts.len() >= 3
                            && parts[0].len() == 40
                            && parts[0].bytes().all(|byte| byte.is_ascii_hexdigit()))
                        .then(|| (parts[0], parts[1], parts[2]))
                    })
                    .collect();
                let lines = result
                    .get("lines")
                    .and_then(Value::as_array)
                    .ok_or("missing blame lines")?;
                require(
                    result.get("path") == Some(&json!(path))
                        && !expected.is_empty()
                        && lines.len() == expected.len(),
                    "Git blame lost fixture lines",
                )?;
                for (line, (commit, origin, final_line)) in lines.iter().zip(expected) {
                    require(
                        line.get("commit") == Some(&json!(commit))
                            && line.get("origin_line")
                                == Some(&json!(
                                    origin.parse::<u64>().map_err(|error| error.to_string())?
                                ))
                            && line.get("final_line")
                                == Some(&json!(
                                    final_line
                                        .parse::<u64>()
                                        .map_err(|error| error.to_string())?
                                )),
                        "Git blame attributed a fixture line to the wrong commit or position",
                    )?;
                }
            }
            "tracedecay_git_history" => {
                let path = args
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("missing history path")?;
                let expected = git_lines(&[
                    "log",
                    "-64",
                    "--format=%H%x09%T%x09%s%x09%P%x09end",
                    "--",
                    path,
                ])?;
                let commits = result
                    .get("commits")
                    .and_then(Value::as_array)
                    .ok_or("missing history commits")?;
                require(
                    !expected.is_empty() && commits.len() == expected.len(),
                    "Git history omitted the fixture's path history",
                )?;
                for (commit, native) in commits.iter().zip(expected) {
                    let parts: Vec<_> = native.split('\t').collect();
                    let [hash, tree, subject, parents, "end"] = parts.as_slice() else {
                        return Err("invalid native Git history record".to_owned());
                    };
                    require(
                        commit.get("commit") == Some(&json!(hash))
                            && commit.get("tree") == Some(&json!(tree))
                            && commit.get("subject") == Some(&json!(subject))
                            && commit.get("parents")
                                == Some(&json!(parents.split_whitespace().collect::<Vec<_>>())),
                        "Git history changed the fixture's commit, tree, subject, or parents",
                    )?;
                }
            }
            "tracedecay_branch_search" => {
                let branch = args
                    .get("branch")
                    .and_then(Value::as_str)
                    .ok_or("missing branch")?;
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or("missing search query")?;
                let path = match query {
                    "fixture_catalog" | "total_quantity" => "src/catalog.py",
                    "render_summary" => "src/main.py",
                    "fixtureGraph" | "dependencyCount" => "src/graph.ts",
                    _ => return Err("branch search lacks a literal fixture target".to_owned()),
                };
                let revision = read_git(&["rev-parse", &format!("refs/heads/{branch}")])?;
                let tree = read_git(&["rev-parse", &format!("refs/heads/{branch}^{{tree}}")])?;
                let source = read_git(&["show", &format!("refs/heads/{branch}:{path}")])?;
                require(
                    source.contains(query)
                        && payload.get("status") == Some(&json!("complete"))
                        && payload.get("source_revision") == Some(&json!(revision))
                        && payload.get("source_tree") == Some(&json!(tree))
                        && payload
                            .get("results")
                            .and_then(Value::as_array)
                            .is_some_and(|hits| {
                                hits.iter().any(|hit| {
                                    hit.get("name") == Some(&json!(query))
                                        && hit.get("path") == Some(&json!(path))
                                        && hit.get("source_revision") == Some(&json!(revision))
                                        && hit.get("source_tree") == Some(&json!(tree))
                                })
                            }),
                    "branch search omitted the requested symbol from the exact branch source",
                )?;
            }
            "tracedecay_branch_diff" => {
                for (argument, field) in [("base", "base_revision"), ("head", "head_revision")] {
                    let reference = args
                        .get(argument)
                        .and_then(Value::as_str)
                        .ok_or("missing branch ref")?;
                    require(
                        payload.get(field) == Some(&json!(read_git(&["rev-parse", reference])?)),
                        "branch diff used a different branch revision",
                    )?;
                }
                require(
                    payload.get("status") == Some(&json!("complete"))
                        && payload.get("total_changes") == Some(&json!(1))
                        && payload.pointer("/summary/changed") == Some(&json!(1))
                        && payload
                            .get("changes")
                            .and_then(Value::as_array)
                            .is_some_and(|changes| {
                                changes.len() == 1
                                    && changes[0].get("change") == Some(&json!("changed"))
                                    && changes[0].pointer("/base/file") == Some(&json!("README.md"))
                                    && changes[0].pointer("/head/file") == Some(&json!("README.md"))
                                    && changes[0].pointer("/head/name")
                                        == Some(&json!("README.md > Runtime Fixture"))
                                    && changes[0].pointer("/base/content_digest")
                                        != changes[0].pointer("/head/content_digest")
                            }),
                    "branch diff missed the fixture's changed README module",
                )?;
            }
            "tracedecay_commit_context" => {
                let staged = args.get("staged_only") == Some(&json!(true));
                let mut command = vec!["diff", "--name-only"];
                if staged {
                    command.push("--cached");
                }
                let expected = git_lines(&command)?;
                let files = payload
                    .get("changed_files")
                    .and_then(Value::as_array)
                    .ok_or("missing commit files")?;
                require(
                    files.len() == expected.len()
                        && expected.iter().all(|path| {
                            files
                                .iter()
                                .any(|file| file.get("file") == Some(&json!(path)))
                        })
                        && strings(payload, "recent_commits")?
                            == git_lines(&["log", "-5", "--format=%s"])?,
                    "commit context lost actual changed files or recent commit subjects",
                )?;
                if !staged {
                    require(
                        payload
                            .pointer("/symbols_by_role/config")
                            .and_then(Value::as_array)
                            .is_some_and(|symbols| {
                                symbols.iter().any(|symbol| {
                                    symbol.get("file") == Some(&json!("Cargo.toml"))
                                        && symbol.get("kind") == Some(&json!("config_summary"))
                                        && symbol
                                            .get("config_keys")
                                            .and_then(Value::as_u64)
                                            .is_some_and(|count| count >= 6)
                                })
                            }),
                        "commit context missed the changed fixture configuration",
                    )?;
                }
            }
            "tracedecay_pr_context" | "tracedecay_changelog" => {
                let (base_field, head_field) = if tool == "tracedecay_pr_context" {
                    ("base_ref", "head_ref")
                } else {
                    ("from_ref", "to_ref")
                };
                let base = args
                    .get(base_field)
                    .and_then(Value::as_str)
                    .ok_or("missing base ref")?;
                let head = args
                    .get(head_field)
                    .and_then(Value::as_str)
                    .ok_or("missing head ref")?;
                let expected = git_lines(&["diff", "--name-only", base, head])?;
                require(
                    expected == ["README.md"],
                    "fixture history has an unexpected comparison",
                )?;
                require(
                    payload.get("status") == Some(&json!("complete"))
                        && payload.pointer("/symbol_changes_coverage/status")
                            == Some(&json!("complete")),
                    "Git context did not serve complete exact branch symbol evidence",
                )?;
                if tool == "tracedecay_changelog" {
                    require(
                        strings(payload, "changed_files")? == expected
                            && payload.get("changed_file_count") == Some(&json!(expected.len()))
                            && payload
                                .get("symbols_modified")
                                .and_then(Value::as_array)
                                .is_some_and(|symbols| {
                                    symbols.len() == 1
                                        && symbols[0].get("file") == Some(&json!("README.md"))
                                }),
                        "changelog missed the actual changed README symbol",
                    )?;
                } else {
                    let revision = read_git(&["rev-parse", head])?;
                    require(
                        payload.get("base_oid") == Some(&json!(read_git(&["rev-parse", base])?))
                            && payload.get("head_oid") == Some(&json!(revision))
                            && payload.get("files_changed") == Some(&json!(expected.len()))
                            && payload
                                .get("changes")
                                .and_then(Value::as_array)
                                .is_some_and(|changes| {
                                    changes.len() == 1
                                        && changes[0].get("path") == Some(&json!("README.md"))
                                        && changes[0].get("status") == Some(&json!("modified"))
                                })
                            && payload
                                .get("commits")
                                .and_then(Value::as_array)
                                .is_some_and(|commits| {
                                    commits.len() == 1
                                        && commits[0].get("hash") == Some(&json!(revision))
                                        && commits[0].get("subject")
                                            == Some(&json!("fixture history point"))
                                })
                            && payload.get("symbols_modified") == Some(&json!(1))
                            && payload.get("symbols_added") == Some(&json!(0))
                            && payload.get("symbols_removed") == Some(&json!(0))
                            && payload
                                .get("modified")
                                .and_then(Value::as_array)
                                .is_some_and(|symbols| {
                                    symbols.len() == 1
                                        && symbols[0].get("file") == Some(&json!("README.md"))
                                }),
                        "PR context lost exact fixture revision, commit, file, or symbol changes",
                    )?;
                }
            }
            "tracedecay_diff_context" => {
                require(
                    payload.get("changed_files") == args.get("files"),
                    "diff context changed the requested fixture file",
                )?;
                let path = args
                    .pointer("/files/0")
                    .and_then(Value::as_str)
                    .ok_or("missing diff-context path")?;
                let (name, impacted) = match path {
                    "Cargo.toml" => ("package", None),
                    "README.md" => ("Runtime Fixture", None),
                    "src/catalog.py" => {
                        ("fixture_catalog", Some(("render_summary", "src/main.py")))
                    }
                    "src/graph.ts" => (
                        "fixtureGraph",
                        Some(("buildFixtureReport", "src/report.ts")),
                    ),
                    "src/lib.rs" => (
                        "fixture_catalog_total",
                        Some(("fixture_catalog_has_stable_total", "tests/catalog.rs")),
                    ),
                    _ => return Err("diff context lacks a literal fixture target".to_owned()),
                };
                let source =
                    std::fs::read_to_string(root.join(path)).map_err(|error| error.to_string())?;
                let literal = if path == "Cargo.toml" {
                    "[package]"
                } else {
                    name
                };
                require(
                    source.contains(literal),
                    "diff-context fixture source lacks the expected literal",
                )?;
                require(
                    payload
                        .get("modified_symbols")
                        .and_then(Value::as_array)
                        .is_some_and(|symbols| {
                            symbols.iter().any(|symbol| {
                                symbol.get("name") == Some(&json!(name))
                                    && symbol.get("file") == Some(&json!(path))
                            })
                        }),
                    "diff context omitted the requested literal fixture symbol",
                )?;
                if let Some((name, file)) = impacted {
                    require(
                        payload
                            .get("impacted_symbols")
                            .and_then(Value::as_array)
                            .is_some_and(|symbols| {
                                symbols.iter().any(|symbol| {
                                    symbol.get("name") == Some(&json!(name))
                                        && symbol.get("file") == Some(&json!(file))
                                })
                            }),
                        "diff context missed the fixture's cross-file dependent",
                    )?;
                }
                if path == "src/lib.rs" {
                    require(
                        strings(payload, "affected_tests")?
                            .contains(&"tests/catalog.rs".to_owned())
                            && payload.pointer("/test_gate/verdict") == Some(&json!("pass")),
                        "diff context missed the real fixture test and its passing test gate",
                    )?;
                }
            }
            "tracedecay_git_apply" => {
                let tree = read_git(&["write-tree"])?;
                let head = read_git(&["rev-parse", "HEAD"])?;
                if payload.get("outcome") != Some(&json!("committed"))
                    || payload.get("operation") != Some(&json!("stage_hunks"))
                    || payload.get("preview_id") != args.get("preview_id")
                    || payload.get("new_index_tree") != Some(&json!(tree))
                    || payload.get("new_head") != Some(&json!(head))
                    || payload.get("old_head") != payload.get("new_head")
                    || payload.get("old_index_tree") == payload.get("new_index_tree")
                {
                    return Err(
                        "stage receipt does not match the changed Git index and HEAD".to_owned(),
                    );
                }
            }
            "tracedecay_git_preview" => {
                let tree = read_git(&["write-tree"])?;
                let preview: tracedecay_domain::GitIndexPreviewV1 =
                    serde_json::from_value(payload.clone())
                        .map_err(|error| format!("invalid Git preview: {error}"))?;
                let selected = preview
                    .selected_hunk_digests()
                    .map_err(|error| error.to_string())?;
                if payload.pointer("/disposition/state") != Some(&json!("applicable"))
                    || payload.get("operation") != Some(&json!("stage_hunks"))
                    || Some(&json!(selected)) != args.get("selected_hunk_digests")
                    || payload
                        .get("candidate_index_tree")
                        .and_then(Value::as_str)
                        .is_none_or(|candidate| candidate == tree)
                {
                    return Err(
                        "stage preview did not retain the selected change to the Git index"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_branch_list" => {
                let head = read_git(&["rev-parse", "refs/heads/bench"])?;
                let tree = read_git(&["rev-parse", "refs/heads/bench^{tree}"])?;
                if !payload
                    .get("snapshots")
                    .and_then(Value::as_array)
                    .is_some_and(|snapshots| {
                        snapshots.iter().any(|snapshot| {
                            snapshot.get("branch") == Some(&json!("bench"))
                                && snapshot.get("source_revision") == Some(&json!(head))
                                && snapshot.get("source_tree") == Some(&json!(tree))
                        })
                    })
                {
                    return Err(
                        "branch snapshot does not match Git's bench tip and tree".to_owned()
                    );
                }
            }
            _ => return Err("unsupported verification tool".to_owned()),
        }
        Ok(())
    };
    matches!(
        tool,
        "tracedecay_git_apply"
            | "tracedecay_git_preview"
            | "tracedecay_branch_list"
            | "tracedecay_git_status"
            | "tracedecay_git_diff"
            | "tracedecay_git_hunks"
            | "tracedecay_git_blame"
            | "tracedecay_git_history"
            | "tracedecay_branch_search"
            | "tracedecay_branch_diff"
            | "tracedecay_commit_context"
            | "tracedecay_pr_context"
            | "tracedecay_changelog"
            | "tracedecay_diff_context"
    )
    .then(check)
}

fn path_at(ctx: &QueryContext, i: usize) -> String {
    crate::queries::dir(ctx, i)
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    out.push(ToolGroup {
        tool: "tracedecay_git_status",
        queries: five(|_i| rq("tracedecay_git_status", "status", json!({}))),
    });
    // Diff reads alternate the working-tree scope with a real commit range
    // when ancestry exists (shallow clones keep 64 commits by design).
    let range_base = ctx.seeds.parent_commit.clone();
    let range_head = ctx.seeds.head_commit.clone();
    out.push(ToolGroup {
        tool: "tracedecay_git_diff",
        queries: five(|i| {
            rq(
                "tracedecay_git_diff",
                "diff",
                if i % 2 == 0 && range_base.is_some() && range_head.is_some() {
                    json!({
                        "scope": "commit_range",
                        "base": range_base,
                        "head": range_head,
                        "max_bytes": 65536,
                        "max_entries": 64,
                    })
                } else {
                    json!({
                        "scope": "working_tree",
                        "max_bytes": 65536,
                        "max_entries": 64,
                    })
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_git_hunks",
        queries: five(|i| {
            rq(
                "tracedecay_git_hunks",
                "hunks",
                json!({
                    "scope": *["working_tree", "staged"].get(i % 2).unwrap_or(&""),
                    "max_bytes": 65536,
                    "max_entries": 64,
                }),
            )
        }),
    });
    if !ctx.seeds.unavailable_tools.contains("tracedecay_git_blame") {
        out.push(ToolGroup {
            tool: "tracedecay_git_blame",
            queries: five(|i| {
                rq(
                    "tracedecay_git_blame",
                    "blame",
                    json!({
                        "path": crate::queries::file_at(ctx, i)["path"],
                        "max_bytes": 65536,
                        "max_entries": 64,
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_git_history",
        queries: five(|i| {
            rq(
                "tracedecay_git_history",
                "history",
                json!({
                    "path": path_at(ctx, i),
                    "max_bytes": 65536,
                    "max_entries": 64,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_branch_list",
        queries: five(|i| {
            rq(
                "tracedecay_branch_list",
                "branch_list",
                json!({"limit": 20 + i}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_branch_search",
        queries: five(|i| {
            rq(
                "tracedecay_branch_search",
                "branch_search",
                json!({
                    "branch": ctx.seeds.branch.clone().unwrap_or_else(|| "main".into()),
                    "query": if crate::repos::small_fixture_enabled() {
                        *["fixture_catalog", "total_quantity", "render_summary", "fixtureGraph", "dependencyCount"].get(i).unwrap_or(&"")
                    } else {
                        *["feat", "fix", "main", "dev", "rel"].get(i).unwrap_or(&"")
                    },
                    "limit": if crate::repos::small_fixture_enabled() { 1 } else { 10 },
                }),
            )
        }),
    });
    if let (Some(base), Some(head)) = (ctx.seeds.base_branch.clone(), ctx.seeds.branch.clone()) {
        out.push(ToolGroup {
            tool: "tracedecay_branch_diff",
            queries: five(|_i| {
                rq(
                    "tracedecay_branch_diff",
                    "branch_diff",
                    json!({
                        "base": base,
                        "head": head,
                        "limit": 32,
                    }),
                )
            }),
        });
    }

    // git_preview: mints a stage preview from live hunk digests; re-mint the
    // preview input each iteration since previews expire on consumption.
    fn preview_primes(_ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
        vec![PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_git_hunks",
            args: json!({"scope": "working_tree", "format": "json"}),
            capture: &[
                (
                    "digpath:outcome:value.payload.result.value.preview_input_id",
                    "preview_input_id",
                ),
                ("deep_array:hunk_digests", "hunk_digests"),
            ],
        }]
    }
    /// Applying a stage preview consumes the working-tree hunks into the
    /// index; this cleanup chain unstages them so the next iteration's
    /// `git_hunks` call sees the same dirty file again.
    fn unstage_cleanup(_ctx: &QueryContext, iter: u64) -> Vec<PrimeStep> {
        vec![
            PrimeStep {
                inject: Vec::new(),
                tool: "tracedecay_git_hunks",
                args: json!({"scope": "staged", "format": "json"}),
                capture: &[
                    (
                        "digpath:outcome:value.payload.result.value.preview_input_id",
                        "staged_input_id",
                    ),
                    ("deep_array:hunk_digests", "staged_digests"),
                ],
            },
            PrimeStep {
                inject: Vec::new(),
                tool: "tracedecay_git_preview",
                args: json!({
                    "operation": "unstage_hunks",
                    "preview_input_id": "{{staged_input_id}}",
                    "selected_hunk_digests": "{{staged_digests}}",
                    "format": "json",
                }),
                capture: &[
                    ("dig:preview_id", "unstage_preview_id"),
                    ("dig:preview_digest", "unstage_preview_digest"),
                ],
            },
            PrimeStep {
                inject: Vec::new(),
                tool: "tracedecay_git_apply",
                args: json!({
                    "preview_id": "{{unstage_preview_id}}",
                    "preview_digest": "{{unstage_preview_digest}}",
                    "idempotency_key": format!("bench-git-unstage-{iter}"),
                    "format": "json",
                }),
                capture: &[],
            },
        ]
    }

    // Preview/apply lanes only run when seeding proved the hunk evidence
    // path serves a real preview input for this composition's dirty file.
    let preview_ready = ctx.seeds.preview_input_id.is_some() && !ctx.seeds.hunk_digests.is_empty();
    if preview_ready {
        out.push(ToolGroup {
            tool: "tracedecay_git_preview",
            queries: five(|_i| {
                eq(
                    "tracedecay_git_preview",
                    "preview_stage",
                    json!({
                        "operation": "stage_hunks",
                        "preview_input_id": "{{preview_input_id}}",
                        "selected_hunk_digests": "{{hunk_digests}}",
                    }),
                    preview_primes,
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_git_apply",
            queries: five(|_i| {
                eqc(
                    "tracedecay_git_apply",
                    "apply_stage",
                    json!({
                        "preview_id": "{{preview_id}}",
                        "preview_digest": "{{preview_digest}}",
                        "idempotency_key": "bench-git-apply-{{iter}}",
                    }),
                    |ctx, iter| {
                        let mut steps = preview_primes(ctx, iter);
                        steps.push(PrimeStep {
                            inject: Vec::new(),
                            tool: "tracedecay_git_preview",
                            args: json!({
                                "operation": "stage_hunks",
                                "preview_input_id": "{{preview_input_id}}",
                                "selected_hunk_digests": "{{hunk_digests}}",
                                "format": "json",
                            }),
                            capture: &[
                                ("dig:preview_id", "preview_id"),
                                ("dig:preview_digest", "preview_digest"),
                            ],
                        });
                        steps
                    },
                    EffectCleanup {
                        capture: &[],
                        steps: unstage_cleanup,
                    },
                )
            }),
        });
    }
}
