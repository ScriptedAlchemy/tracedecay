//! Integration tests that run each language extractor against realistic sample files.

use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

include!("support/edges.rs");

fn read_fixture(name: &str) -> String {
    let path = format!("../../tests/fixtures/{}", name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read {}: {}", path, e))
}

fn names<'a>(nodes: &[&'a Node]) -> Vec<&'a str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

fn kind_names(result: &ExtractionResult, kind: NodeKind) -> Vec<&str> {
    result
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.as_str())
        .collect()
}

fn docstring_of<'a>(result: &'a ExtractionResult, kind: NodeKind, name: &str) -> Option<&'a str> {
    result
        .nodes
        .iter()
        .find(|n| n.kind == kind && n.name == name)
        .unwrap_or_else(|| panic!("{kind:?} {name} not extracted"))
        .docstring
        .as_deref()
}

fn ref_names(result: &ExtractionResult, kind: EdgeKind) -> Vec<&str> {
    result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == kind)
        .map(|r| r.reference_name.as_str())
        .collect()
}

/// Names of the nodes `parent` directly contains, in emission order.
fn contained_children<'a>(result: &'a ExtractionResult, parent: &str) -> Vec<&'a str> {
    edge_pairs(result, EdgeKind::Contains)
        .into_iter()
        .filter(|(source, _)| *source == parent)
        .map(|(_, child)| child)
        .collect()
}

// ── TypeScript ──────────────────────────────────────────────────────────────

#[test]
fn test_fixture_typescript() {
    let source = read_fixture("sample.ts");
    let extractor = tracedecay_code_extraction::TypeScriptExtractor;
    let result = extractor.extract_artifact("sample.ts", &source).result;
    assert!(result.errors.is_empty(), "TS errors: {:?}", result.errors);

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(names(&imports), ["events", "path"]);

    // Const
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert!(consts.iter().any(|n| n.name == "MAX_RETRIES"));

    // Type alias
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::TypeAlias && n.name == "UserId")
    );

    // Interface
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Interface && n.name == "IUser")
    );

    // Enum
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Enum && n.name == "Role")
    );

    // Function
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "log")
    );

    // Exported class with decorator
    let class = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "UserService")
        .expect("UserService class not found");
    assert_eq!(class.visibility, Visibility::Pub);
    assert_eq!(
        contained_children(&result, "UserService"),
        [
            "id",
            "name",
            "_cache",
            "settings",
            "constructor",
            "getDisplayName",
            "fetchProfile",
            "resetCache",
        ]
    );

    // Methods including async
    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();
    assert_eq!(
        names(&methods),
        [
            "getDisplayName",
            "getDisplayName",
            "fetchProfile",
            "resetCache"
        ]
    );
    let fetch = methods
        .iter()
        .find(|m| m.name == "fetchProfile")
        .expect("fetchProfile method not found");
    assert!(fetch.is_async, "fetchProfile should be async");

    // Arrow function (export const createUser = ...)
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::ArrowFunction && n.name == "createUser")
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "console.log",
            "super",
            "fetch",
            "response.json",
            "this._cache.set",
            "log",
            "this._cache.clear",
        ]
    );
    assert_eq!(ref_names(&result, EdgeKind::Extends), ["EventEmitter"]);
    assert_eq!(ref_names(&result, EdgeKind::Implements), ["IUser"]);
}

// ── JavaScript ──────────────────────────────────────────────────────────────

#[test]
fn test_fixture_javascript() {
    let source = read_fixture("sample.js");
    let extractor = tracedecay_code_extraction::TypeScriptExtractor;
    let result = extractor.extract_artifact("sample.js", &source).result;
    assert!(result.errors.is_empty(), "JS errors: {:?}", result.errors);

    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Handler")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "JsonHandler")
    );
    let fetch_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "fetchData")
        .expect("fetchData function");
    assert!(fetch_fn.is_async);
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::ArrowFunction && n.name == "double")
    );
}

// ── Python ──────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_python() {
    let source = read_fixture("sample.py");
    let extractor = tracedecay_code_extraction::PythonExtractor;
    let result = extractor.extract_artifact("sample.py", &source).result;
    assert!(
        result.errors.is_empty(),
        "Python errors: {:?}",
        result.errors
    );

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(
        names(&imports),
        ["os", "pathlib.Path", "typing.List", "typing.Optional"]
    );

    // Module-level constants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "MAX_CONNECTIONS")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "DEFAULT_TIMEOUT")
    );

    // Functions
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "log")
    );
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Log a message to stdout.")
    );

    // Decorator
    let decorators: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Decorator)
        .collect();
    assert_eq!(names(&decorators), ["retry", "property"]);

    // Classes
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Base")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Connection")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Pool")
    );

    // Docstring on class
    let conn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "Connection")
        .unwrap();
    assert_eq!(
        conn.docstring.as_deref(),
        Some("Manages a network connection.")
    );

    // Methods, with the nested Config class owned by Connection
    assert_eq!(
        contained_children(&result, "Base"),
        ["__init__", "__repr__", "_internal_method"]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "__init__",
            "connect",
            "disconnect",
            "is_connected",
            "Config"
        ]
    );
    assert_eq!(
        contained_children(&result, "Pool"),
        ["__init__", "acquire", "release"]
    );
    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();

    // Async method
    let connect = methods
        .iter()
        .find(|m| m.name == "connect")
        .expect("connect method not found");
    assert!(connect.is_async, "connect should be async");

    // Visibility: _internal_method is private
    let internal = methods
        .iter()
        .find(|m| m.name == "_internal_method")
        .expect("_internal_method");
    assert_eq!(internal.visibility, Visibility::Private);

    // Inheritance
    assert_eq!(
        ref_names(&result, EdgeKind::Extends),
        ["Base", "Connection"]
    );

    // Call sites
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "print",
            "super",
            "super().__init__",
            "log",
            "super",
            "super().__init__",
            "self._connections.pop",
            "Connection",
            "conn.connect",
            "self._connections.append",
        ]
    );

    // Signature with type annotations should not be truncated
    assert_eq!(
        log_fn.signature.as_deref(),
        Some("def log(message: str) -> None")
    );
}

// ── C ───────────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_c() {
    let source = read_fixture("sample.c");
    let extractor = tracedecay_code_extraction::CExtractor;
    let result = extractor.extract_artifact("sample.c", &source).result;
    assert!(result.errors.is_empty(), "C errors: {:?}", result.errors);

    assert_eq!(
        kind_names(&result, NodeKind::Include),
        ["stdio.h", "stdlib.h", "string.h"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::PreprocessorDef),
        ["MAX_BUFFER_SIZE"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Typedef),
        ["Point", "Variant", "Status", "Callback"]
    );
    assert_eq!(kind_names(&result, NodeKind::Struct), ["Point", "Color"]);
    assert_eq!(
        kind_names(&result, NodeKind::Field),
        [
            "x",
            "y",
            "r",
            "g",
            "b",
            "a",
            "int_val",
            "float_val",
            "str_val",
        ]
    );
    assert_eq!(kind_names(&result, NodeKind::Union), ["Variant"]);
    assert_eq!(kind_names(&result, NodeKind::Enum), ["Status"]);
    assert_eq!(
        kind_names(&result, NodeKind::EnumVariant),
        [
            "STATUS_OK",
            "STATUS_ERROR",
            "STATUS_PENDING",
            "STATUS_TIMEOUT",
        ]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Function),
        [
            "point_distance",
            "point_new",
            "set_error",
            "process_variant",
            "main",
        ]
    );

    // Static function is private
    let set_err = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "set_error")
        .expect("set_error function");
    assert_eq!(set_err.visibility, Visibility::Private);

    assert_eq!(
        docstring_of(&result, NodeKind::Function, "point_distance"),
        Some("Compute the distance between two points.")
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "sqrt",
            "strncpy",
            "cb",
            "printf",
            "set_error",
            "point_new",
            "point_new",
            "point_distance",
            "printf",
            "process_variant",
        ]
    );
}

// ── C header ────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_c_header() {
    let source = read_fixture("sample.h");
    let extractor = tracedecay_code_extraction::CExtractor;
    let result = extractor.extract_artifact("sample.h", &source).result;
    assert!(
        result.errors.is_empty(),
        "C header errors: {:?}",
        result.errors
    );

    // File node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Preprocessor def
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::PreprocessorDef && n.name == "API_VERSION")
    );

    // Typedef struct
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Typedef && n.name == "Rect")
    );

    assert_eq!(kind_names(&result, NodeKind::Enum), ["LogLevel"]);
    assert_eq!(
        kind_names(&result, NodeKind::Function),
        ["rect_new", "rect_area", "rect_contains", "log_init"]
    );
}

// ── C++ ─────────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_cpp() {
    let source = read_fixture("sample.cpp");
    let extractor = tracedecay_code_extraction::CppExtractor;
    let result = extractor.extract_artifact("sample.cpp", &source).result;
    assert!(result.errors.is_empty(), "C++ errors: {:?}", result.errors);

    // Namespace
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Namespace && n.name == "geom")
    );

    // Struct
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "Vec2")
    );

    // Abstract class
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Shape")
    );

    // Derived classes
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Circle")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Rectangle")
    );

    assert_eq!(kind_names(&result, NodeKind::Template), ["FixedBuffer"]);
    assert_eq!(
        kind_names(&result, NodeKind::Method),
        [
            "length",
            "~Shape",
            "name",
            "area",
            "perimeter",
            "center",
            "radius",
            "~Rectangle",
            "area",
            "perimeter",
        ]
    );
    assert_eq!(
        kind_names(&result, NodeKind::AbstractMethod),
        ["area", "perimeter"]
    );
    assert_eq!(kind_names(&result, NodeKind::Enum), ["Color"]);
    // `} // namespace geom` trails code two lines above, so it documents nothing.
    assert_eq!(docstring_of(&result, NodeKind::Enum, "Color"), None);
    assert_eq!(kind_names(&result, NodeKind::Union), ["Number"]);
    assert_eq!(kind_names(&result, NodeKind::Typedef), ["EntityId"]);
    assert_eq!(
        kind_names(&result, NodeKind::Include),
        ["iostream", "string", "vector", "memory"]
    );

    // Preprocessor def
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::PreprocessorDef && n.name == "DEFAULT_CAPACITY")
    );

    // Static function is private
    let helper = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "internal_helper")
        .expect("internal_helper function");
    assert_eq!(helper.visibility, Visibility::Private);

    // Circle and Rectangle both extend Shape
    assert_eq!(ref_names(&result, EdgeKind::Extends), ["Shape", "Shape"]);

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "std::sqrt",
            "shape.name",
            "shape.area",
            "shape.perimeter",
            "print_shape",
            "print_shape",
            "buffer.push",
            "buffer.push",
            "internal_helper",
        ]
    );
}

// ── Kotlin ──────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_kotlin() {
    let source = read_fixture("sample.kt");
    let extractor = tracedecay_code_extraction::KotlinExtractor;
    let result = extractor.extract_artifact("sample.kt", &source).result;
    assert!(
        result.errors.is_empty(),
        "Kotlin errors: {:?}",
        result.errors
    );

    assert_eq!(
        kind_names(&result, NodeKind::KotlinPackage),
        ["com.example.app"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Use),
        ["kotlin.math.sqrt", "java.time.Instant"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::DataClass),
        ["Point", "Success", "Failure"]
    );
    assert_eq!(kind_names(&result, NodeKind::SealedClass), ["Result"]);
    assert_eq!(kind_names(&result, NodeKind::Trait), ["Repository"]);
    assert_eq!(
        kind_names(&result, NodeKind::AnnotationUsage),
        ["Target", "Retention", "Cacheable"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Class),
        ["Cacheable", "Entity", "User"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Property),
        ["MAX_RETRIES", "APP_NAME", "createdAt", "lastLogin"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::CompanionObject),
        ["Companion"]
    );
    assert_eq!(kind_names(&result, NodeKind::Enum), ["Role"]);
    assert_eq!(
        kind_names(&result, NodeKind::KotlinObject),
        ["Loading", "Logger"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Function),
        ["toSlug", "processUser", "helperFunction"]
    );

    // Visibility: protected helper
    let helper = result
        .nodes
        .iter()
        .find(|n| n.name == "helperFunction")
        .expect("helperFunction");
    assert_eq!(
        helper.visibility,
        Visibility::PubSuper,
        "protected should be PubSuper"
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "sqrt",
            "name.isNotBlank",
            "email.contains",
            "println",
            "mapOf",
            "User",
            "println",
            "println",
            "this.lowercase",
            "this.lowercase().replace",
            "repo.count",
            "Logger.info",
            "Result.Success",
            "User.guest",
            "Logger.info",
        ]
    );
}

// ── Dart ────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-dart")]
#[test]
fn test_fixture_dart() {
    let source = read_fixture("sample.dart");
    let extractor = tracedecay_code_extraction::DartExtractor;
    let result = extractor.extract_artifact("sample.dart", &source).result;
    assert!(result.errors.is_empty(), "Dart errors: {:?}", result.errors);

    assert_eq!(kind_names(&result, NodeKind::Library), ["sample"]);
    assert_eq!(
        kind_names(&result, NodeKind::Use),
        ["dart:async", "dart:convert"]
    );
    assert_eq!(kind_names(&result, NodeKind::Enum), ["LogLevel"]);
    // The abstract class is modelled as an interface
    assert_eq!(kind_names(&result, NodeKind::Interface), ["Serializable"]);

    // Mixin
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Mixin && n.name == "Timestamped")
    );

    // Class
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "User")
    );

    // Extension
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Extension && n.name == "StringUtils")
    );

    assert_eq!(
        kind_names(&result, NodeKind::Method),
        [
            "toJson",
            "toJsonString",
            "createdAt",
            "updatedAt",
            "age",
            "toJson",
            "fetchProfile",
            "_isValid",
            "_logAction",
            "toSlug",
            "isBlank",
        ]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Constructor),
        ["User", "User.guest"]
    );

    // Underscore-prefixed members are library-private
    let privates: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.visibility == Visibility::Private && n.name.starts_with('_'))
        .collect();
    assert_eq!(names(&privates), ["_email", "_isValid", "_logAction"]);

    // Async function
    let process = result
        .nodes
        .iter()
        .find(|n| n.name == "processUsers")
        .expect("processUsers function");
    assert!(process.is_async, "processUsers should be async");

    assert_eq!(
        kind_names(&result, NodeKind::TypeAlias),
        ["JsonMap", "Callback"]
    );
    assert_eq!(
        contained_children(&result, "StringUtils"),
        ["toSlug", "isBlank"]
    );
}

// ── C# ──────────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_csharp() {
    let source = read_fixture("sample.cs");
    let extractor = tracedecay_code_extraction::CSharpExtractor;
    let result = extractor.extract_artifact("sample.cs", &source).result;
    assert!(result.errors.is_empty(), "C# errors: {:?}", result.errors);

    assert_eq!(
        kind_names(&result, NodeKind::Namespace),
        ["SampleApp.Models"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Use),
        [
            "System",
            "System.Collections.Generic",
            "System.Threading.Tasks",
        ]
    );
    assert_eq!(kind_names(&result, NodeKind::Enum), ["LogLevel"]);
    assert_eq!(kind_names(&result, NodeKind::Record), ["AppConfig"]);
    assert_eq!(
        kind_names(&result, NodeKind::Delegate),
        ["StatusChangedHandler"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Interface),
        ["IEntity", "IRepository"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::AnnotationUsage),
        ["AttributeUsage", "Cacheable"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Class),
        ["CacheableAttribute", "Entity", "User"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Method),
        [
            "Validate",
            "FindByIdAsync",
            "GetAllAsync",
            "Validate",
            "Validate",
            "FetchProfileAsync",
            "LogAction",
            "DistanceTo",
        ]
    );
    assert_eq!(
        kind_names(&result, NodeKind::Constructor),
        ["CacheableAttribute", "Entity", "User", "Point"]
    );
    assert_eq!(
        kind_names(&result, NodeKind::CSharpProperty),
        [
            "Id",
            "Count",
            "TtlSeconds",
            "Id",
            "CreatedAt",
            "Name",
            "Level",
            "IsActive",
            "InstanceCount",
            "X",
            "Y",
        ]
    );
    assert_eq!(kind_names(&result, NodeKind::Event), ["StatusChanged"]);
    assert_eq!(
        kind_names(&result, NodeKind::Field),
        ["_email", "_instanceCount"]
    );
    assert_eq!(kind_names(&result, NodeKind::Struct), ["Point"]);

    // Visibility: private fields, internal property
    for field in ["_email", "_instanceCount"] {
        let node = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Field && n.name == field)
            .expect("field");
        assert_eq!(node.visibility, Visibility::Private, "{field}");
    }
    let internal: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.visibility == Visibility::PubCrate)
        .collect();
    assert_eq!(names(&internal), ["Level"]);

    // Async method
    let fetch = result
        .nodes
        .iter()
        .find(|m| m.kind == NodeKind::Method && m.name == "FetchProfileAsync")
        .expect("FetchProfileAsync method");
    assert!(fetch.is_async, "FetchProfileAsync should be async");

    assert_eq!(
        ref_names(&result, EdgeKind::Extends),
        ["Attribute", "IEntity", "Entity"]
    );
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "string.IsNullOrWhiteSpace",
            "_email.Contains",
            "Task.Delay",
            "Console.WriteLine",
            "StatusChanged?.Invoke",
            "new Dictionary<string, object>",
            "Level.ToString",
            "Console.WriteLine",
            "Math.Sqrt",
        ]
    );
}

// ── PHP ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-php")]
#[test]
fn test_fixture_php() {
    let source = read_fixture("sample.php");
    let extractor = tracedecay_code_extraction::PhpExtractor;
    let result = extractor.extract_artifact("sample.php", &source).result;
    assert!(result.errors.is_empty(), "PHP errors: {:?}", result.errors);

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Namespace (mapped to NodeKind::Module in PHP extractor)
    assert_eq!(kind_names(&result, NodeKind::Module), [r"App\Http"]);

    // Use nodes are the trait `use` declarations inside class bodies:
    // Connection uses Timestamps and Pool uses Loggable.
    assert_eq!(
        kind_names(&result, NodeKind::Use),
        ["Timestamps", "Loggable"]
    );

    // Interface and Trait (both mapped to NodeKind::Trait)
    assert_eq!(
        kind_names(&result, NodeKind::Trait),
        ["ConnectionInterface", "Timestamps", "Loggable"]
    );
    assert_eq!(kind_names(&result, NodeKind::Class), ["Connection", "Pool"]);
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "Timestamps",
            "host",
            "port",
            "connected",
            "__construct",
            "connect",
            "disconnect",
            "validatePort",
        ]
    );
    assert_eq!(kind_names(&result, NodeKind::Enum), ["ConnectionState"]);
    assert_eq!(
        kind_names(&result, NodeKind::Field),
        ["connectedAt", "host", "port", "connected", "size"]
    );

    // Visibility: private members
    let privates: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.visibility == Visibility::Private && n.kind != NodeKind::Use)
        .collect();
    assert_eq!(
        names(&privates),
        ["connectedAt", "port", "connected", "validatePort", "size"]
    );

    // Pool extends Connection
    assert_eq!(ref_names(&result, EdgeKind::Extends), ["Connection"]);
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        ["error_log", "log_message", "log_message"]
    );
}

// ── Pascal ──────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-pascal")]
#[test]
fn test_fixture_pascal() {
    let source = read_fixture("sample.pas");
    let extractor = tracedecay_code_extraction::PascalExtractor;
    let result = extractor.extract_artifact("sample.pas", &source).result;
    assert!(
        result.errors.is_empty(),
        "Pascal errors: {:?}",
        result.errors
    );

    assert_eq!(kind_names(&result, NodeKind::PascalUnit), ["SampleUnit"]);
    assert_eq!(kind_names(&result, NodeKind::Use), ["SysUtils", "Classes"]);

    // Constants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "MaxRetries")
    );

    // Record type
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::PascalRecord && n.name == "TPoint")
    );

    assert_eq!(kind_names(&result, NodeKind::Interface), ["ISerializable"]);
    assert_eq!(kind_names(&result, NodeKind::Class), ["TEntity", "TUser"]);
    assert_eq!(
        contained_children(&result, "TEntity"),
        [
            "FId",
            "FCreatedAt",
            "GetId",
            "Create",
            "Destroy",
            "Validate",
            "Id",
            "CreatedAt",
        ]
    );

    // Functions and procedures
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "PointDistance")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Procedure && n.name == "LogMessage")
    );

    assert_eq!(
        kind_names(&result, NodeKind::Property),
        ["Id", "CreatedAt", "Name", "Level"]
    );
    assert_eq!(contained_children(&result, "TPoint"), ["X", "Y"]);

    // Visibility: fields declared in `private` sections
    let private_fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field && n.visibility == Visibility::Private)
        .collect();
    assert_eq!(
        names(&private_fields),
        ["FId", "FCreatedAt", "FName", "FEmail", "FLevel"]
    );
}

// ── Ruby ────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-ruby")]
#[test]
fn test_fixture_ruby() {
    let source = read_fixture("sample.rb");
    let extractor = tracedecay_code_extraction::RubyExtractor;
    let result = extractor.extract_artifact("sample.rb", &source).result;
    assert!(result.errors.is_empty(), "Ruby errors: {:?}", result.errors);

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Module
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Module && n.name == "Networking"),
        "Networking module not found"
    );

    // Constants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "MAX_CONNECTIONS"),
        "MAX_CONNECTIONS constant not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "DEFAULT_TIMEOUT"),
        "DEFAULT_TIMEOUT constant not found"
    );

    // Classes
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Base"),
        "Base class not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Connection"),
        "Connection class not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Pool"),
        "Pool class not found"
    );

    // Nested class
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Config"),
        "nested Config class not found"
    );

    // `log` is defined inside a module, so it is a Method of Networking.
    assert_eq!(
        contained_children(&result, "Networking"),
        [
            "MAX_CONNECTIONS",
            "DEFAULT_TIMEOUT",
            "log",
            "Base",
            "Connection",
            "Pool",
        ]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "initialize",
            "connect",
            "disconnect",
            "connected?",
            "Config"
        ]
    );
    assert_eq!(
        contained_children(&result, "Config"),
        ["initialize", "valid?"]
    );

    // Inheritance: Connection < Base, Pool < Connection
    assert_eq!(
        ref_names(&result, EdgeKind::Extends),
        ["Base", "Connection"]
    );
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "puts", "class", "name", "raise", "super", "log", "super", "empty?", "new", "connect",
            "pop", "push",
        ]
    );
}

// -- Swift ────────────────────────────────────────────────────────────────────

#[test]
fn test_fixture_swift() {
    let source = read_fixture("sample.swift");
    let extractor = tracedecay_code_extraction::SwiftExtractor;
    let result = extractor.extract_artifact("sample.swift", &source).result;
    assert!(
        result.errors.is_empty(),
        "Swift errors: {:?}",
        result.errors
    );

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(names(&imports), ["Foundation", "UIKit"]);

    // Top-level constant
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "maxConnections"),
        "maxConnections constant not found"
    );

    // Typealias
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::TypeAlias && n.name == "CompletionHandler"),
        "CompletionHandler typealias not found"
    );

    // Enum
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Enum && n.name == "LogLevel"),
        "LogLevel enum not found"
    );

    // Enum variants
    let variants: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert_eq!(names(&variants), ["debug", "info", "warning", "error"]);

    // Protocol as Interface
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Interface && n.name == "Serializable"),
        "Serializable protocol not found"
    );

    // Classes
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Base"),
        "Base class not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Connection"),
        "Connection class not found"
    );

    // Struct
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "Point"),
        "Point struct not found"
    );

    // Extension
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Extension && n.name == "String"),
        "String extension not found"
    );

    assert_eq!(kind_names(&result, NodeKind::Constructor), ["init", "init"]);
    assert_eq!(
        kind_names(&result, NodeKind::Method),
        [
            "toJson",
            "toJsonString",
            "description",
            "validate",
            "connect",
            "disconnect",
            "distance",
            "toSlug",
        ]
    );

    // Top-level function
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "processUsers"),
        "processUsers function not found"
    );

    // Properties (inside classes/structs)
    let props: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .collect();
    assert_eq!(
        names(&props),
        ["name", "port", "connected", "isConnected", "x", "y"]
    );

    assert_eq!(
        docstring_of(&result, NodeKind::Class, "Base"),
        Some("Base class with shared functionality.")
    );

    // Inheritance: Connection extends Base
    assert_eq!(ref_names(&result, EdgeKind::Extends), ["Base"]);
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "type",
            "assert",
            "init",
            "print",
            "squareRoot",
            "lowercased",
            "replacingOccurrences",
            "map",
            "description",
        ]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "port",
            "connected",
            "init",
            "connect",
            "disconnect",
            "isConnected",
        ]
    );

    // Async method
    let connect = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == "connect")
        .expect("connect method");
    assert!(connect.is_async, "connect should be async");

    let privates: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.visibility == Visibility::Private && n.kind != NodeKind::Use)
        .collect();
    assert_eq!(names(&privates), ["validate", "connected"]);
}

// ── Bash ────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-bash")]
#[test]
fn test_fixture_bash() {
    let source = read_fixture("sample.sh");
    let extractor = tracedecay_code_extraction::BashExtractor;
    let result = extractor.extract_artifact("sample.sh", &source).result;
    assert!(result.errors.is_empty(), "Bash errors: {:?}", result.errors);

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Functions (5: log, validate_config, connect, disconnect, main)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 5, "expected 5 functions, got {}", fns.len());
    assert!(fns.iter().any(|n| n.name == "log"));
    assert!(fns.iter().any(|n| n.name == "validate_config"));
    assert!(fns.iter().any(|n| n.name == "connect"));
    assert!(fns.iter().any(|n| n.name == "disconnect"));
    assert!(fns.iter().any(|n| n.name == "main"));

    // Readonly constants (2: MAX_RETRIES, DEFAULT_PORT)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|n| n.name == "MAX_RETRIES"));
    assert!(consts.iter().any(|n| n.name == "DEFAULT_PORT"));

    // Source import (1 Use: ./utils.sh)
    let uses: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(uses.len(), 1, "expected 1 Use node, got {}", uses.len());
    assert_eq!(uses[0].name, "./utils.sh");

    // Docstrings
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Logs a message with timestamp.")
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "source",
            "echo",
            "date",
            "log",
            "return",
            "log",
            "return",
            "return",
            "log",
            "seq",
            "curl",
            "log",
            "return",
            "log",
            "sleep",
            "return",
            "log",
            "validate_config",
            "connect",
            "log",
            "exit",
            "disconnect",
            "main",
        ]
    );
    assert_eq!(
        contained_children(&result, "sample"),
        [
            "MAX_RETRIES",
            "DEFAULT_PORT",
            "./utils.sh",
            "log",
            "validate_config",
            "connect",
            "disconnect",
            "main",
        ]
    );
}

// ── Lua ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-lua")]
#[test]
fn test_fixture_lua() {
    let source = read_fixture("sample.lua");
    let extractor = tracedecay_code_extraction::LuaExtractor;
    let result = extractor.extract_artifact("sample.lua", &source).result;
    assert!(result.errors.is_empty(), "Lua errors: {:?}", result.errors);

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Requires (2: json, socket)
    let uses: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(uses.len(), 2, "expected 2 Use nodes, got {}", uses.len());
    assert!(uses.iter().any(|n| n.name == "json"));
    assert!(uses.iter().any(|n| n.name == "socket"));

    // Constants (2: MAX_RETRIES, DEFAULT_PORT)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|n| n.name == "MAX_RETRIES"));
    assert!(consts.iter().any(|n| n.name == "DEFAULT_PORT"));

    // Functions (3: log, Connection.new, Pool.new)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 3, "expected 3 functions, got {}", fns.len());
    assert!(fns.iter().any(|n| n.name == "log"));

    // Methods (5: connect, disconnect, isConnected, acquire, release)
    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();
    assert_eq!(
        methods.len(),
        5,
        "expected 5 methods, got {}",
        methods.len()
    );
    assert!(methods.iter().any(|n| n.name == "connect"));
    assert!(methods.iter().any(|n| n.name == "disconnect"));
    assert!(methods.iter().any(|n| n.name == "isConnected"));
    assert!(methods.iter().any(|n| n.name == "acquire"));
    assert!(methods.iter().any(|n| n.name == "release"));

    // Docstrings
    let lua_log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .unwrap();
    assert_eq!(
        lua_log_fn.docstring.as_deref(),
        Some(
            "Logs a message with the given level.\n\
             @param level string The log level\n\
             @param message string The message to log"
        )
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "print",
            "string.format",
            "setmetatable",
            "log",
            "setmetatable",
            "table.remove",
            "Connection.new",
            "conn:connect",
            "table.insert",
        ]
    );
    assert_eq!(
        contained_children(&result, "sample.lua"),
        [
            "json",
            "socket",
            "MAX_RETRIES",
            "DEFAULT_PORT",
            "log",
            "new",
            "connect",
            "disconnect",
            "isConnected",
            "new",
            "acquire",
            "release",
        ]
    );
}

// ── Zig ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-zig")]
#[test]
fn test_fixture_zig() {
    let source = read_fixture("sample.zig");
    let extractor = tracedecay_code_extraction::ZigExtractor;
    let result = extractor.extract_artifact("sample.zig", &source).result;
    assert!(result.errors.is_empty(), "Zig errors: {:?}", result.errors);

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports (2: std, std.mem)
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(
        imports.len(),
        2,
        "expected 2 imports, got {}",
        imports.len()
    );
    assert!(imports.iter().any(|n| n.name == "std"));

    // Const (max_connections)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert!(
        consts.iter().any(|n| n.name == "max_connections"),
        "max_connections constant not found"
    );

    // Enum (LogLevel)
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Enum && n.name == "LogLevel"),
        "LogLevel enum not found"
    );

    // Enum variants (4: debug, info, warning, err)
    let variants: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert_eq!(
        variants.len(),
        4,
        "expected 4 enum variants, got {}",
        variants.len()
    );
    assert!(variants.iter().any(|v| v.name == "debug"));
    assert!(variants.iter().any(|v| v.name == "info"));
    assert!(variants.iter().any(|v| v.name == "warning"));
    assert!(variants.iter().any(|v| v.name == "err"));

    // Structs (Point, Connection)
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "Point"),
        "Point struct not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "Connection"),
        "Connection struct not found"
    );

    // Fields
    // Fields and methods nest under their structs
    assert_eq!(
        contained_children(&result, "Point"),
        ["x", "y", "distance", "origin"]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "host",
            "port",
            "connected",
            "init",
            "connect",
            "disconnect",
            "isConnected",
        ]
    );

    // Top-level functions (log, processConnections)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert!(
        fns.iter().any(|f| f.name == "log"),
        "log function not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "processConnections"),
        "processConnections function not found"
    );

    // Test declaration as Function
    assert!(
        fns.iter().any(|f| f.name == "point distance"),
        "test 'point distance' not found"
    );

    // Visibility: pub functions
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .unwrap();
    assert_eq!(log_fn.visibility, Visibility::Pub, "log should be pub");
    let process_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "processConnections")
        .unwrap();
    assert_eq!(
        process_fn.visibility,
        Visibility::Pub,
        "processConnections should be pub"
    );

    // Docstrings
    let point = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Struct && n.name == "Point")
        .unwrap();
    assert_eq!(point.docstring.as_deref(), Some("A 2D point."));

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "std.debug.print",
            "std.debug.print",
            "conn.connect",
            "p1.distance",
            "std.testing.expectEqual",
        ]
    );
}

// ── Protobuf ────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-protobuf")]
#[test]
fn test_fixture_proto() {
    let source = read_fixture("sample.proto");
    let extractor = tracedecay_code_extraction::ProtoExtractor;
    let result = extractor.extract_artifact("sample.proto", &source).result;
    assert!(
        result.errors.is_empty(),
        "Proto errors: {:?}",
        result.errors
    );

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Package
    let pkgs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Package)
        .collect();
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].name, "networking");

    // Imports
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(
        imports.len(),
        2,
        "expected 2 imports, got {}",
        imports.len()
    );

    // Messages
    let msgs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::ProtoMessage)
        .collect();
    assert_eq!(
        names(&msgs),
        [
            "Endpoint",
            "ConnectionConfig",
            "AuthConfig",
            "ConnectionStatus",
            "DisconnectRequest",
            "HealthCheckRequest",
            "HealthCheckResponse",
        ]
    );

    // Enum + variants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Enum && n.name == "LogLevel")
    );
    let variants: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert_eq!(
        variants.len(),
        5,
        "expected 5 enum variants, got {}",
        variants.len()
    );

    // Service
    let services: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::ProtoService)
        .collect();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].name, "ConnectionService");

    // RPCs
    let rpcs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::ProtoRpc)
        .collect();
    assert_eq!(rpcs.len(), 3, "expected 3 rpcs, got {}", rpcs.len());
    assert!(rpcs.iter().any(|r| r.name == "Connect"));
    assert!(rpcs.iter().any(|r| r.name == "Disconnect"));
    assert!(rpcs.iter().any(|r| r.name == "HealthCheck"));

    // Fields, with the nested AuthConfig message owned by ConnectionConfig
    assert_eq!(
        contained_children(&result, "Endpoint"),
        ["host", "port", "tls"]
    );
    assert_eq!(
        contained_children(&result, "ConnectionConfig"),
        [
            "endpoint",
            "max_retries",
            "timeout_ms",
            "log_level",
            "AuthConfig",
            "auth",
            "round_robin",
            "least_connections",
        ]
    );
    assert_eq!(
        contained_children(&result, "AuthConfig"),
        ["token", "username"]
    );
    assert_eq!(
        contained_children(&result, "ConnectionService"),
        ["Connect", "Disconnect", "HealthCheck"]
    );

    assert_eq!(
        docstring_of(&result, NodeKind::ProtoMessage, "Endpoint"),
        Some("A network endpoint.")
    );
    assert_eq!(
        docstring_of(&result, NodeKind::Enum, "LogLevel"),
        Some("Represents the log level.")
    );
}

// ── Nix ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-nix")]
#[test]
fn test_fixture_nix() {
    let source = read_fixture("sample.nix");
    let extractor = tracedecay_code_extraction::NixExtractor;
    let result = extractor.extract_artifact("sample.nix", &source).result;
    assert!(result.errors.is_empty(), "Nix errors: {:?}", result.errors);

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Functions
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert!(
        fns.iter().any(|f| f.name == "log"),
        "log function not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "mkConnection"),
        "mkConnection function not found"
    );

    // Constants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "defaultPort")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "maxRetries")
    );

    // Module
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Module && n.name == "networking")
    );

    // Nested functions inside networking
    assert!(fns.iter().any(|f| f.name == "mkPool"), "mkPool not found");
    assert!(
        fns.iter().any(|f| f.name == "validateConfig"),
        "validateConfig not found"
    );

    // Docstrings
    let log_fn = fns.iter().find(|f| f.name == "log").unwrap();
    assert_eq!(log_fn.docstring.as_deref(), Some("Formats a log message."));
    assert_eq!(
        docstring_of(&result, NodeKind::Module, "networking"),
        Some("Networking utilities.")
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "builtins.trace",
            "builtins.trace",
            "toString",
            "toString",
            "builtins.genList",
            "builtins.genList",
            "mkConnection",
        ]
    );
    assert_eq!(
        contained_children(&result, "networking"),
        ["mkPool", "validateConfig", "defaultConfig"]
    );
    assert_eq!(
        contained_children(&result, "defaultConfig"),
        ["host", "port", "tls"]
    );

    // Inherit (Use) nodes
    let uses: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert!(
        uses.iter().any(|u| u.name == "networking"),
        "inherit networking Use not found"
    );

    // All visibility should be Pub
    for node in &result.nodes {
        assert_eq!(
            node.visibility,
            Visibility::Pub,
            "node {} should be Pub",
            node.name
        );
    }
}

// ── VB.NET ──────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-vbnet")]
#[test]
fn test_fixture_vbnet() {
    let source = read_fixture("sample.vb");
    let extractor = tracedecay_code_extraction::VbNetExtractor;
    let result = extractor.extract_artifact("sample.vb", &source).result;

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(
        imports.len(),
        2,
        "expected 2 imports, got {}",
        imports.len()
    );
    assert!(imports.iter().any(|u| u.name == "System"));
    assert!(
        imports
            .iter()
            .any(|u| u.name == "System.Collections.Generic")
    );

    // Const (top-level)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert!(
        consts.iter().any(|c| c.name == "MaxConnections"),
        "MaxConnections const not found"
    );

    // Enum with variants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Enum && n.name == "LogLevel")
    );
    let variants: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert_eq!(names(&variants), ["Debug", "Info", "Warning", "[Error]"]);

    // Interface
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Interface && n.name == "ISerializable"),
        "ISerializable interface not found"
    );

    // Classes
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Base"),
        "Base class not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Connection"),
        "Connection class not found"
    );

    // Struct
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "Point"),
        "Point struct not found"
    );

    // Module
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Module && n.name == "Helpers"),
        "Helpers module not found"
    );

    assert_eq!(
        contained_children(&result, "Base"),
        ["Name", "New", "Description", "Validate"]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "Port",
            "_connected",
            "New",
            "Connect",
            "Disconnect",
            "IsConnected",
            "ToJson",
        ]
    );
    assert_eq!(contained_children(&result, "Point"), ["X", "Y", "Distance"]);
    assert_eq!(contained_children(&result, "Helpers"), ["LogMessage"]);

    assert_eq!(
        docstring_of(&result, NodeKind::Class, "Base"),
        Some("Base class with shared functionality.")
    );

    // Inheritance: Connection extends Base
    assert!(
        result
            .unresolved_refs
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Extends && r.reference_name == "Base"),
        "expected Extends ref to Base"
    );

    // Implements: Connection implements ISerializable
    assert!(
        result.unresolved_refs.iter().any(
            |r| r.reference_kind == EdgeKind::Implements && r.reference_name == "ISerializable"
        ),
        "expected Implements ref to ISerializable"
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "Me.GetType",
            "Debug.Assert",
            "String.IsNullOrEmpty",
            "MyBase.New",
            "Console.WriteLine",
            "Math.Sqrt",
            "Console.WriteLine",
        ]
    );
}

// ── PowerShell ──────────────────────────────────────────────────────────────

#[cfg(feature = "lang-powershell")]
#[test]
fn test_fixture_powershell() {
    let source = read_fixture("sample.ps1");
    let extractor = tracedecay_code_extraction::PowerShellExtractor;
    let result = extractor.extract_artifact("sample.ps1", &source).result;
    assert!(
        result.errors.is_empty(),
        "PowerShell errors: {:?}",
        result.errors
    );

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Functions (5: Write-Log, Test-Config, Connect-Server, Disconnect-Server, Main)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 5, "expected 5 functions, got {}", fns.len());
    assert!(fns.iter().any(|n| n.name == "Write-Log"));
    assert!(fns.iter().any(|n| n.name == "Test-Config"));
    assert!(fns.iter().any(|n| n.name == "Connect-Server"));
    assert!(fns.iter().any(|n| n.name == "Disconnect-Server"));
    assert!(fns.iter().any(|n| n.name == "Main"));

    // Typed constants (2: MaxRetries, DefaultPort)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|n| n.name == "MaxRetries"));
    assert!(consts.iter().any(|n| n.name == "DefaultPort"));

    // Imports (2: Import-Module ActiveDirectory, . .\Utils.ps1)
    let uses: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(uses.len(), 2, "expected 2 Use nodes, got {}", uses.len());
    assert_eq!(names(&uses), ["ActiveDirectory", r".\Utils.ps1"]);

    assert_eq!(
        docstring_of(&result, NodeKind::Function, "Write-Log"),
        Some(
            ".SYNOPSIS\n    Logs a message with the given level.\n\
             .PARAMETER Level\n    The log level.\n\
             .PARAMETER Message\n    The message to log."
        )
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "Write-Host",
            "Get-Date",
            "Write-Log",
            "Write-Log",
            "Write-Log",
            "Test-Connection",
            "Write-Log",
            "Write-Log",
            "Start-Sleep",
            "Write-Log",
            "Test-Config",
            "Connect-Server",
            "Disconnect-Server",
        ]
    );
    assert_eq!(
        contained_children(&result, "sample.ps1"),
        [
            "ActiveDirectory",
            r".\Utils.ps1",
            "MaxRetries",
            "DefaultPort",
            "Write-Log",
            "Test-Config",
            "Connect-Server",
            "Disconnect-Server",
            "Main",
        ]
    );
}

// ── Batch ───────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-batch")]
#[test]
fn test_fixture_batch() {
    let source = read_fixture("sample.bat");
    let extractor = tracedecay_code_extraction::BatchExtractor;
    let result = extractor.extract_artifact("sample.bat", &source).result;
    assert!(
        result.errors.is_empty(),
        "Batch errors: {:?}",
        result.errors
    );

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Labels as functions (5: Log, ValidateConfig, Connect, Disconnect, Main)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 5, "expected 5 functions, got {}", fns.len());
    assert!(fns.iter().any(|n| n.name == "Log"));
    assert!(fns.iter().any(|n| n.name == "ValidateConfig"));
    assert!(fns.iter().any(|n| n.name == "Connect"));
    assert!(fns.iter().any(|n| n.name == "Disconnect"));
    assert!(fns.iter().any(|n| n.name == "Main"));

    // Set constants (2: MAX_RETRIES, DEFAULT_PORT)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|n| n.name == "MAX_RETRIES"));
    assert!(consts.iter().any(|n| n.name == "DEFAULT_PORT"));

    // Docstrings
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "Log")
        .unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Logs a message with timestamp.")
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "Log",
            "Log",
            "Log",
            "Log",
            "Log",
            "Log",
            "ValidateConfig",
            "Connect",
            "Disconnect",
        ]
    );
    assert_eq!(
        contained_children(&result, "sample.bat"),
        [
            "MAX_RETRIES",
            "DEFAULT_PORT",
            "Log",
            "ValidateConfig",
            "Connect",
            "Disconnect",
            "Main",
        ]
    );
}

// ── Perl ────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-perl")]
#[test]
fn test_fixture_perl() {
    let source = read_fixture("sample.pl");
    let extractor = tracedecay_code_extraction::PerlExtractor;
    let result = extractor.extract_artifact("sample.pl", &source).result;
    assert!(result.errors.is_empty(), "Perl errors: {:?}", result.errors);

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports (4: strict, warnings, File::Path, Carp)
    let imports: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(
        imports.len(),
        4,
        "expected 4 imports, got {}",
        imports.len()
    );
    assert!(imports.iter().any(|n| n.name == "strict"));
    assert!(imports.iter().any(|n| n.name == "warnings"));
    assert!(imports.iter().any(|n| n.name == "File::Path"));
    assert!(imports.iter().any(|n| n.name == "Carp"));

    // Constants (2: MAX_RETRIES, DEFAULT_PORT)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|n| n.name == "MAX_RETRIES"));
    assert!(consts.iter().any(|n| n.name == "DEFAULT_PORT"));

    // Packages as Modules (2: Connection, Pool)
    let modules: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .collect();
    assert_eq!(
        modules.len(),
        2,
        "expected 2 modules, got {}",
        modules.len()
    );
    assert!(modules.iter().any(|n| n.name == "Connection"));
    assert!(modules.iter().any(|n| n.name == "Pool"));

    // Top-level functions (2: log_message, validate_config)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 2, "expected 2 functions, got {}", fns.len());
    assert!(fns.iter().any(|n| n.name == "log_message"));
    assert!(fns.iter().any(|n| n.name == "validate_config"));

    // Methods inside packages (7: Connection::new, connect, disconnect, is_connected,
    //                              Pool::new, acquire, release)
    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();
    assert_eq!(
        methods.len(),
        7,
        "expected 7 methods, got {}",
        methods.len()
    );
    assert!(methods.iter().any(|n| n.name == "connect"));
    assert!(methods.iter().any(|n| n.name == "disconnect"));
    assert!(methods.iter().any(|n| n.name == "is_connected"));
    assert!(methods.iter().any(|n| n.name == "acquire"));
    assert!(methods.iter().any(|n| n.name == "release"));

    // Docstrings
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log_message")
        .unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Logs a message with the given level.")
    );
    assert_eq!(
        docstring_of(&result, NodeKind::Const, "MAX_RETRIES"),
        Some("Maximum number of retries.")
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "log_message",
            "main::log_message",
            "Connection->new",
            "$conn->connect",
            "croak",
            "croak",
        ]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        ["new", "connect", "disconnect", "is_connected"]
    );
    assert_eq!(
        contained_children(&result, "Pool"),
        ["new", "acquire", "release"]
    );
}

// ── Objective-C ─────────────────────────────────────────────────────────────

#[cfg(feature = "lang-objc")]
#[test]
fn test_fixture_objc() {
    let source = read_fixture("sample.m");
    let extractor = tracedecay_code_extraction::ObjcExtractor;
    let result = extractor.extract_artifact("sample.m", &source).result;

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Imports/includes
    let includes: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Include)
        .collect();
    assert_eq!(
        includes.len(),
        2,
        "expected 2 includes, got {}",
        includes.len()
    );

    // Preprocessor defines
    let defs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::PreprocessorDef)
        .collect();
    assert_eq!(defs.len(), 2, "expected 2 preprocessor defs");
    assert!(defs.iter().any(|n| n.name == "MAX_RETRIES"));
    assert!(defs.iter().any(|n| n.name == "DEFAULT_PORT"));

    // Enum (NS_ENUM)
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Enum && n.name == "LogLevel")
    );
    let variants: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert_eq!(variants.len(), 4, "expected 4 enum variants");

    // Protocol
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Interface && n.name == "Serializable")
    );

    // Classes
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Base")
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Connection")
    );

    // Docstring on Base class
    let base = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "Base")
        .unwrap();
    assert_eq!(
        base.docstring.as_deref(),
        Some("Base class with shared functionality.")
    );

    assert_eq!(kind_names(&result, NodeKind::Impl), ["Base", "Connection"]);
    assert_eq!(
        kind_names(&result, NodeKind::Property),
        ["name", "port", "connected"]
    );

    // Methods come from both the @interface declaration and the
    // @implementation definition of each class, which share its name.
    assert_eq!(
        contained_children(&result, "Base"),
        [
            "name",
            "initWithName",
            "description",
            "initWithName",
            "description",
            "validate",
        ]
    );
    assert_eq!(
        contained_children(&result, "Connection"),
        [
            "port",
            "connected",
            "initWithHost",
            "connect",
            "disconnect",
            "connectionWithHost",
            "initWithHost",
            "connect",
            "disconnect",
            "connectionWithHost",
            "toJson",
            "toJsonString",
        ]
    );

    // C function
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "logMessage")
    );
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "logMessage")
        .unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Top-level C function for logging.")
    );

    // Base extends NSObject and Connection extends Base; both conform to
    // protocols.
    assert_eq!(ref_names(&result, EdgeKind::Extends), ["NSObject", "Base"]);
    assert_eq!(
        ref_names(&result, EdgeKind::Implements),
        ["NSObject", "Serializable"]
    );
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "super.init",
            "name.copy",
            "NSString.stringWithFormat",
            "NSStringFromClass",
            "self.class",
            "NSAssert",
            "super.initWithName",
            "NSLog",
            "[self alloc].initWithHost",
            "self.alloc",
            "NSJSONSerialization.dataWithJSONObject",
            "self.toJson",
            "[NSString alloc].initWithData",
            "NSString.alloc",
            "NSLog",
        ]
    );
}

// -- Fortran ──────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-fortran")]
#[test]
fn test_fixture_fortran() {
    let source = read_fixture("sample.f90");
    let extractor = tracedecay_code_extraction::FortranExtractor;
    let result = extractor.extract_artifact("sample.f90", &source).result;
    assert!(
        result.errors.is_empty(),
        "Fortran errors: {:?}",
        result.errors
    );

    // File root node
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Module
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Module && n.name == "networking"),
        "networking module not found"
    );

    // Program as Function
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "main"),
        "program main not found"
    );

    // Constants
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "MAX_RETRIES"),
        "MAX_RETRIES constant not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Const && n.name == "DEFAULT_PORT"),
        "DEFAULT_PORT constant not found"
    );

    // Derived types (Struct)
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "Endpoint"),
        "Endpoint type not found"
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Struct && n.name == "PooledEndpoint"),
        "PooledEndpoint type not found"
    );

    assert_eq!(
        contained_children(&result, "Endpoint"),
        ["host", "port", "connected"]
    );
    assert_eq!(contained_children(&result, "PooledEndpoint"), ["pool_size"]);

    // Interface
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Interface && n.name == "Connectable"),
        "Connectable interface not found"
    );

    // Subroutines and functions
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert!(
        fns.iter().any(|f| f.name == "log_message"),
        "log_message not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "create_endpoint"),
        "create_endpoint not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "connect_endpoint"),
        "connect_endpoint not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "disconnect_endpoint"),
        "disconnect_endpoint not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "is_connected"),
        "is_connected not found"
    );

    // Docstrings
    let log_msg = fns.iter().find(|f| f.name == "log_message").unwrap();
    assert_eq!(
        log_msg.docstring.as_deref(),
        Some("Logs a message with the given level.")
    );

    // Use imports
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Use && n.name == "networking"),
        "use networking not found"
    );

    // Inheritance (PooledEndpoint extends Endpoint)
    assert!(
        result
            .unresolved_refs
            .iter()
            .any(|r| r.reference_kind == EdgeKind::Extends && r.reference_name == "Endpoint"),
        "expected Extends ref for PooledEndpoint -> Endpoint"
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "trim",
            "trim",
            "present",
            "log_message",
            "trim",
            "create_endpoint",
            "connect_endpoint",
            "disconnect_endpoint",
        ]
    );
    assert_eq!(
        contained_children(&result, "networking"),
        [
            "MAX_RETRIES",
            "DEFAULT_PORT",
            "Endpoint",
            "PooledEndpoint",
            "Connectable",
            "log_message",
            "create_endpoint",
            "connect_endpoint",
            "disconnect_endpoint",
            "is_connected",
        ]
    );
    assert_eq!(contained_children(&result, "main"), ["networking"]);
}

// -- COBOL ────────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-cobol")]
#[test]
fn test_fixture_cobol() {
    let source = read_fixture("sample.cob");
    let extractor = tracedecay_code_extraction::CobolExtractor;
    let result = extractor.extract_artifact("sample.cob", &source).result;
    assert!(
        result.errors.is_empty(),
        "COBOL errors: {:?}",
        result.errors
    );

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Module (PROGRAM-ID)
    let modules: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .collect();
    assert_eq!(modules.len(), 1, "expected 1 module, got {}", modules.len());
    assert_eq!(modules[0].name, "NETWORKING");

    // Paragraphs as functions (5: MAIN-PROGRAM, VALIDATE-CONFIG, LOG-MESSAGE, CONNECT-SERVER, DISCONNECT-SERVER)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 5, "expected 5 functions, got {}", fns.len());
    assert!(
        fns.iter().any(|f| f.name == "MAIN-PROGRAM"),
        "MAIN-PROGRAM not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "VALIDATE-CONFIG"),
        "VALIDATE-CONFIG not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "LOG-MESSAGE"),
        "LOG-MESSAGE not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "CONNECT-SERVER"),
        "CONNECT-SERVER not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "DISCONNECT-SERVER"),
        "DISCONNECT-SERVER not found"
    );

    // Data items: consts and fields
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert!(
        consts.iter().any(|c| c.name == "WS-MAX-RETRIES"),
        "WS-MAX-RETRIES const not found"
    );
    assert!(
        consts.iter().any(|c| c.name == "WS-DEFAULT-PORT"),
        "WS-DEFAULT-PORT const not found"
    );

    let fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field)
        .collect();
    assert!(
        fields.iter().any(|f| f.name == "WS-HOST"),
        "WS-HOST field not found"
    );

    // Docstrings
    let validate = fns.iter().find(|f| f.name == "VALIDATE-CONFIG").unwrap();
    assert_eq!(
        validate.docstring.as_deref(),
        Some("Validates the configuration.")
    );

    // PERFORM targets
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "VALIDATE-CONFIG",
            "CONNECT-SERVER",
            "DISCONNECT-SERVER",
            "LOG-MESSAGE",
            "LOG-MESSAGE",
            "LOG-MESSAGE",
            "LOG-MESSAGE",
        ]
    );
    assert_eq!(
        contained_children(&result, "NETWORKING"),
        [
            "WS-MAX-RETRIES",
            "WS-DEFAULT-PORT",
            "WS-HOST",
            "WS-PORT",
            "WS-CONNECTED",
            "WS-LOG-LEVEL",
            "WS-LOG-MESSAGE",
            "WS-RETRY-COUNT",
            "MAIN-PROGRAM",
            "VALIDATE-CONFIG",
            "LOG-MESSAGE",
            "CONNECT-SERVER",
            "DISCONNECT-SERVER",
        ]
    );
}

// ── MS BASIC 2.0 ────────────────────────────────────────────────────────────

#[cfg(feature = "lang-msbasic2")]
#[test]
fn test_fixture_msbasic2() {
    let source = read_fixture("sample.bas");
    let extractor = tracedecay_code_extraction::MsBasic2Extractor;
    let result = extractor.extract_artifact("sample.bas", &source).result;
    assert!(
        result.errors.is_empty(),
        "MS BASIC 2.0 errors: {:?}",
        result.errors
    );

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Constants from LET statements (MR, DP)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|c| c.name == "MR"), "MR const not found");
    assert!(consts.iter().any(|c| c.name == "DP"), "DP const not found");

    // Subroutines synthesized from REM...RETURN blocks (3)
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 3, "expected 3 functions, got {}", fns.len());
    assert!(
        fns.iter().any(|f| f.name == "LOG_A_MESSAGE"),
        "LOG_A_MESSAGE not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "CONNECT_TO_SERVER"),
        "CONNECT_TO_SERVER not found"
    );
    assert!(
        fns.iter().any(|f| f.name == "DISCONNECT"),
        "DISCONNECT not found"
    );

    // Docstrings
    let log_fn = fns.iter().find(|f| f.name == "LOG_A_MESSAGE").unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("LOG A MESSAGE\nPARAMS: L$=LEVEL, M$=MESSAGE")
    );

    // Complexity: CONNECT_TO_SERVER has a FOR loop
    let connect_fn = fns.iter().find(|f| f.name == "CONNECT_TO_SERVER").unwrap();
    assert!(
        connect_fn.loops >= 1,
        "CONNECT_TO_SERVER should have >= 1 loop"
    );

    // GOSUB targets are line numbers
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        ["200", "300", "400", "200", "200"]
    );
    assert_eq!(
        contained_children(&result, "sample.bas"),
        [
            "MR",
            "DP",
            "LOG_A_MESSAGE",
            "CONNECT_TO_SERVER",
            "DISCONNECT"
        ]
    );
}

// ── GW-BASIC ────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-gwbasic")]
#[test]
fn test_fixture_gwbasic() {
    let source = read_fixture("sample.gw");
    let extractor = tracedecay_code_extraction::GwBasicExtractor;
    let result = extractor.extract_artifact("sample.gw", &source).result;
    assert!(
        result.errors.is_empty(),
        "GW-BASIC errors: {:?}",
        result.errors
    );

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // Constants from LET statements (MR, DP)
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 2, "expected 2 consts, got {}", consts.len());
    assert!(consts.iter().any(|c| c.name == "MR"), "MR const not found");
    assert!(consts.iter().any(|c| c.name == "DP"), "DP const not found");

    // Functions: 1 DEF FN + 3 subroutines = 4
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(
        names(&fns),
        [
            "FNLOG",
            "VALIDATE_CONFIGURATION",
            "CONNECT_TO_SERVER",
            "DISCONNECT",
        ]
    );

    // Docstrings
    let validate_fn = fns
        .iter()
        .find(|f| f.name == "VALIDATE_CONFIGURATION")
        .unwrap();
    assert_eq!(
        validate_fn.docstring.as_deref(),
        Some("VALIDATE CONFIGURATION")
    );

    // Complexity: CONNECT_TO_SERVER has a WHILE loop
    let connect_fn = fns.iter().find(|f| f.name == "CONNECT_TO_SERVER").unwrap();
    assert!(
        connect_fn.loops >= 1,
        "CONNECT_TO_SERVER should have >= 1 loop"
    );

    // GOSUB targets are line numbers
    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        ["1000", "2000", "3000"]
    );
    assert_eq!(
        contained_children(&result, "sample.gw"),
        [
            "MR",
            "DP",
            "FNLOG",
            "VALIDATE_CONFIGURATION",
            "CONNECT_TO_SERVER",
            "DISCONNECT",
        ]
    );
}

// ── QBasic ──────────────────────────────────────────────────────────────────

#[cfg(feature = "lang-qbasic")]
#[test]
fn test_fixture_qbasic() {
    let source = read_fixture("sample.qb");
    let extractor = tracedecay_code_extraction::QBasicExtractor;
    let result = extractor.extract_artifact("sample.qb", &source).result;
    assert!(
        result.errors.is_empty(),
        "QBasic errors: {:?}",
        result.errors
    );

    // File root
    assert!(result.nodes.iter().any(|n| n.kind == NodeKind::File));

    // TYPE as Struct (Endpoint)
    let structs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Struct)
        .collect();
    assert_eq!(
        structs.len(),
        1,
        "expected 1 struct (Endpoint), got {}",
        structs.len()
    );
    assert_eq!(structs[0].name, "Endpoint");

    // Struct fields (host, port, connected)
    let struct_fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field && n.qualified_name.contains("Endpoint"))
        .collect();
    assert_eq!(names(&struct_fields), ["host", "port", "connected"]);
    assert_eq!(
        contained_children(&result, "Endpoint"),
        ["host", "port", "connected"]
    );

    // SUBs and FUNCTION as Function nodes
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(
        names(&fns),
        [
            "LogMessage",
            "ValidateConfig",
            "ConnectServer",
            "DisconnectServer",
            "IsConnected",
        ]
    );

    // Docstrings on functions
    let log_fn = fns.iter().find(|f| f.name == "LogMessage").unwrap();
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Logs a message with the given level.")
    );

    // Complexity: ValidateConfig has IF branches, ConnectServer has FOR loop
    let validate_fn = fns.iter().find(|f| f.name == "ValidateConfig").unwrap();
    assert!(
        validate_fn.branches >= 1,
        "ValidateConfig should have >= 1 branch"
    );
    let connect_fn = fns.iter().find(|f| f.name == "ConnectServer").unwrap();
    assert!(connect_fn.loops >= 1, "ConnectServer should have >= 1 loop");

    assert_eq!(
        kind_names(&result, NodeKind::Const),
        ["MAX_RETRIES", "DEFAULT_PORT"]
    );

    assert_eq!(
        ref_names(&result, EdgeKind::Calls),
        [
            "ValidateConfig",
            "ConnectServer",
            "DisconnectServer",
            "LogMessage",
            "LogMessage",
            "LogMessage",
            "LogMessage",
            "LogMessage",
            "LogMessage",
            "LogMessage",
        ]
    );
}
