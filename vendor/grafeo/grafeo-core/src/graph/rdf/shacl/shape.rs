//! SHACL shape data model.
//!
//! Defines the core types for representing SHACL shapes, targets, property paths,
//! constraints, and severities per the W3C SHACL specification.

use crate::graph::rdf::Term;

// =========================================================================
// SHACL namespace constants
// =========================================================================

/// SHACL namespace IRI constants.
pub struct SH;

#[allow(missing_docs)]
impl SH {
    pub const NS: &str = "http://www.w3.org/ns/shacl#";

    // Shape types
    pub const NODE_SHAPE: &str = "http://www.w3.org/ns/shacl#NodeShape";
    pub const PROPERTY_SHAPE: &str = "http://www.w3.org/ns/shacl#PropertyShape";

    // Targets
    pub const TARGET_CLASS: &str = "http://www.w3.org/ns/shacl#targetClass";
    pub const TARGET_NODE: &str = "http://www.w3.org/ns/shacl#targetNode";
    pub const TARGET_SUBJECTS_OF: &str = "http://www.w3.org/ns/shacl#targetSubjectsOf";
    pub const TARGET_OBJECTS_OF: &str = "http://www.w3.org/ns/shacl#targetObjectsOf";

    // Property
    pub const PROPERTY: &str = "http://www.w3.org/ns/shacl#property";
    pub const PATH: &str = "http://www.w3.org/ns/shacl#path";

    // Path modifiers
    pub const INVERSE_PATH: &str = "http://www.w3.org/ns/shacl#inversePath";
    pub const ALTERNATIVE_PATH: &str = "http://www.w3.org/ns/shacl#alternativePath";
    pub const ZERO_OR_MORE_PATH: &str = "http://www.w3.org/ns/shacl#zeroOrMorePath";
    pub const ONE_OR_MORE_PATH: &str = "http://www.w3.org/ns/shacl#oneOrMorePath";
    pub const ZERO_OR_ONE_PATH: &str = "http://www.w3.org/ns/shacl#zeroOrOnePath";

    // Value type constraints
    pub const CLASS: &str = "http://www.w3.org/ns/shacl#class";
    pub const DATATYPE: &str = "http://www.w3.org/ns/shacl#datatype";
    pub const NODE_KIND: &str = "http://www.w3.org/ns/shacl#nodeKind";

    // Node kind values
    pub const BLANK_NODE: &str = "http://www.w3.org/ns/shacl#BlankNode";
    pub const IRI: &str = "http://www.w3.org/ns/shacl#IRI";
    pub const LITERAL: &str = "http://www.w3.org/ns/shacl#Literal";
    pub const BLANK_NODE_OR_IRI: &str = "http://www.w3.org/ns/shacl#BlankNodeOrIRI";
    pub const BLANK_NODE_OR_LITERAL: &str = "http://www.w3.org/ns/shacl#BlankNodeOrLiteral";
    pub const IRI_OR_LITERAL: &str = "http://www.w3.org/ns/shacl#IRIOrLiteral";

    // Cardinality constraints
    pub const MIN_COUNT: &str = "http://www.w3.org/ns/shacl#minCount";
    pub const MAX_COUNT: &str = "http://www.w3.org/ns/shacl#maxCount";

    // Value range constraints
    pub const MIN_EXCLUSIVE: &str = "http://www.w3.org/ns/shacl#minExclusive";
    pub const MAX_EXCLUSIVE: &str = "http://www.w3.org/ns/shacl#maxExclusive";
    pub const MIN_INCLUSIVE: &str = "http://www.w3.org/ns/shacl#minInclusive";
    pub const MAX_INCLUSIVE: &str = "http://www.w3.org/ns/shacl#maxInclusive";

    // String constraints
    pub const MIN_LENGTH: &str = "http://www.w3.org/ns/shacl#minLength";
    pub const MAX_LENGTH: &str = "http://www.w3.org/ns/shacl#maxLength";
    pub const PATTERN: &str = "http://www.w3.org/ns/shacl#pattern";
    pub const FLAGS: &str = "http://www.w3.org/ns/shacl#flags";
    pub const LANGUAGE_IN: &str = "http://www.w3.org/ns/shacl#languageIn";
    pub const UNIQUE_LANG: &str = "http://www.w3.org/ns/shacl#uniqueLang";

    // Property pair constraints
    pub const EQUALS: &str = "http://www.w3.org/ns/shacl#equals";
    pub const DISJOINT: &str = "http://www.w3.org/ns/shacl#disjoint";
    pub const LESS_THAN: &str = "http://www.w3.org/ns/shacl#lessThan";
    pub const LESS_THAN_OR_EQUALS: &str = "http://www.w3.org/ns/shacl#lessThanOrEquals";

    // Logical constraints
    pub const NOT: &str = "http://www.w3.org/ns/shacl#not";
    pub const AND: &str = "http://www.w3.org/ns/shacl#and";
    pub const OR: &str = "http://www.w3.org/ns/shacl#or";
    pub const XONE: &str = "http://www.w3.org/ns/shacl#xone";

    // Shape-based constraints
    pub const NODE: &str = "http://www.w3.org/ns/shacl#node";
    pub const QUALIFIED_VALUE_SHAPE: &str = "http://www.w3.org/ns/shacl#qualifiedValueShape";
    pub const QUALIFIED_MIN_COUNT: &str = "http://www.w3.org/ns/shacl#qualifiedMinCount";
    pub const QUALIFIED_MAX_COUNT: &str = "http://www.w3.org/ns/shacl#qualifiedMaxCount";
    pub const QUALIFIED_VALUE_SHAPES_DISJOINT: &str =
        "http://www.w3.org/ns/shacl#qualifiedValueShapesDisjoint";

    // Other constraints
    pub const CLOSED: &str = "http://www.w3.org/ns/shacl#closed";
    pub const IGNORED_PROPERTIES: &str = "http://www.w3.org/ns/shacl#ignoredProperties";
    pub const HAS_VALUE: &str = "http://www.w3.org/ns/shacl#hasValue";
    pub const IN: &str = "http://www.w3.org/ns/shacl#in";

    // SPARQL constraints
    pub const SPARQL: &str = "http://www.w3.org/ns/shacl#sparql";
    pub const SELECT: &str = "http://www.w3.org/ns/shacl#select";
    pub const PREFIXES: &str = "http://www.w3.org/ns/shacl#prefixes";
    pub const DECLARE: &str = "http://www.w3.org/ns/shacl#declare";
    pub const PREFIX_DECL: &str = "http://www.w3.org/ns/shacl#prefix";
    pub const NAMESPACE: &str = "http://www.w3.org/ns/shacl#namespace";
    pub const MESSAGE: &str = "http://www.w3.org/ns/shacl#message";

    // Shape metadata
    pub const DEACTIVATED: &str = "http://www.w3.org/ns/shacl#deactivated";
    pub const SEVERITY: &str = "http://www.w3.org/ns/shacl#severity";
    pub const SEVERITY_VIOLATION: &str = "http://www.w3.org/ns/shacl#Violation";
    pub const SEVERITY_WARNING: &str = "http://www.w3.org/ns/shacl#Warning";
    pub const SEVERITY_INFO: &str = "http://www.w3.org/ns/shacl#Info";
    pub const NAME: &str = "http://www.w3.org/ns/shacl#name";
    pub const DESCRIPTION: &str = "http://www.w3.org/ns/shacl#description";
    pub const ORDER: &str = "http://www.w3.org/ns/shacl#order";
    pub const GROUP: &str = "http://www.w3.org/ns/shacl#group";

    // Report vocabulary
    pub const VALIDATION_REPORT: &str = "http://www.w3.org/ns/shacl#ValidationReport";
    pub const CONFORMS: &str = "http://www.w3.org/ns/shacl#conforms";
    pub const RESULT: &str = "http://www.w3.org/ns/shacl#result";
    pub const VALIDATION_RESULT: &str = "http://www.w3.org/ns/shacl#ValidationResult";
    pub const FOCUS_NODE: &str = "http://www.w3.org/ns/shacl#focusNode";
    pub const RESULT_PATH: &str = "http://www.w3.org/ns/shacl#resultPath";
    pub const VALUE: &str = "http://www.w3.org/ns/shacl#value";
    pub const RESULT_SEVERITY: &str = "http://www.w3.org/ns/shacl#resultSeverity";
    pub const RESULT_MESSAGE: &str = "http://www.w3.org/ns/shacl#resultMessage";
    pub const SOURCE_CONSTRAINT_COMPONENT: &str =
        "http://www.w3.org/ns/shacl#sourceConstraintComponent";
    pub const SOURCE_SHAPE: &str = "http://www.w3.org/ns/shacl#sourceShape";
}

/// Common RDF namespace constants used by SHACL.
pub struct RDF;

#[allow(missing_docs)]
impl RDF {
    pub const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    pub const FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
    pub const REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
    pub const NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
}

// =========================================================================
// Shape types
// =========================================================================

/// A SHACL shape (either a node shape or a property shape).
#[derive(Debug, Clone)]
pub enum Shape {
    /// A node shape validates focus nodes directly.
    Node(NodeShape),
    /// A property shape validates values reachable via a property path.
    Property(PropertyShape),
}

impl Shape {
    /// Returns the shape's identifier.
    #[must_use]
    pub fn id(&self) -> &Term {
        match self {
            Shape::Node(s) => &s.id,
            Shape::Property(s) => &s.id,
        }
    }

    /// Returns whether the shape is deactivated.
    #[must_use]
    pub fn is_deactivated(&self) -> bool {
        match self {
            Shape::Node(s) => s.deactivated,
            Shape::Property(s) => s.deactivated,
        }
    }

    /// Returns the shape's targets.
    #[must_use]
    pub fn targets(&self) -> &[Target] {
        match self {
            Shape::Node(s) => &s.targets,
            Shape::Property(s) => &s.targets,
        }
    }

    /// Returns the shape's severity.
    #[must_use]
    pub fn severity(&self) -> Severity {
        match self {
            Shape::Node(s) => s.severity,
            Shape::Property(s) => s.severity,
        }
    }

    /// Returns the shape's constraints.
    #[must_use]
    pub fn constraints(&self) -> &[Constraint] {
        match self {
            Shape::Node(s) => &s.constraints,
            Shape::Property(s) => &s.constraints,
        }
    }

    /// Returns the shape's messages.
    #[must_use]
    pub fn messages(&self) -> &[String] {
        match self {
            Shape::Node(s) => &s.messages,
            Shape::Property(s) => &s.messages,
        }
    }
}

/// A SHACL node shape.
#[derive(Debug, Clone)]
pub struct NodeShape {
    /// Shape identifier (IRI or blank node).
    pub id: Term,
    /// Target declarations for this shape.
    pub targets: Vec<Target>,
    /// Property shapes nested under this node shape.
    pub property_shapes: Vec<PropertyShape>,
    /// Constraints that apply directly to focus nodes.
    pub constraints: Vec<Constraint>,
    /// Whether this shape is deactivated.
    pub deactivated: bool,
    /// Severity level for violations.
    pub severity: Severity,
    /// Human-readable messages for violations.
    pub messages: Vec<String>,
}

/// A SHACL property shape.
#[derive(Debug, Clone)]
pub struct PropertyShape {
    /// Shape identifier (IRI or blank node).
    pub id: Term,
    /// The property path this shape validates.
    pub path: PropertyPath,
    /// Target declarations (usually inherited from parent node shape).
    pub targets: Vec<Target>,
    /// Constraints that apply to value nodes.
    pub constraints: Vec<Constraint>,
    /// Whether this shape is deactivated.
    pub deactivated: bool,
    /// Severity level for violations.
    pub severity: Severity,
    /// Human-readable messages for violations.
    pub messages: Vec<String>,
    /// Display name for the property.
    pub name: Option<String>,
    /// Description of the property.
    pub description: Option<String>,
}

// =========================================================================
// Property paths
// =========================================================================

/// A SHACL property path.
#[derive(Debug, Clone)]
pub enum PropertyPath {
    /// A simple predicate path (an IRI).
    Predicate(Term),
    /// An inverse path: traverse the predicate in reverse.
    Inverse(Box<PropertyPath>),
    /// A sequence path: traverse each path in order.
    Sequence(Vec<PropertyPath>),
    /// An alternative path: union of results from each path.
    Alternative(Vec<PropertyPath>),
    /// Zero or more repetitions of the inner path.
    ZeroOrMore(Box<PropertyPath>),
    /// One or more repetitions of the inner path.
    OneOrMore(Box<PropertyPath>),
    /// Zero or one repetition of the inner path.
    ZeroOrOne(Box<PropertyPath>),
}

// =========================================================================
// Targets
// =========================================================================

/// A SHACL target declaration.
#[derive(Debug, Clone)]
pub enum Target {
    /// All instances of the given class (via `rdf:type`).
    Class(Term),
    /// A specific node.
    Node(Term),
    /// All subjects of triples with the given predicate.
    SubjectsOf(Term),
    /// All objects of triples with the given predicate.
    ObjectsOf(Term),
}

// =========================================================================
// Severity
// =========================================================================

/// SHACL validation severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// A constraint violation (default).
    Violation,
    /// A warning (less severe).
    Warning,
    /// Informational (least severe).
    Info,
}

impl Default for Severity {
    fn default() -> Self {
        Self::Violation
    }
}

// =========================================================================
// Node kind
// =========================================================================

/// SHACL node kind values for `sh:nodeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKindValue {
    /// `sh:BlankNode`: must be a blank node.
    BlankNode,
    /// `sh:IRI`: must be an IRI.
    Iri,
    /// `sh:Literal`: must be a literal.
    Literal,
    /// `sh:BlankNodeOrIRI`: must be a blank node or IRI.
    BlankNodeOrIri,
    /// `sh:BlankNodeOrLiteral`: must be a blank node or literal.
    BlankNodeOrLiteral,
    /// `sh:IRIOrLiteral`: must be an IRI or literal.
    IriOrLiteral,
}

// =========================================================================
// Constraints
// =========================================================================

/// A SHACL constraint component.
#[derive(Debug, Clone)]
pub enum Constraint {
    // -- Value type constraints --
    /// `sh:class`: value nodes must be instances of the given class.
    Class(Term),
    /// `sh:datatype`: value nodes must be literals with the given datatype.
    Datatype(Term),
    /// `sh:nodeKind`: value nodes must match the given node kind.
    NodeKind(NodeKindValue),

    // -- Cardinality constraints --
    /// `sh:minCount`: minimum number of value nodes.
    MinCount(usize),
    /// `sh:maxCount`: maximum number of value nodes.
    MaxCount(usize),

    // -- Value range constraints --
    /// `sh:minExclusive`: value must be greater than the given term.
    MinExclusive(Term),
    /// `sh:maxExclusive`: value must be less than the given term.
    MaxExclusive(Term),
    /// `sh:minInclusive`: value must be greater than or equal to the given term.
    MinInclusive(Term),
    /// `sh:maxInclusive`: value must be less than or equal to the given term.
    MaxInclusive(Term),

    // -- String constraints --
    /// `sh:minLength`: minimum string length.
    MinLength(usize),
    /// `sh:maxLength`: maximum string length.
    MaxLength(usize),
    /// `sh:pattern`: regex pattern with optional flags.
    Pattern {
        /// The regular expression pattern string.
        pattern: String,
        /// Optional regex flags (e.g., "i" for case-insensitive).
        flags: Option<String>,
    },
    /// `sh:languageIn`: allowed language tags.
    LanguageIn(Vec<String>),
    /// `sh:uniqueLang`: whether language tags must be unique among value nodes.
    UniqueLang,

    // -- Property pair constraints --
    /// `sh:equals`: value set must equal values reachable via the given path.
    Equals(Term),
    /// `sh:disjoint`: value set must be disjoint from values via the given path.
    Disjoint(Term),
    /// `sh:lessThan`: each value must be less than each value via the given path.
    LessThan(Term),
    /// `sh:lessThanOrEquals`: each value must be <= each value via the given path.
    LessThanOrEquals(Term),

    // -- Logical constraints --
    /// `sh:not`: focus node must NOT conform to the given shape.
    Not(Box<Shape>),
    /// `sh:and`: focus node must conform to ALL shapes in the list.
    And(Vec<Shape>),
    /// `sh:or`: focus node must conform to at least one shape.
    Or(Vec<Shape>),
    /// `sh:xone`: focus node must conform to exactly one shape.
    Xone(Vec<Shape>),

    // -- Shape-based constraints --
    /// `sh:node`: each value node must conform to the given shape.
    ShapeNode(Box<Shape>),
    /// `sh:qualifiedValueShape`: qualified cardinality on conforming values.
    QualifiedValueShape {
        /// The qualifying shape.
        shape: Box<Shape>,
        /// Minimum count of conforming values (`sh:qualifiedMinCount`).
        min_count: Option<usize>,
        /// Maximum count of conforming values (`sh:qualifiedMaxCount`).
        max_count: Option<usize>,
        /// Whether sibling qualified shapes are disjoint (`sh:qualifiedValueShapesDisjoint`).
        disjoint: bool,
    },

    // -- Other constraints --
    /// `sh:closed`: only declared properties are allowed.
    Closed {
        /// Properties to ignore when checking closedness (`sh:ignoredProperties`).
        ignored_properties: Vec<Term>,
    },
    /// `sh:hasValue`: value set must contain the given value.
    HasValue(Term),
    /// `sh:in`: each value node must be in the given list.
    In(Vec<Term>),

    // -- SPARQL constraints (evaluated by engine, not core) --
    /// `sh:sparql`: a SPARQL-based constraint.
    Sparql(SparqlConstraint),
}

/// A SHACL-SPARQL constraint definition.
#[derive(Debug, Clone)]
pub struct SparqlConstraint {
    /// The SPARQL SELECT query.
    pub select: String,
    /// Optional message template.
    pub message: Option<String>,
    /// Prefix declarations.
    pub prefixes: Vec<PrefixDeclaration>,
    /// Whether the constraint is deactivated.
    pub deactivated: bool,
}

/// A SPARQL prefix declaration from `sh:declare`.
#[derive(Debug, Clone)]
pub struct PrefixDeclaration {
    /// The prefix (e.g., "ex").
    pub prefix: String,
    /// The namespace IRI (e.g., `http://example.org/`).
    pub namespace: String,
}

// =========================================================================
// Error type
// =========================================================================

/// Errors that can occur during SHACL processing.
#[derive(Debug, Clone)]
pub enum ShaclError {
    /// A shape definition is malformed.
    InvalidShape(String),
    /// A property path is malformed.
    InvalidPath(String),
    /// A constraint could not be evaluated.
    ConstraintError(String),
    /// A SPARQL constraint failed to execute.
    SparqlError(String),
}

impl std::fmt::Display for ShaclError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShaclError::InvalidShape(msg) => write!(f, "Invalid SHACL shape: {msg}"),
            ShaclError::InvalidPath(msg) => write!(f, "Invalid SHACL path: {msg}"),
            ShaclError::ConstraintError(msg) => write!(f, "SHACL constraint error: {msg}"),
            ShaclError::SparqlError(msg) => write!(f, "SHACL SPARQL error: {msg}"),
        }
    }
}

impl std::error::Error for ShaclError {}
