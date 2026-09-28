//! Durable copy of one generation's interactive catalog.
//!
//! The catalog is derived from the verified projection. The file beside a
//! sealed generation artifact lets a later process reopen that derivation
//! instead of scanning the projection again. It is not a second authority:
//! the schema, projector revision, generation, projection, and recovered
//! digest must all match, and any miss falls through to a scan of the
//! snapshot.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::models::InteractiveCatalog;

const INTERACTIVE_CATALOG_CACHE_SCHEMA: u32 = 1;
const INTERACTIVE_CATALOG_CACHE_FILE: &str = "interactive-catalog.json";

pub(super) struct CatalogCacheBinding<'a> {
    pub(super) projector_revision: &'a str,
    pub(super) generation: &'a str,
    pub(super) namespace: &'a str,
    pub(super) projection: &'a str,
    pub(super) recovered_digest: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedInteractiveCatalogV1 {
    schema: u32,
    projector_revision: String,
    generation: String,
    namespace: String,
    projection: String,
    recovered_digest: String,
    catalog: InteractiveCatalog,
}

#[derive(Serialize)]
struct PersistedInteractiveCatalogRef<'a> {
    schema: u32,
    projector_revision: &'a str,
    generation: &'a str,
    namespace: &'a str,
    projection: &'a str,
    recovered_digest: &'a str,
    catalog: &'a InteractiveCatalog,
}

impl PersistedInteractiveCatalogV1 {
    fn binds(&self, expected: &CatalogCacheBinding<'_>) -> bool {
        self.schema == INTERACTIVE_CATALOG_CACHE_SCHEMA
            && self.projector_revision == expected.projector_revision
            && self.generation == expected.generation
            && self.namespace == expected.namespace
            && self.projection == expected.projection
            && self.recovered_digest == expected.recovered_digest
    }
}

pub(super) fn load_interactive_catalog(
    directory: &Path,
    expected: &CatalogCacheBinding<'_>,
) -> Option<InteractiveCatalog> {
    let bytes = match std::fs::read(directory.join(INTERACTIVE_CATALOG_CACHE_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        // Unreadable cache is a miss. The projection scan is the authority.
        Err(_) => return None,
    };
    let persisted: PersistedInteractiveCatalogV1 = serde_json::from_slice(&bytes).ok()?;
    persisted.binds(expected).then_some(persisted.catalog)
}

pub(super) fn store_interactive_catalog(
    directory: &Path,
    binding: &CatalogCacheBinding<'_>,
    catalog: &InteractiveCatalog,
) -> std::io::Result<()> {
    let persisted = PersistedInteractiveCatalogRef {
        schema: INTERACTIVE_CATALOG_CACHE_SCHEMA,
        projector_revision: binding.projector_revision,
        generation: binding.generation,
        namespace: binding.namespace,
        projection: binding.projection,
        recovered_digest: binding.recovered_digest,
        catalog,
    };
    let bytes = serde_json::to_vec(&persisted)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let target = directory.join(INTERACTIVE_CATALOG_CACHE_FILE);
    let temporary = directory.join(format!(".{INTERACTIVE_CATALOG_CACHE_FILE}.tmp"));
    let write = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &target)?;
        Ok(())
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write
}
