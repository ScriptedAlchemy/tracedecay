use tempfile::TempDir;
use tokio::sync::OnceCell;
use tracedecay::db::Database;
use tracedecay::resolution::ReferenceResolver;
use tracedecay::types::*;

struct ResolutionFixture {
    _dir: TempDir,
    db: Database,
    nodes: Vec<Node>,
}

static RESOLUTION_FIXTURE: OnceCell<ResolutionFixture> = OnceCell::const_new();

async fn resolution_fixture() -> &'static ResolutionFixture {
    RESOLUTION_FIXTURE
        .get_or_init(|| async {
            let dir = TempDir::new().expect("failed to create temp dir");
            let (db, _) = Database::initialize(&dir.path().join("test.db"))
                .await
                .expect("failed to init db");
            let nodes = basic_nodes();

            for node in &nodes {
                db.insert_node(node).await.expect("failed to insert node");
            }

            ResolutionFixture {
                _dir: dir,
                db,
                nodes,
            }
        })
        .await
}

fn function_node(
    file_path: &str,
    name: &str,
    qualified_name: &str,
    start_line: u32,
    end_line: u32,
    signature: &str,
    visibility: Visibility,
) -> Node {
    Node {
        id: generate_node_id(file_path, &NodeKind::Function, name, start_line),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        file_path: file_path.to_string(),
        start_line,
        attrs_start_line: start_line,
        end_line,
        start_column: 0,
        end_column: 1,
        signature: Some(signature.to_string()),
        docstring: None,
        visibility,
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 0,
        parent_id: None,
    }
}

fn basic_nodes() -> Vec<Node> {
    vec![
        function_node(
            "src/utils.rs",
            "helper",
            "src/utils.rs::helper",
            1,
            5,
            "fn helper() -> i32",
            Visibility::Pub,
        ),
        function_node(
            "src/main.rs",
            "main",
            "src/main.rs::main",
            1,
            5,
            "fn main()",
            Visibility::Private,
        ),
    ]
}

async fn setup_db_with_nodes() -> (TempDir, Database) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let (db, _) = Database::initialize(&dir.path().join("test.db"))
        .await
        .expect("failed to init db");

    for node in basic_nodes() {
        db.insert_node(&node).await.expect("failed to insert node");
    }

    (dir, db)
}

#[tokio::test]
async fn test_resolve_exact_name_match() {
    let (_dir, db) = setup_db_with_nodes().await;
    let resolver = ReferenceResolver::from_nodes(&db, &db.get_all_nodes().await.unwrap());

    let uref = UnresolvedRef {
        from_node_id: generate_node_id("src/main.rs", &NodeKind::Function, "main", 1),
        reference_name: "helper".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 3,
        column: 12,
        file_path: "src/main.rs".to_string(),
    };

    let result = resolver.resolve_one(&uref);
    assert!(result.is_some(), "should resolve the helper reference");
    let resolved = result.unwrap();
    assert!(
        resolved.confidence >= 0.7,
        "confidence should be at least 0.7, got {}",
        resolved.confidence
    );
    assert_eq!(
        resolved.target_node_id,
        generate_node_id("src/utils.rs", &NodeKind::Function, "helper", 1),
    );
}

#[tokio::test]
async fn test_resolve_qualified_name_match() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let uref = UnresolvedRef {
        from_node_id: generate_node_id("src/main.rs", &NodeKind::Function, "main", 1),
        reference_name: "src/utils.rs::helper".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 3,
        column: 12,
        file_path: "src/main.rs".to_string(),
    };

    let result = resolver.resolve_one(&uref);
    assert!(result.is_some(), "should resolve via qualified name match");
    let resolved = result.unwrap();
    assert!(
        (resolved.confidence - 0.95).abs() < f64::EPSILON,
        "qualified match should have confidence 0.95, got {}",
        resolved.confidence
    );
    assert_eq!(resolved.resolved_by, "qualified-match");
}

#[tokio::test]
async fn test_resolve_all() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let refs = vec![UnresolvedRef {
        from_node_id: generate_node_id("src/main.rs", &NodeKind::Function, "main", 1),
        reference_name: "helper".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 3,
        column: 12,
        file_path: "src/main.rs".to_string(),
    }];

    let result = resolver.resolve_all(&refs);
    assert_eq!(result.total, 1);
    assert_eq!(result.resolved_count, 1);
    assert_eq!(result.resolved.len(), 1);
    assert!(result.unresolved.is_empty());
}

#[tokio::test]
async fn test_unresolvable_reference() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let uref = UnresolvedRef {
        from_node_id: "function:caller".to_string(),
        reference_name: "nonexistent".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 5,
        column: 8,
        file_path: "src/main.rs".to_string(),
    };

    assert!(
        resolver.resolve_one(&uref).is_none(),
        "nonexistent reference should not resolve"
    );
}

#[tokio::test]
async fn test_unresolvable_in_resolve_all() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let refs = vec![
        UnresolvedRef {
            from_node_id: generate_node_id("src/main.rs", &NodeKind::Function, "main", 1),
            reference_name: "helper".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            column: 12,
            file_path: "src/main.rs".to_string(),
        },
        UnresolvedRef {
            from_node_id: "function:caller".to_string(),
            reference_name: "nonexistent".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 5,
            column: 8,
            file_path: "src/main.rs".to_string(),
        },
    ];

    let result = resolver.resolve_all(&refs);
    assert_eq!(result.total, 2);
    assert_eq!(result.resolved_count, 1);
    assert_eq!(result.unresolved.len(), 1);
    assert_eq!(result.unresolved[0].reference_name, "nonexistent");
}

#[tokio::test]
async fn test_creates_edges_from_resolved() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let resolved = ResolvedRef {
        original: UnresolvedRef {
            from_node_id: generate_node_id("src/main.rs", &NodeKind::Function, "main", 1),
            reference_name: "helper".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            column: 12,
            file_path: "src/main.rs".to_string(),
        },
        target_node_id: generate_node_id("src/utils.rs", &NodeKind::Function, "helper", 1),
        confidence: 0.9,
        resolved_by: "exact-match".to_string(),
    };

    let edges = resolver.create_edges(&[resolved]);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].kind, EdgeKind::Calls);
    assert_eq!(edges[0].line, Some(3));
    assert_eq!(
        edges[0].source,
        generate_node_id("src/main.rs", &NodeKind::Function, "main", 1)
    );
    assert_eq!(
        edges[0].target,
        generate_node_id("src/utils.rs", &NodeKind::Function, "helper", 1)
    );
}

#[tokio::test]
async fn test_multiple_candidates_best_match_scoring() {
    // Two nodes with the same name "process" in different files.
    let same_file_node = function_node(
        "src/main.rs",
        "process",
        "src/main.rs::process",
        10,
        15,
        "fn process()",
        Visibility::Private,
    );
    let other_file_node = function_node(
        "src/other.rs",
        "process",
        "src/other.rs::process",
        1,
        5,
        "fn process()",
        Visibility::Pub,
    );
    let caller = function_node(
        "src/main.rs",
        "run",
        "src/main.rs::run",
        1,
        5,
        "fn run()",
        Visibility::Private,
    );
    let nodes = vec![same_file_node.clone(), other_file_node, caller.clone()];
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &nodes);

    // Reference from src/main.rs should prefer the same-file candidate.
    let uref = UnresolvedRef {
        from_node_id: caller.id.clone(),
        reference_name: "process".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 3,
        column: 4,
        file_path: "src/main.rs".to_string(),
    };

    let result = resolver.resolve_one(&uref);
    assert!(result.is_some(), "should resolve with multiple candidates");
    let resolved = result.unwrap();
    assert_eq!(
        resolved.target_node_id, same_file_node.id,
        "should prefer the same-file candidate"
    );
    assert!(
        (resolved.confidence - 0.7).abs() < f64::EPSILON,
        "multiple-match confidence should be 0.7, got {}",
        resolved.confidence
    );
}

#[tokio::test]
async fn test_create_edges_empty_input() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let edges = resolver.create_edges(&[]);
    assert!(edges.is_empty());
}

#[tokio::test]
async fn test_resolve_all_empty_input() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.db, &fixture.nodes);

    let result = resolver.resolve_all(&[]);
    assert_eq!(result.total, 0);
    assert_eq!(result.resolved_count, 0);
    assert!(result.resolved.is_empty());
    assert!(result.unresolved.is_empty());
}
