use tracedecay_code_extraction::GoExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

include!("support/edges.rs");
include!("support/calls.rs");

#[test]
fn test_go_extract_package() {
    let source = r#"package main

import "fmt"

func main() {
    fmt.Println("hello")
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let pkgs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::GoPackage)
        .collect();
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "main");
}

#[test]
fn test_go_extract_function() {
    let source = r#"package main

// Add adds two numbers.
func Add(a, b int) int {
    return a + b
}

func helper() {}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("math.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 2);
    let add_fn = fns.iter().find(|f| f.name == "Add").unwrap();
    assert_eq!(add_fn.visibility, Visibility::Pub); // uppercase = exported
    assert!(
        add_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Add adds two numbers")
    );
    let helper_fn = fns.iter().find(|f| f.name == "helper").unwrap();
    assert_eq!(helper_fn.visibility, Visibility::Private); // lowercase = unexported
}

#[test]
fn test_go_extract_struct_with_fields() {
    let source = r#"package model

// Point represents a 2D point.
type Point struct {
    X float64
    Y float64
    label string
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("model/point.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let structs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Struct)
        .collect();
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].name, "Point");
    assert_eq!(structs[0].visibility, Visibility::Pub);
    let fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field)
        .collect();
    assert_eq!(fields.len(), 3);
    // X is exported, label is not
    let x_field = fields.iter().find(|f| f.name == "X").unwrap();
    assert_eq!(x_field.visibility, Visibility::Pub);
    let label_field = fields.iter().find(|f| f.name == "label").unwrap();
    assert_eq!(label_field.visibility, Visibility::Private);
}

#[test]
fn test_go_extract_struct_tags() {
    let source = r#"package model

type Config struct {
    Name string `json:"name" yaml:"name"`
    Port int    `json:"port"`
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("model/config.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let tags: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::StructTag)
        .map(|n| (n.name.as_str(), n.signature.as_deref()))
        .collect();
    assert_eq!(
        tags,
        [
            ("Name:tag", Some(r#"`json:"name" yaml:"name"`"#)),
            ("Port:tag", Some(r#"`json:"port"`"#)),
        ]
    );
}

#[test]
fn test_go_extract_interface() {
    let source = r#"package io

// Reader is the interface for reading.
type Reader interface {
    Read(p []byte) (n int, err error)
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("io/reader.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let ifaces: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::InterfaceType)
        .collect();
    assert_eq!(ifaces.len(), 1);
    assert_eq!(ifaces[0].name, "Reader");
    assert_eq!(ifaces[0].visibility, Visibility::Pub);
}

#[test]
fn test_go_extract_method_with_receiver() {
    let source = r#"package model

type Circle struct {
    Radius float64
}

// Area calculates the area.
func (c *Circle) Area() float64 {
    return 3.14159 * c.Radius * c.Radius
}

func (c Circle) String() string {
    return "circle"
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("model/circle.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::StructMethod)
        .collect();
    assert_eq!(methods.len(), 2);
    // Check Receives edges
    assert_eq!(
        edge_pairs(&result, EdgeKind::Receives),
        [("Area", "Circle"), ("String", "Circle")]
    );
}

#[test]
fn test_go_extract_imports() {
    let source = r#"package main

import (
    "fmt"
    "os"
    "github.com/pkg/errors"
)
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let uses: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(uses.len(), 3);
}

#[test]
fn test_go_extract_const_and_var() {
    let source = r#"package main

const MaxSize = 1024

var counter int
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 1);
    assert_eq!(consts[0].name, "MaxSize");
    let statics: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Static)
        .collect();
    assert_eq!(statics.len(), 1);
    assert_eq!(statics[0].name, "counter");
}

#[test]
fn test_go_extract_call_sites() {
    let source = r#"package main

import "fmt"

func greet(name string) {
    fmt.Println("Hello", name)
}

func main() {
    greet("world")
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(call_refs, ["fmt.Println", "greet"]);
}

#[test]
fn test_go_extract_type_alias() {
    let source = r#"package main

type StringSlice = []string
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let aliases: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::TypeAlias)
        .collect();
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases[0].name, "StringSlice");
}

#[test]
fn test_go_extract_interface_embedding() {
    let source = r#"package io

type Reader interface {
    Read(p []byte) (int, error)
}

type ReadWriter interface {
    Reader
    Write(p []byte) (int, error)
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("io/io.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // Should have an Extends edge or unresolved ref for Reader embedded in ReadWriter
    let has_extends = result.edges.iter().any(|e| e.kind == EdgeKind::Extends)
        || result
            .unresolved_refs
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Extends);
    assert!(has_extends, "should detect interface embedding as Extends");
}

#[test]
fn test_go_extract_generic_function() {
    let source = r#"package main

func Map[T any, U any](s []T, f func(T) U) []U {
    r := make([]U, len(s))
    for i, v := range s {
        r[i] = f(v)
    }
    return r
}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 1);
    assert_eq!(fns[0].name, "Map");
    let generics: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::GenericParam)
        .collect();
    assert_eq!(
        generics.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        ["T", "U"]
    );
}

#[test]
fn test_go_file_node_is_root() {
    let source = r#"package main

func main() {}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    let files: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "main.go");
}

#[test]
fn test_go_contains_edges() {
    let source = r#"package main

type Foo struct {
    Bar int
}

func (f Foo) Baz() {}
"#;
    let extractor = GoExtractor;
    let result = extractor.extract_artifact("main.go", source).result;
    assert_eq!(
        edge_pairs(&result, EdgeKind::Contains),
        [
            ("main.go", "main"),
            ("main.go", "Foo"),
            ("Foo", "Bar"),
            ("main.go", "Baz")
        ]
    );
}

#[test]
fn test_go_qualified_names() {
    let source = r#"package server

func HandleRequest() {}
"#;
    let extractor = GoExtractor;
    let result = extractor
        .extract_artifact("pkg/server/handler.go", source)
        .result;
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 1);
    assert!(fns[0].qualified_name.contains("HandleRequest"));
    assert!(fns[0].qualified_name.contains("handler.go"));
}

#[test]
fn test_go_calls_inside_func_literals_belong_to_the_enclosing_function() {
    let source = r#"package main

func run(items []string) {
	go func() {
		work()
	}()
	each(items, func(s string) {
		handle(s)
	})
}
"#;
    let result = GoExtractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let run = result.nodes.iter().find(|n| n.name == "run").expect("run");
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls && r.from_node_id == run.id)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(calls, ["work", "each", "handle"]);
}

#[test]
fn test_go_package_var_initializers_own_their_calls() {
    let source = r#"package main

var registry = buildRegistry()

var (
	cache = newCache(8)
)

func run() {
	work()
}
"#;
    let result = GoExtractor.extract_artifact("main.go", source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    assert_eq!(
        calls_by_owner(&result),
        [
            ("static", "registry", 2, vec!["buildRegistry"]),
            ("static", "cache", 5, vec!["newCache"]),
            ("function", "run", 8, vec!["work"]),
        ]
    );
}

#[test]
fn go_interface_method_specs_are_methods_the_interface_contains() {
    let source = "package shapes\n\
\n\
type Shape interface {\n\
\t// Area is the enclosed area.\n\
\tArea() float64\n\
\tScale(factor float64) Shape\n\
}\n\
\n\
type Circle struct{ R float64 }\n\
\n\
func (c Circle) Area() float64 { return c.R }\n";
    let result = GoExtractor
        .extract_artifact("shapes/shapes.go", source)
        .result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::AbstractMethod | NodeKind::StructMethod))
        .map(|n| {
            (
                n.kind.clone(),
                n.qualified_name.as_str(),
                n.start_line,
                n.signature.as_deref(),
                n.docstring.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        methods,
        vec![
            (
                NodeKind::AbstractMethod,
                "shapes/shapes.go::Shape::Area",
                4,
                Some("Area() float64"),
                Some("Area is the enclosed area."),
            ),
            (
                NodeKind::AbstractMethod,
                "shapes/shapes.go::Shape::Scale",
                5,
                Some("Scale(factor float64) Shape"),
                None,
            ),
            (
                NodeKind::StructMethod,
                "shapes/shapes.go::Area",
                10,
                Some("func (c Circle) Area() float64"),
                None,
            ),
        ]
    );
    let contains = edge_pairs(&result, EdgeKind::Contains);
    assert!(contains.contains(&("Shape", "Area")), "{contains:?}");
    assert!(contains.contains(&("Shape", "Scale")), "{contains:?}");
}

fn go_method_set_rows(source: &str) -> Vec<(String, tracedecay_code_extraction::GoMethodSetRowV1)> {
    let artifact = GoExtractor.extract_artifact("pkg/file.go", source);
    assert!(
        artifact.result.errors.is_empty(),
        "errors: {:?}",
        artifact.result.errors
    );
    let mut rows: Vec<_> = artifact
        .go_method_sets
        .iter()
        .map(|row| {
            let name = artifact
                .result
                .nodes
                .iter()
                .find(|node| node.id == row.node_id)
                .map(|node| node.name.clone())
                .expect("method-set row names an extracted node");
            (name, row.row.clone())
        })
        .collect();
    rows.sort();
    rows
}

fn text(token: &str) -> tracedecay_code_extraction::GoTypeTokenV1 {
    tracedecay_code_extraction::GoTypeTokenV1::Text(token.to_owned())
}

fn local(name: &str) -> tracedecay_code_extraction::GoTypeTokenV1 {
    tracedecay_code_extraction::GoTypeTokenV1::Local(name.to_owned())
}

#[test]
fn test_go_method_signature_strips_names_and_expands_grouped_params() {
    use tracedecay_code_extraction::{GoMethodSetRowV1, GoMethodSignatureV1};
    let rows = go_method_set_rows(
        r#"package calc

type Simple struct{}

func (s *Simple) Add(a, b int) (sum int, err error) { return a + b, nil }
func (Simple) Log(format string, args ...any) {}
"#,
    );
    assert_eq!(
        rows,
        [
            (
                "Add".to_owned(),
                GoMethodSetRowV1::Receiver {
                    type_name: "Simple".to_owned(),
                    method: GoMethodSignatureV1 {
                        name: "Add".to_owned(),
                        params: vec![vec![text("int")], vec![text("int")]],
                        results: vec![vec![text("int")], vec![text("error")]],
                    },
                },
            ),
            (
                "Log".to_owned(),
                GoMethodSetRowV1::Receiver {
                    type_name: "Simple".to_owned(),
                    method: GoMethodSignatureV1 {
                        name: "Log".to_owned(),
                        params: vec![vec![text("string")], vec![text("..."), text("any")]],
                        results: vec![],
                    },
                },
            ),
            ("Simple".to_owned(), GoMethodSetRowV1::NamedType),
        ]
    );
}

#[test]
fn test_go_method_signature_tokenizes_qualified_and_composite_types() {
    use tracedecay_code_extraction::{GoMethodSetRowV1, GoTypeTokenV1};
    let rows = go_method_set_rows(
        r#"package shapes

import g "example.com/sat/geom"

type Box struct{}

func (Box) Map(m map[string]*g.Rect, f func(int) error) []Box { return nil }
"#,
    );
    let Some((_, GoMethodSetRowV1::Receiver { method, .. })) =
        rows.iter().find(|(name, _)| name == "Map")
    else {
        panic!("Map receiver row: {rows:?}");
    };
    assert_eq!(
        method.params,
        [
            vec![
                text("map"),
                text("["),
                text("string"),
                text("]"),
                text("*"),
                GoTypeTokenV1::Qualified {
                    package: "g".to_owned(),
                    name: "Rect".to_owned(),
                },
            ],
            vec![
                text("func"),
                text("("),
                text("int"),
                text(")"),
                text("("),
                text("error"),
                text(")"),
            ],
        ]
    );
    assert_eq!(method.results, [vec![text("["), text("]"), local("Box")]]);
}

#[test]
fn test_go_generic_receiver_binds_its_type_name() {
    use tracedecay_code_extraction::GoMethodSetRowV1;
    let source = r#"package gen

type List[T any] struct{ items []T }

func (l *List[T]) Len() int { return len(l.items) }
"#;
    let rows = go_method_set_rows(source);
    assert!(
        rows.iter().any(|(name, row)| name == "Len"
            && matches!(row, GoMethodSetRowV1::Receiver { type_name, .. } if type_name == "List")),
        "generic receiver row: {rows:?}"
    );
    let result = GoExtractor.extract_artifact("gen/list.go", source).result;
    assert_eq!(edge_pairs(&result, EdgeKind::Receives), [("Len", "List")]);
}

#[test]
fn test_go_interface_rows_record_methods_embeddings_and_generics() {
    use tracedecay_code_extraction::{GoMethodSetRowV1, GoMethodSignatureV1, GoTypeTokenV1};
    let rows = go_method_set_rows(
        r#"package io

import "io"

type Rows interface {
    io.Reader
    Count() int
}

type Box[T any] interface {
    Get() T
}
"#,
    );
    assert_eq!(
        rows,
        [
            ("Box".to_owned(), GoMethodSetRowV1::GenericInterface),
            (
                "Rows".to_owned(),
                GoMethodSetRowV1::InterfaceMethod {
                    method: GoMethodSignatureV1 {
                        name: "Count".to_owned(),
                        params: vec![],
                        results: vec![vec![text("int")]],
                    },
                },
            ),
            (
                "Rows".to_owned(),
                GoMethodSetRowV1::Embeds {
                    embedded: vec![GoTypeTokenV1::Qualified {
                        package: "io".to_owned(),
                        name: "Reader".to_owned(),
                    }],
                },
            ),
        ]
    );
}

#[test]
fn test_go_embedded_fields_and_named_aliases_promote_methods() {
    use tracedecay_code_extraction::{GoMethodSetRowV1, GoTypeTokenV1};
    let rows = go_method_set_rows(
        r#"package wrap

import "bytes"

type Base struct{}

type Ptr struct{}

type List[T any] struct{}

type Wrapped struct {
    Base
    *Ptr
    bytes.Buffer
    List[int]
    name string
}

type Same = Base

type Many = []Base
"#,
    );
    let promotes =
        |name: &str, embedded| (name.to_owned(), GoMethodSetRowV1::Promotes { embedded });
    let buffer = GoTypeTokenV1::Qualified {
        package: "bytes".to_owned(),
        name: "Buffer".to_owned(),
    };
    assert_eq!(
        rows,
        [
            ("Base".to_owned(), GoMethodSetRowV1::NamedType),
            ("List".to_owned(), GoMethodSetRowV1::NamedType),
            ("Many".to_owned(), GoMethodSetRowV1::NamedType),
            ("Ptr".to_owned(), GoMethodSetRowV1::NamedType),
            ("Same".to_owned(), GoMethodSetRowV1::NamedType),
            promotes("Same", vec![local("Base")]),
            ("Wrapped".to_owned(), GoMethodSetRowV1::NamedType),
            promotes("Wrapped", vec![local("Base")]),
            promotes("Wrapped", vec![local("List")]),
            promotes("Wrapped", vec![local("Ptr")]),
            promotes("Wrapped", vec![buffer]),
        ]
    );
}
