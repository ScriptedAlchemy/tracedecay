use std::collections::{HashMap, HashSet};

use tracedecay_graph_query::GraphQueryResult as Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedAffectedTest {
    pub path: String,
    pub distance: usize,
}

pub struct AffectedTestTraversal {
    pub test_distances: HashMap<String, usize>,
}

pub fn rank_affected_tests(test_distances: &HashMap<String, usize>) -> Vec<RankedAffectedTest> {
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

pub const fn affected_test_proximity(distance: usize) -> &'static str {
    match distance {
        0 => "changed",
        1 => "direct",
        2 => "near",
        _ => "transitive",
    }
}

#[tracing::instrument(
    name = "usecases.primitives.affected_tests_traverse",
    level = "trace",
    skip_all
)]
pub(crate) async fn collect_affected_test_files(
    graph: &tracedecay_graph_query::queries::GraphQueryManager<'_>,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> Result<AffectedTestTraversal> {
    let is_test = |path: &str| {
        custom_glob.map_or_else(
            || tracedecay_code_index::is_test_file(path) || files_with_inline_tests.contains(path),
            |pattern| pattern.matches(path),
        )
    };
    let mut test_distances = HashMap::new();
    let mut visited = HashSet::new();
    let mut frontier = Vec::new();
    for file in files {
        if is_test(file) {
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
        let mut dependents = Vec::new();
        for file in &frontier {
            dependents.extend(graph.get_file_dependents(file).await?.files);
        }
        dependents.sort();
        dependents.dedup();
        let mut next = Vec::new();
        for dependent in dependents {
            if !visited.insert(dependent.clone()) {
                continue;
            }
            if is_test(&dependent) {
                test_distances.insert(dependent, depth + 1);
            } else {
                next.push(dependent);
            }
        }
        frontier = next;
    }
    Ok(AffectedTestTraversal { test_distances })
}
