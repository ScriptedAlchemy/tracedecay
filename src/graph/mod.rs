/// Graph traversal algorithms for the code graph.
pub mod traversal;

/// Query operations for analyzing the code graph.
pub mod queries;

/// Tarjan's strongly-connected-components algorithm.
pub mod scc;

/// Structural health analysis algorithms.
pub mod health;

/// Git integration helpers for churn analysis.
pub mod git;

/// AST-level functional-duplicate scanning over the code graph.
pub mod redundancy_scan;

pub use queries::GraphQueryManager;
pub use traversal::GraphTraverser;
