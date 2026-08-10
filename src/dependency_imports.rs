use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyImportCandidate {
    pub module: String,
    pub symbol: String,
    pub import_file: String,
    pub line: u32,
}
