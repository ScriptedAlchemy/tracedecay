//! `tracedecay_affected` — breadth-first reverse-dependency traversal from changed files to the tests that cover them.

use super::*;

type FileDependentsByFile = HashMap<String, Vec<String>>;
type AffectedDependentsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FileDependentsByFile>> + Send + 'a>>;

pub(crate) trait AffectedTestDependents: Sync {
    fn get_file_dependents_batch<'a>(&'a self, files: &'a [String])
    -> AffectedDependentsFuture<'a>;
}

impl AffectedTestDependents for TraceDecay {
    fn get_file_dependents_batch<'a>(
        &'a self,
        files: &'a [String],
    ) -> AffectedDependentsFuture<'a> {
        Box::pin(async move {
            let mut dependents: FileDependentsByFile = HashMap::new();
            for file in files {
                dependents.insert(file.clone(), self.get_file_dependents(file).await?);
            }
            Ok(dependents)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankedAffectedTest {
    pub(crate) path: String,
    pub(crate) distance: usize,
}

pub(crate) struct AffectedTestTraversal {
    pub(crate) test_distances: HashMap<String, usize>,
}

pub(crate) fn rank_affected_tests(
    test_distances: &HashMap<String, usize>,
) -> Vec<RankedAffectedTest> {
    let mut ranked = test_distances
        .iter()
        .map(|(path, distance)| RankedAffectedTest {
            path: path.clone(),
            distance: *distance,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
}

pub(crate) fn affected_test_proximity(distance: usize) -> &'static str {
    match distance {
        0 => "changed",
        1 => "direct",
        2 => "near",
        _ => "transitive",
    }
}

pub(crate) async fn collect_affected_test_files<D: AffectedTestDependents + ?Sized>(
    dependents_source: &D,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> Result<AffectedTestTraversal> {
    let mut test_distances: HashMap<String, usize> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier = Vec::new();

    for file in files {
        if matches_test_file(file, custom_glob, files_with_inline_tests) {
            test_distances.insert(file.clone(), 0);
        }
        if visited.insert(file.clone()) {
            frontier.push(file.clone());
        }
    }
    frontier.sort();

    for depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let dependents_by_file = dependents_source
            .get_file_dependents_batch(&frontier)
            .await?;
        let mut dependents = frontier
            .iter()
            .filter_map(|file| dependents_by_file.get(file))
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        dependents.sort();
        dependents.dedup();

        let mut next_frontier = Vec::new();
        for dep in dependents {
            if !visited.insert(dep.clone()) {
                continue;
            }
            if matches_test_file(&dep, custom_glob, files_with_inline_tests) {
                test_distances.insert(dep, depth + 1);
            } else {
                next_frontier.push(dep);
            }
        }
        frontier = next_frontier;
    }

    Ok(AffectedTestTraversal { test_distances })
}

/// Handles `tracedecay_affected` tool calls.
pub(crate) async fn handle_affected(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let files = require_string_array_arg(&args, "files")?;
    let max_depth = clamped_depth_arg(&args, "depth", 5, 10);

    let custom_filter = args.get("filter").and_then(|v| v.as_str());
    let custom_glob = custom_filter.and_then(|p| glob::Pattern::new(p).ok());

    let files_with_inline_tests = cg.get_files_with_test_annotations().await?;
    let traversal = collect_affected_test_files(
        cg,
        &files,
        max_depth,
        custom_glob.as_ref(),
        &files_with_inline_tests,
    )
    .await?;

    let mut result = traversal.test_distances.keys().cloned().collect::<Vec<_>>();
    result.sort();
    let ranked = rank_affected_tests(&traversal.test_distances);
    let ranked_tests = ranked
        .iter()
        .enumerate()
        .map(|(index, test)| {
            json!({
                "path": test.path,
                "rank": index + 1,
                "distance": test.distance,
                "proximity": affected_test_proximity(test.distance),
            })
        })
        .collect::<Vec<_>>();
    let recommended_tests = ranked
        .iter()
        .filter(|test| test.distance <= 2)
        .map(|test| test.path.clone())
        .collect::<Vec<_>>();

    let touched_files = unique_file_paths(result.iter().map(std::string::String::as_str));
    let output = json!({
        "changed_files": files,
        "affected_tests": result,
        "count": result.len(),
        "ranked_tests": ranked_tests,
        "recommended_tests": recommended_tests,
        "ranking_metadata": {
            "strategy": "dependency_distance_then_path",
            "distance": "minimum file-dependency hops from the changed files",
            "recommended_proximity": ["changed", "direct", "near"],
            "compatibility_field": "affected_tests",
        },
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAffectedTestDependents {
        dependents: HashMap<String, Vec<String>>,
        frontiers: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl AffectedTestDependents for FakeAffectedTestDependents {
        fn get_file_dependents_batch<'a>(
            &'a self,
            files: &'a [String],
        ) -> AffectedDependentsFuture<'a> {
            Box::pin(async move {
                self.frontiers.lock().unwrap().push(files.to_vec());
                Ok(files
                    .iter()
                    .map(|file| {
                        (
                            file.clone(),
                            self.dependents.get(file).cloned().unwrap_or_default(),
                        )
                    })
                    .collect())
            })
        }
    }

    fn fake_affected_test_dependents(reverse: bool) -> FakeAffectedTestDependents {
        let mut root = vec![
            "tests/direct_test.rs".to_string(),
            "src/b.rs".to_string(),
            "src/a.rs".to_string(),
        ];
        let mut a = vec!["tests/near_test.rs".to_string(), "src/leaf.rs".to_string()];
        let mut b = vec!["src/root.rs".to_string(), "tests/near_test.rs".to_string()];
        if reverse {
            root.reverse();
            a.reverse();
            b.reverse();
        }
        FakeAffectedTestDependents {
            dependents: HashMap::from([
                ("src/root.rs".to_string(), root),
                ("src/a.rs".to_string(), a),
                ("src/b.rs".to_string(), b),
                (
                    "src/leaf.rs".to_string(),
                    vec!["tests/transitive_test.rs".to_string()],
                ),
            ]),
            frontiers: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn serial_affected_test_set(
        source: &FakeAffectedTestDependents,
        files: &[String],
        max_depth: usize,
    ) -> HashSet<String> {
        let mut affected = HashSet::new();
        let mut visited = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        for file in files {
            if crate::tracedecay::is_test_file(file) {
                affected.insert(file.clone());
            }
            if visited.insert(file.clone()) {
                queue.push_back((file.clone(), 0));
            }
        }
        while let Some((file, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for dependent in source.dependents.get(&file).into_iter().flatten() {
                if !visited.insert(dependent.clone()) {
                    continue;
                }
                if crate::tracedecay::is_test_file(dependent) {
                    affected.insert(dependent.clone());
                } else {
                    queue.push_back((dependent.clone(), depth + 1));
                }
            }
        }
        affected
    }

    #[tokio::test]
    async fn affected_traversal_batches_one_database_read_per_frontier() {
        let source = fake_affected_test_dependents(false);
        let traversal = collect_affected_test_files(
            &source,
            &["src/root.rs".to_string()],
            5,
            None,
            &HashSet::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            *source.frontiers.lock().unwrap(),
            vec![
                vec!["src/root.rs".to_string()],
                vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
                vec!["src/leaf.rs".to_string()],
            ]
        );
        assert_eq!(source.frontiers.lock().unwrap().len(), 3);
        assert_eq!(traversal.test_distances.len(), 3);
    }

    #[tokio::test]
    async fn affected_traversal_preserves_set_parity_and_ranks_deterministically() {
        let expected_set = HashSet::from([
            "tests/changed_test.rs".to_string(),
            "tests/direct_test.rs".to_string(),
            "tests/near_test.rs".to_string(),
            "tests/transitive_test.rs".to_string(),
        ]);
        let mut ranked_runs = Vec::new();

        for reverse in [false, true] {
            let source = fake_affected_test_dependents(reverse);
            let files = [
                "tests/changed_test.rs".to_string(),
                "src/root.rs".to_string(),
            ];
            let serial_set = serial_affected_test_set(&source, &files, 5);
            let traversal = collect_affected_test_files(&source, &files, 5, None, &HashSet::new())
                .await
                .unwrap();
            let batched_set = traversal
                .test_distances
                .keys()
                .cloned()
                .collect::<HashSet<_>>();
            assert_eq!(serial_set, expected_set);
            assert_eq!(batched_set, serial_set);
            ranked_runs.push(rank_affected_tests(&traversal.test_distances));
        }

        assert_eq!(ranked_runs[0], ranked_runs[1]);
        assert_eq!(
            ranked_runs[0],
            vec![
                RankedAffectedTest {
                    path: "tests/changed_test.rs".to_string(),
                    distance: 0,
                },
                RankedAffectedTest {
                    path: "tests/direct_test.rs".to_string(),
                    distance: 1,
                },
                RankedAffectedTest {
                    path: "tests/near_test.rs".to_string(),
                    distance: 2,
                },
                RankedAffectedTest {
                    path: "tests/transitive_test.rs".to_string(),
                    distance: 3,
                },
            ]
        );
    }
}
