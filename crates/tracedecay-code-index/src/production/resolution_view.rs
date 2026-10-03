//! What cross-file resolution reads of a generation's files.
//!
//! Resolution decides a reference against every file of a generation, but it
//! reads most files only for their path and language, and their symbols only
//! by simple name. A file set therefore offers those facts without its
//! artifacts, and the symbols by name through [`SymbolsByNameV1`], so a
//! sealed generation can answer them from its snapshot and resolution index
//! while decoding only the files a lookup actually walks into.

use std::collections::HashMap;
use std::sync::Arc;

use super::FileGenerationArtifactsV1;
use crate::lineage::LineageSymbolRecordV1;

/// One file of the set resolution runs over.
pub(crate) trait ResolutionFileV1: AsRef<FileGenerationArtifactsV1> + Sync {
    fn logical_path(&self) -> &str {
        &self.as_ref().authority.logical_path
    }

    fn language(&self) -> &str {
        self.as_ref().extraction.language.as_str()
    }
}

impl ResolutionFileV1 for Arc<FileGenerationArtifactsV1> {}

impl ResolutionFileV1 for FileGenerationArtifactsV1 {}

/// A symbol a simple name finds: its file's index in the set and its record.
pub(crate) type NamedSymbolV1 = (usize, Arc<LineageSymbolRecordV1>);

/// Every symbol of a file set by simple name, each name's symbols in file
/// order and then in their file's symbol order.
pub(crate) trait SymbolsByNameV1: Sync {
    fn get(&self, name: &str) -> Option<&[NamedSymbolV1]>;
}

/// [`SymbolsByNameV1`] over files held in memory.
pub(crate) struct FileSymbolsByNameV1<'f> {
    by_name: HashMap<&'f str, Vec<NamedSymbolV1>>,
}

impl<'f> FileSymbolsByNameV1<'f> {
    pub(crate) fn new<T: ResolutionFileV1>(files: &'f [T]) -> Self {
        let mut by_name: HashMap<&str, Vec<NamedSymbolV1>> = HashMap::new();
        for (index, file) in files.iter().enumerate() {
            for symbol in &file.as_ref().artifacts.symbols {
                by_name
                    .entry(symbol.simple_name.as_str())
                    .or_default()
                    .push((index, Arc::clone(symbol)));
            }
        }
        Self { by_name }
    }
}

impl SymbolsByNameV1 for FileSymbolsByNameV1<'_> {
    fn get(&self, name: &str) -> Option<&[NamedSymbolV1]> {
        self.by_name.get(name).map(Vec::as_slice)
    }
}
