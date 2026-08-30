use tracedecay_domain::code_intelligence::{
    Edge, EdgeKind, ExtractionResult, Node, NodeKind, UnresolvedRef, Visibility, generate_node_id,
};

fn make_node(id: &str, name: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: "src/lib.rs".to_string(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 5,
        start_column: 0,
        end_column: 0,
        signature: None,
        docstring: None,
        visibility: Visibility::Pub,
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

/// Drives every `NodeKind` variant off `NodeKind::ALL`, which the domain crate
/// keeps total with an exhaustive `match` — a new variant fails to compile
/// until it is in the table, so it cannot reach this test uncovered. Replaces
/// three hand-maintained lists that between them named 60 of the 63 variants
/// and left the protobuf kinds untested.
#[test]
fn node_kind_wire_strings_round_trip_for_every_variant() {
    for (kind, wire) in NodeKind::ALL {
        assert_eq!(
            kind.as_str(),
            wire,
            "NodeKind::{kind:?} no longer serializes to {wire:?}; node IDs embed \
             this string, so changing it invalidates stored IDs"
        );
        assert_eq!(
            NodeKind::from_str(wire).as_ref(),
            Some(&kind),
            "NodeKind::from_str({wire:?}) did not round-trip back to NodeKind::{kind:?}"
        );
    }
}

#[test]
fn node_kind_from_str_unknown_returns_none() {
    assert!(NodeKind::from_str("unknown_kind").is_none());
    assert!(NodeKind::from_str("").is_none());
}

/// Same contract as the `NodeKind` test above, driven off `EdgeKind::ALL`.
#[test]
fn edge_kind_wire_strings_round_trip_for_every_variant() {
    for (kind, wire) in EdgeKind::ALL {
        assert_eq!(
            kind.as_str(),
            wire,
            "EdgeKind::{kind:?} no longer serializes to {wire:?}"
        );
        assert_eq!(
            EdgeKind::from_str(wire),
            Some(kind),
            "EdgeKind::from_str({wire:?}) did not round-trip back to EdgeKind::{kind:?}"
        );
    }
}

#[test]
fn edge_kind_from_str_unknown_returns_none() {
    assert!(EdgeKind::from_str("unknown_edge").is_none());
    assert!(EdgeKind::from_str("").is_none());
}

#[test]
fn generate_node_id_is_deterministic() {
    let id1 = generate_node_id("src/main.rs", &NodeKind::Function, "main", 1);
    let id2 = generate_node_id("src/main.rs", &NodeKind::Function, "main", 1);
    assert_eq!(id1, id2, "same inputs must produce same ID");
}

#[test]
fn generate_node_id_format() {
    let id = generate_node_id("src/lib.rs", &NodeKind::Struct, "MyStruct", 10);

    // Format should be "kind:32hexchars"
    let parts: Vec<&str> = id.splitn(2, ':').collect();
    assert_eq!(parts.len(), 2, "ID should have exactly one colon separator");
    assert_eq!(parts[0], "struct", "prefix should be the node kind");
    assert_eq!(parts[1].len(), 32, "hex portion should be 32 characters");

    // Verify the hex portion contains only hex characters
    assert!(
        parts[1].chars().all(|c| c.is_ascii_hexdigit()),
        "hex portion should contain only hex digits"
    );
}

#[test]
fn generate_node_id_different_inputs_produce_different_ids() {
    let id1 = generate_node_id("src/main.rs", &NodeKind::Function, "main", 1);
    let id2 = generate_node_id("src/main.rs", &NodeKind::Function, "other", 1);
    let id3 = generate_node_id("src/main.rs", &NodeKind::Function, "main", 2);
    let id4 = generate_node_id("src/lib.rs", &NodeKind::Function, "main", 1);
    let id5 = generate_node_id("src/main.rs", &NodeKind::Struct, "main", 1);

    assert_ne!(id1, id2, "different names should produce different IDs");
    assert_ne!(id1, id3, "different lines should produce different IDs");
    assert_ne!(
        id1, id4,
        "different file paths should produce different IDs"
    );
    assert_ne!(id1, id5, "different kinds should produce different IDs");
}

/// Same `ALL`-driven contract as the two tests above, plus the two facts that
/// are not expressible in the table: `"pub"` is an inbound-only alias, and an
/// unset visibility must fall back to the most restrictive variant rather than
/// to the first one declared.
#[test]
fn visibility_wire_strings_round_trip_for_every_variant() {
    for (visibility, wire) in Visibility::ALL {
        assert_eq!(
            visibility.as_str(),
            wire,
            "Visibility::{visibility:?} no longer serializes to {wire:?}"
        );
        assert_eq!(
            Visibility::from_str(wire).as_ref(),
            Some(&visibility),
            "Visibility::from_str({wire:?}) did not round-trip back to \
             Visibility::{visibility:?}"
        );
    }
    assert_eq!(Visibility::from_str("pub"), Some(Visibility::Pub));
    assert!(Visibility::from_str("unknown").is_none());
    assert_eq!(Visibility::default(), Visibility::Private);
}

#[test]
fn extraction_result_sanitize_no_empty_names() {
    let good = make_node("function:aaa", "good_fn");
    let bad = make_node("function:bbb", "");

    let edge_good_to_good = Edge {
        source: "function:aaa".to_string(),
        target: "function:aaa".to_string(),
        kind: EdgeKind::Calls,
        line: None,
    };
    let edge_involving_bad = Edge {
        source: "function:bbb".to_string(),
        target: "function:aaa".to_string(),
        kind: EdgeKind::Calls,
        line: None,
    };
    let unresolved_bad = UnresolvedRef {
        from_node_id: "function:bbb".to_string(),
        reference_name: "something".to_string(),
        reference_kind: EdgeKind::Uses,
        line: 1,
        column: 0,
        file_path: "src/lib.rs".to_string(),
    };

    let mut result = ExtractionResult {
        nodes: vec![good, bad],
        edges: vec![edge_good_to_good.clone(), edge_involving_bad],
        unresolved_refs: vec![unresolved_bad],
        errors: vec![],
        duration_ms: 0,
    };

    result.sanitize();

    assert_eq!(result.nodes.len(), 1, "empty-name node should be removed");
    assert_eq!(
        result.edges.len(),
        1,
        "edge referencing bad node should be removed"
    );
    assert_eq!(edge_good_to_good.source, result.edges[0].source);
    assert!(
        result.unresolved_refs.is_empty(),
        "unresolved ref from bad node should be removed"
    );
    assert_eq!(
        result.errors.len(),
        1,
        "sanitize should log a stripped-node error"
    );
}

#[test]
fn extraction_result_sanitize_noop_when_clean() {
    let node = make_node("function:abc", "my_fn");
    let mut result = ExtractionResult {
        nodes: vec![node],
        edges: vec![],
        unresolved_refs: vec![],
        errors: vec![],
        duration_ms: 0,
    };
    result.sanitize();
    assert_eq!(result.nodes.len(), 1);
    assert!(result.errors.is_empty());
}
