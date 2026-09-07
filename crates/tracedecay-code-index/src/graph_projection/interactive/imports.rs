//! Import-oriented interactive reads over the verified generation catalog.

use std::sync::Arc;

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_domain::repository_path_matches_scope;
use tracedecay_graph_db::GraphCancellation;

use super::super::CodeGraphProjectionError;
use super::catalog::check_cancelled;
use super::{CodeGraphInteractiveReader, require_positive};
use crate::chunks::CodeIndexImportEvidenceV1;

impl CodeGraphInteractiveReader {
    /// Returns parser-backed bare-module type imports whose module specifier or
    /// imported name contains `query` under ASCII case folding.
    pub fn external_type_import_candidates(
        &self,
        query: &str,
        scope_prefix: Option<&str>,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeIndexImportEvidenceV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph external type import candidate limit")?;
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let query = query.to_ascii_lowercase();
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let mut candidates = Vec::new();

        for import in &catalog.imports {
            check_cancelled(cancellation.as_ref())?;
            if !repository_path_matches_scope(&import.logical_path, scope_prefix)
                || import.namespace != ImportNamespaceV1::Type
                || import.module_kind != ImportModuleKindV1::BareModule
                || !matches_query(import, &query)
                || candidates.contains(import)
            {
                continue;
            }
            candidates.push(import.clone());
            if candidates.len() == limit {
                check_cancelled(cancellation.as_ref())?;
                break;
            }
        }
        check_cancelled(cancellation.as_ref())?;
        Ok(candidates)
    }
}

fn matches_query(import: &CodeIndexImportEvidenceV1, query: &str) -> bool {
    import.module_specifier.to_ascii_lowercase().contains(query)
        || import
            .imported_name
            .as_deref()
            .is_some_and(|name| name.to_ascii_lowercase().contains(query))
}
