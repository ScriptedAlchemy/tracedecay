use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_tool_catalog::{CatalogValidationError, ExecutableBindingRegistryV1};

pub const WORK_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 0] = [];
pub const WORK_ATTEMPT_OPERATION_IDS_V1: [(&str, &str, &str); 0] = [];

pub fn work_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    ExecutableBindingRegistryV1::new(Vec::new())
}

pub fn work_executable_catalog_digest() -> Result<ManifestDigest, CatalogValidationError> {
    let registry = work_executable_binding_registry()?;
    canonical_sha256(&(
        "tracedecay.application.work-executable-catalog.v1",
        registry.iter().collect::<Vec<_>>(),
    ))
    .map_err(|_| CatalogValidationError::InvalidValue {
        field: "work executable catalog digest",
        reason: "canonical Work executable catalog could not be encoded",
    })
}

#[cfg(test)]
mod tests {
    use super::work_executable_binding_registry;

    #[test]
    fn work_registry_advertises_nothing_without_native_graph_authority() {
        let registry = work_executable_binding_registry().unwrap();
        assert_eq!(registry.iter().count(), 0);
    }
}
