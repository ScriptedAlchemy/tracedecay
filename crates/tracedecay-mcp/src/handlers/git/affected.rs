//! `tracedecay_affected`, breadth-first reverse-dependency traversal from changed files to the tests that cover them.

use super::*;
use tracedecay_application::primitives::{
    AffectedTestTraversal, affected_test_proximity, rank_affected_tests,
};

type FileDependentsByFile = HashMap<String, Vec<String>>;
type AffectedDependentsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FileDependentsByFile>> + Send + 'a>>;

pub(crate) trait AffectedTestDependents: Sync {
    fn get_file_dependents_batch<'a>(&'a self, files: &'a [String])
    -> AffectedDependentsFuture<'a>;
}

struct VerifiedAffectedTestDependents<'a> {
    query: &'a tracedecay_graph_query::VerifiedGraphQuery,
}

pub(super) async fn collect_verified_affected_test_files(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
) -> Result<AffectedTestTraversal> {
    collect_affected_test_files_with(
        &VerifiedAffectedTestDependents { query: graph },
        files,
        max_depth,
        custom_glob,
        |paths| graph.test_annotated_logical_files(Some(paths), 500_000, 2_000_000),
    )
    .await
}

impl AffectedTestDependents for VerifiedAffectedTestDependents<'_> {
    fn get_file_dependents_batch<'a>(
        &'a self,
        files: &'a [String],
    ) -> AffectedDependentsFuture<'a> {
        Box::pin(async move {
            let mut dependents = FileDependentsByFile::new();
            for file in files {
                dependents.insert(file.clone(), self.query.get_file_dependents(file).await?);
            }
            Ok(dependents)
        })
    }
}

#[cfg(test)]
pub(crate) async fn collect_affected_test_files<D: AffectedTestDependents + ?Sized>(
    dependents_source: &D,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> Result<AffectedTestTraversal> {
    collect_affected_test_files_with(dependents_source, files, max_depth, custom_glob, |paths| {
        Ok(paths
            .intersection(files_with_inline_tests)
            .cloned()
            .collect())
    })
    .await
}

async fn collect_affected_test_files_with<D, F>(
    dependents_source: &D,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    inline_tests_in: F,
) -> Result<AffectedTestTraversal>
where
    D: AffectedTestDependents + ?Sized,
    F: Fn(&HashSet<String>) -> Result<HashSet<String>>,
{
    let mut test_distances: HashMap<String, usize> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier = Vec::new();
    let initial_inline_tests = inline_tests_in(&files.iter().cloned().collect())?;

    for file in files {
        if matches_test_file(file, custom_glob, &initial_inline_tests) {
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
        let inline_tests = inline_tests_in(&dependents.iter().cloned().collect())?;

        let mut next_frontier = Vec::new();
        for dep in dependents {
            if !visited.insert(dep.clone()) {
                continue;
            }
            if matches_test_file(&dep, custom_glob, &inline_tests) {
                test_distances.insert(dep, depth + 1);
            } else {
                next_frontier.push(dep);
            }
        }
        frontier = next_frontier;
    }

    Ok(AffectedTestTraversal { test_distances })
}

#[hotpath::measure(future = true, label = "mcp.git.affected.total")]
pub async fn handle_affected(
    ctx: &McpToolContext<'_>,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    ctx.verify_graph_scope(graph)?;
    let files = require_string_array_arg(&args, "files")?;
    let max_depth = clamped_depth_arg(&args, "depth", 5, 10);

    let custom_filter = args.get("filter").and_then(|v| v.as_str());
    let custom_glob = custom_filter.and_then(|p| glob::Pattern::new(p).ok());

    let traversal = hotpath::future!(
        collect_verified_affected_test_files(graph, &files, max_depth, custom_glob.as_ref()),
        label = "mcp.git.affected.traverse"
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
    let output = hotpath::measure_block!(
        "mcp.git.affected.assemble",
        json!({
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
        })
    );

    Ok(generic_tool_result(
        Some(ctx.project_root()),
        &args,
        &output,
        touched_files,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::primitives::RankedAffectedTest;

    struct FakeAffectedTestDependents {
        dependents: HashMap<String, Vec<String>>,
    }

    impl AffectedTestDependents for FakeAffectedTestDependents {
        fn get_file_dependents_batch<'a>(
            &'a self,
            files: &'a [String],
        ) -> AffectedDependentsFuture<'a> {
            Box::pin(async move {
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
        }
    }

    #[tokio::test]
    async fn affected_traversal_records_minimum_hop_to_each_test() {
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
            traversal.test_distances,
            HashMap::from([
                ("tests/direct_test.rs".to_string(), 1),
                ("tests/near_test.rs".to_string(), 2),
                ("tests/transitive_test.rs".to_string(), 3),
            ])
        );
    }

    #[tokio::test]
    async fn affected_traversal_ranks_changed_tests_first_regardless_of_neighbor_order() {
        let mut ranked_runs = Vec::new();

        for reverse in [false, true] {
            let source = fake_affected_test_dependents(reverse);
            let files = [
                "tests/changed_test.rs".to_string(),
                "src/root.rs".to_string(),
            ];
            let traversal = collect_affected_test_files(&source, &files, 5, None, &HashSet::new())
                .await
                .unwrap();
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
