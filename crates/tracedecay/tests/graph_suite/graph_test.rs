use std::collections::{HashMap, HashSet};
use std::fs;

use tempfile::TempDir;
use tracedecay::graph::git::file_churn;
use tracedecay::graph::health::{
    HealthDimensions, acyclicity_score, compute_composite_health, dependency_depth,
    gini_coefficient, gini_label, modularity_score,
};

// These tests cover the store-independent health algorithms. Code-graph read
// behavior is covered through the verified Grafeo reader and production MCP
// composition instead of constructing a second relational graph authority.

#[test]
fn test_gini_perfect_equality() {
    let g = gini_coefficient(&[5.0, 5.0, 5.0, 5.0]);
    assert!(
        g.abs() < 1e-9,
        "all-equal values should give Gini ~0.0, got {g}"
    );
}

#[test]
fn test_gini_perfect_inequality() {
    let g = gini_coefficient(&[0.0, 0.0, 0.0, 1000.0]);
    assert!(
        g > 0.7,
        "extreme inequality should give Gini > 0.7, got {g}"
    );
}

#[test]
fn test_gini_moderate() {
    let g = gini_coefficient(&[1.0, 2.0, 3.0, 4.0, 5.0]);
    assert!(
        (0.1..0.5).contains(&g),
        "moderate distribution should give Gini between 0.1 and 0.5, got {g}"
    );
}

#[test]
fn test_gini_empty() {
    assert_eq!(gini_coefficient(&[]), 0.0);
}

#[test]
fn test_gini_single() {
    assert_eq!(gini_coefficient(&[42.0]), 0.0);
}

#[test]
fn test_gini_label_thresholds() {
    assert_eq!(gini_label(0.10), "low inequality (healthy)");
    assert_eq!(gini_label(0.30), "moderate inequality");
    assert_eq!(gini_label(0.50), "high inequality");
    assert_eq!(gini_label(0.70), "extreme inequality (god files likely)");
}

fn make_adj(edges: &[(&str, &str)]) -> HashMap<String, HashSet<String>> {
    let mut adjacency = HashMap::new();
    for &(source, target) in edges {
        adjacency
            .entry(source.to_owned())
            .or_insert_with(HashSet::new)
            .insert(target.to_owned());
        adjacency
            .entry(target.to_owned())
            .or_insert_with(HashSet::new);
    }
    adjacency
}

#[test]
fn test_acyclicity_no_cycles() {
    let (score, cycles) = acyclicity_score(&make_adj(&[("a", "b"), ("b", "c")]));
    assert_eq!(score, 1.0);
    assert_eq!(cycles, 0);
}

#[test]
fn test_acyclicity_with_cycle() {
    let (score, cycles) = acyclicity_score(&make_adj(&[("a", "b"), ("b", "a")]));
    assert!(
        score < 1.0,
        "cyclic graph should score below 1.0, got {score}"
    );
    assert!(cycles > 0, "cyclic graph should report cycle edges");
}

#[test]
fn test_acyclicity_empty() {
    let adjacency = HashMap::<String, HashSet<String>>::new();
    assert_eq!(acyclicity_score(&adjacency), (1.0, 0));
}

#[test]
fn test_depth_linear_chain() {
    let result = dependency_depth(&make_adj(&[("a", "b"), ("b", "c"), ("c", "d")]), 10);
    assert_eq!(result.max_depth, 3);
    let deepest = result
        .chains
        .iter()
        .find(|chain| chain.depth == 3)
        .expect("linear graph should expose its deepest chain");
    assert_eq!(deepest.chain.len(), 4);
}

#[test]
fn test_depth_empty() {
    let adjacency = HashMap::<String, HashSet<String>>::new();
    assert_eq!(dependency_depth(&adjacency, 10).max_depth, 0);
}

#[test]
fn test_depth_with_cycle_breaks() {
    let result = dependency_depth(&make_adj(&[("a", "b"), ("b", "a"), ("b", "c")]), 10);
    assert!(result.max_depth >= 1);
}

#[test]
fn test_modularity_independent_clusters() {
    let adjacency = make_adj(&[("a", "b"), ("c", "d")]);
    let (score, components) = modularity_score(&adjacency);
    assert!(components >= 2);
    assert!(score > 0.0);
}

#[test]
fn test_modularity_single_blob() {
    let adjacency = make_adj(&[("a", "b"), ("b", "c"), ("c", "a")]);
    let (score, components) = modularity_score(&adjacency);
    assert_eq!(components, 1);
    assert!(score < 0.5);
}

#[test]
fn test_modularity_empty() {
    let adjacency = HashMap::<String, HashSet<String>>::new();
    assert_eq!(modularity_score(&adjacency).0, 1.0);
}

#[test]
fn test_composite_health_all_perfect() {
    let dimensions = HealthDimensions {
        acyclicity: 1.0,
        depth: 1.0,
        equality: 1.0,
        redundancy: 1.0,
        modularity: 1.0,
        coverage_discipline: 1.0,
    };
    assert_eq!(compute_composite_health(&dimensions), 10_000);
}

#[test]
fn test_composite_health_one_zero() {
    let dimensions = HealthDimensions {
        acyclicity: 0.0,
        depth: 1.0,
        equality: 1.0,
        redundancy: 1.0,
        modularity: 1.0,
        coverage_discipline: 1.0,
    };
    assert_eq!(compute_composite_health(&dimensions), 0);
}

#[test]
fn test_composite_health_mixed() {
    let dimensions = HealthDimensions {
        acyclicity: 0.8,
        depth: 0.7,
        equality: 0.9,
        redundancy: 0.6,
        modularity: 0.5,
        coverage_discipline: 1.0,
    };
    let score = compute_composite_health(&dimensions);
    assert!((1..10_000).contains(&score));
}

#[tokio::test]
async fn test_file_churn() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let project = dir.path();

    for args in [
        &["init"][..],
        &["config", "user.email", "test@test.com"][..],
        &["config", "user.name", "Test"][..],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("git setup command failed");
        assert!(output.status.success(), "git {args:?} failed");
    }

    fs::write(project.join("file.rs"), "fn foo() {}").expect("write failed");
    for args in [&["add", "."][..], &["commit", "-m", "first"][..]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("first commit command failed");
        assert!(output.status.success(), "git {args:?} failed");
    }

    fs::write(project.join("file.rs"), "fn foo() {} fn bar() {}").expect("write failed");
    for args in [&["add", "."][..], &["commit", "-m", "second"][..]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .expect("second commit command failed");
        assert!(output.status.success(), "git {args:?} failed");
    }

    let churn = file_churn(project, 90).await.expect("file_churn failed");
    let count = churn.get("file.rs").copied().unwrap_or(0);
    assert!(count >= 2, "file.rs should have churn >= 2, got {count}");
}

#[tokio::test]
async fn test_file_churn_nonexistent_dir() {
    let churn = file_churn(
        std::path::Path::new("/nonexistent/path/that/does/not/exist"),
        90,
    )
    .await
    .expect("file_churn should not error for nonexistent dir");
    assert!(churn.is_empty());
}
