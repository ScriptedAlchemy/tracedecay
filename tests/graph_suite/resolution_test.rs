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
            let (db, _) = crate::common::initialize_test_database(&dir.path().join("test.db"))
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
    let (db, _) = crate::common::initialize_test_database(&dir.path().join("test.db"))
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
    let resolver = ReferenceResolver::from_nodes(&db.get_all_nodes().await.unwrap());

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
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

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
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

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
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

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
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

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
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

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
    let resolver = ReferenceResolver::from_nodes(&nodes);

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

/// Nodes exercising the `::`-prefilter fix: qualified names are file-path
/// prefixed, so references that carry a `crate::`/`Self::`/longer module
/// prefix never appear verbatim in `known_names`. Before the fix these were
/// bucketed hopeless by `resolve_all`'s prefilter and never reached
/// `resolve_one`'s simple-name fallback.
fn prefilter_nodes() -> Vec<Node> {
    vec![
        // Top-level pub handler in a `handlers` module. Only `handle_ping`
        // and (the full qn) are in known_names — NOT `handlers::handle_ping`
        // nor `crate::handlers::handle_ping`.
        function_node(
            "src/handlers.rs",
            "handle_ping",
            "src/handlers.rs::handle_ping",
            10,
            15,
            "pub fn handle_ping()",
            Visibility::Pub,
        ),
        // A method: qn suffix index has `Service::process` and `process`,
        // but never `Self::process`.
        function_node(
            "src/service.rs",
            "process",
            "src/service.rs::Service::process",
            20,
            30,
            "fn process(&self)",
            Visibility::Pub,
        ),
        // An associated function reached via a longer path.
        function_node(
            "src/config.rs",
            "load",
            "src/config.rs::Config::load",
            5,
            9,
            "fn load() -> Config",
            Visibility::Pub,
        ),
    ]
}

/// Before/after guard for the `::`-prefilter fix. Each of these references
/// carries a prefix (`crate::`, `Self::`, or a longer module path) that is
/// absent from `known_names`, so on the pre-fix resolver they were partitioned
/// hopeless and never resolved. After admitting refs whose simple name is
/// known, each produces a call edge.
#[tokio::test]
async fn test_prefilter_admits_prefixed_module_calls() {
    let fixture = resolution_fixture().await;
    let nodes = prefilter_nodes();
    let resolver = ReferenceResolver::from_nodes(&nodes);

    let caller = generate_node_id("src/dispatch.rs", &NodeKind::Function, "dispatch", 1);
    let refs = vec![
        // crate::module::fn
        UnresolvedRef {
            from_node_id: caller.clone(),
            reference_name: "crate::handlers::handle_ping".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            column: 4,
            file_path: "src/dispatch.rs".to_string(),
        },
        // Self::method
        UnresolvedRef {
            from_node_id: caller.clone(),
            reference_name: "Self::process".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 4,
            column: 4,
            file_path: "src/dispatch.rs".to_string(),
        },
        // crate::module::Type::assoc_fn
        UnresolvedRef {
            from_node_id: caller.clone(),
            reference_name: "crate::config::Config::load".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 5,
            column: 4,
            file_path: "src/dispatch.rs".to_string(),
        },
    ];

    let result = resolver.resolve_all(&refs);
    assert_eq!(result.total, 3);
    assert_eq!(
        result.resolved_count,
        3,
        "all three prefixed refs should resolve via the simple-name fallback; \
         unresolved = {:?}",
        result
            .unresolved
            .iter()
            .map(|u| u.reference_name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(result.unresolved.is_empty());

    // Each resolved to the intended target.
    let target_for = |name: &str| {
        result
            .resolved
            .iter()
            .find(|r| r.original.reference_name == name)
            .map(|r| r.target_node_id.clone())
    };
    assert_eq!(
        target_for("crate::handlers::handle_ping"),
        Some(generate_node_id(
            "src/handlers.rs",
            &NodeKind::Function,
            "handle_ping",
            10
        ))
    );
    assert_eq!(
        target_for("Self::process"),
        Some(generate_node_id(
            "src/service.rs",
            &NodeKind::Function,
            "process",
            20
        ))
    );
    assert_eq!(
        target_for("crate::config::Config::load"),
        Some(generate_node_id(
            "src/config.rs",
            &NodeKind::Function,
            "load",
            5
        ))
    );
}

/// Regression for dispatch-table dead-code false positives: a handler invoked
/// only through a `match` arm calling `crate::handlers::handle_ping` must gain
/// at least one caller edge once the prefilter admits the prefixed ref.
#[tokio::test]
async fn test_dispatch_handler_gains_caller_edge() {
    let fixture = resolution_fixture().await;
    let nodes = prefilter_nodes();
    let resolver = ReferenceResolver::from_nodes(&nodes);

    let handler_id = generate_node_id("src/handlers.rs", &NodeKind::Function, "handle_ping", 10);
    let dispatch_ref = UnresolvedRef {
        from_node_id: generate_node_id("src/dispatch.rs", &NodeKind::Function, "dispatch", 1),
        reference_name: "crate::handlers::handle_ping".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 42,
        column: 8,
        file_path: "src/dispatch.rs".to_string(),
    };

    let result = resolver.resolve_all(&[dispatch_ref]);
    let edges = resolver.create_edges(&result.resolved);
    let callers = edges
        .iter()
        .filter(|e| e.target == handler_id && e.kind == EdgeKind::Calls)
        .count();
    assert!(
        callers >= 1,
        "dispatch handler should have >=1 caller after the prefilter fix"
    );
}

/// Collision hygiene: two same-named functions in different modules with a
/// prefixed qualified call. Strategy-2 resolves via the simple name and does
/// NOT use the qualified module path to disambiguate — it falls to
/// `find_best_match` heuristic scoring (confidence 0.7). We assert it resolves
/// to one of the real candidates (never fabricates a target) and document that
/// module-path disambiguation is a deliberate follow-up, not part of this fix.
#[tokio::test]
async fn test_prefilter_collision_resolves_without_fabricating() {
    let render_a = function_node(
        "src/a.rs",
        "render",
        "src/a.rs::feature_a::render",
        10,
        20,
        "pub fn render()",
        Visibility::Pub,
    );
    let render_b = function_node(
        "src/b.rs",
        "render",
        "src/b.rs::feature_b::render",
        30,
        40,
        "pub fn render()",
        Visibility::Pub,
    );
    let nodes = vec![render_a.clone(), render_b.clone()];
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&nodes);

    // Neutral caller file so neither candidate wins on same-file proximity.
    let uref = UnresolvedRef {
        from_node_id: generate_node_id("src/dispatch.rs", &NodeKind::Function, "dispatch", 1),
        reference_name: "crate::feature_a::render".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 3,
        column: 4,
        file_path: "src/dispatch.rs".to_string(),
    };

    let result = resolver.resolve_one(&uref);
    let resolved = result.expect("collision ref still resolves via simple name");
    assert!(
        resolved.target_node_id == render_a.id || resolved.target_node_id == render_b.id,
        "must resolve to one of the real candidates, got {}",
        resolved.target_node_id
    );
    assert!(
        (resolved.confidence - 0.7).abs() < f64::EPSILON,
        "multi-candidate simple-name match uses find_best_match confidence 0.7, got {}",
        resolved.confidence
    );
    // NOTE: current Strategy-2 does not consult the `feature_a` module path
    // segment; it tie-breaks by heuristic score (here: first candidate).
    // Qualified module-path disambiguation is a documented follow-up.
}

#[tokio::test]
async fn test_create_edges_empty_input() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

    let edges = resolver.create_edges(&[]);
    assert!(edges.is_empty());
}

#[tokio::test]
async fn test_resolve_all_empty_input() {
    let fixture = resolution_fixture().await;
    let resolver = ReferenceResolver::from_nodes(&fixture.nodes);

    let result = resolver.resolve_all(&[]);
    assert_eq!(result.total, 0);
    assert_eq!(result.resolved_count, 0);
    assert!(result.resolved.is_empty());
    assert!(result.unresolved.is_empty());
}

/// Review-fix guard: an external qualified call (`serde_json::to_string`)
/// must NOT be admitted by simple name just because the project defines a
/// local `to_string` — the leading segment is neither a crate-relative
/// keyword nor a known local name, so the ref stays hopeless and no bogus
/// local call edge is fabricated.
#[tokio::test]
async fn test_prefilter_rejects_external_crate_qualified_calls() {
    let fixture = resolution_fixture().await;
    let nodes = vec![function_node(
        "src/render.rs",
        "to_string",
        "src/render.rs::to_string",
        3,
        7,
        "fn to_string() -> String",
        Visibility::Pub,
    )];
    let resolver = ReferenceResolver::from_nodes(&nodes);

    let caller = generate_node_id("src/main.rs", &NodeKind::Function, "main", 1);
    let refs = vec![
        UnresolvedRef {
            from_node_id: caller.clone(),
            reference_name: "serde_json::to_string".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 3,
            column: 4,
            file_path: "src/main.rs".to_string(),
        },
        UnresolvedRef {
            from_node_id: caller,
            reference_name: "tokio::spawn".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 4,
            column: 4,
            file_path: "src/main.rs".to_string(),
        },
    ];

    let result = resolver.resolve_all(&refs);
    assert_eq!(result.total, 2);
    assert_eq!(
        result.resolved_count, 0,
        "external qualified calls must not bind to same-named local items; \
         resolved = {:?}",
        result.resolved
    );
    assert_eq!(result.unresolved.len(), 2);
}

/// Plain `module::fn` calls (no `crate::` prefix) stay admitted when the
/// module itself is a known node, which the Rust extractor emits for `mod`
/// items — so the external-crate rejection above cannot regress them.
#[tokio::test]
async fn test_prefilter_admits_known_module_qualified_calls() {
    let fixture = resolution_fixture().await;
    let mut nodes = prefilter_nodes();
    nodes.push(Node {
        id: generate_node_id("src/handlers.rs", &NodeKind::Module, "handlers", 1),
        kind: NodeKind::Module,
        name: "handlers".to_string(),
        qualified_name: "src/handlers.rs::handlers".to_string(),
        file_path: "src/handlers.rs".to_string(),
        ..nodes[0].clone()
    });
    let resolver = ReferenceResolver::from_nodes(&nodes);

    let caller = generate_node_id("src/dispatch.rs", &NodeKind::Function, "dispatch", 1);
    let refs = vec![UnresolvedRef {
        from_node_id: caller,
        reference_name: "handlers::handle_ping".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 3,
        column: 4,
        file_path: "src/dispatch.rs".to_string(),
    }];

    let result = resolver.resolve_all(&refs);
    assert_eq!(
        result.resolved_count, 1,
        "module-qualified call must resolve when the module is a known node; \
         unresolved = {:?}",
        result.unresolved
    );
}
