//! Independently readable code-graph output pages.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::{CodeGenerationId, FileOccurrenceId, ManifestDigest};

use super::{
    CodeIndexProductionErrorV1, PersistedCodeGraphPageV1, SealedGenerationFileWindowsV1,
    SealedGenerationSegmentReaderV1,
};

const CODE_GRAPH_PAGE_STORE_MAGIC_V1: &[u8; 8] = b"TDGRAPH1";
const CODE_GRAPH_PAGE_STORE_REVISION_V1: u32 = 1;

fn store_error(context: &str, error: impl std::fmt::Display) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(format!("code graph page store {context}: {error}"))
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeGraphPageBuildFootprintV1 {
    pub(crate) decode_bytes: u64,
    pub(crate) entity_spill_buffered: u64,
    pub(crate) entity_spill_resident: u64,
    pub(crate) relation_spill_buffered: u64,
    pub(crate) relation_spill_resident: u64,
    pub(crate) identity_bytes: u64,
    pub(crate) entity_count: u64,
    pub(crate) relation_count: u64,
    pub(crate) max_entity_spill_buffered: u64,
    pub(crate) max_entity_spill_resident: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodeGraphPageDescriptorV1 {
    pub(crate) file_key: u32,
    pub(crate) file_occurrence_id: FileOccurrenceId,
    pub(crate) logical_path: String,
    pub(crate) page_digest: ManifestDigest,
    pub(crate) size_bytes: u64,
    pub(crate) build_footprint: CodeGraphPageBuildFootprintV1,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CodeGraphPageStoreHeaderV1 {
    revision: u32,
    generation: CodeGenerationId,
    projector_revision: String,
    pages: Vec<CodeGraphPageDescriptorV1>,
}

pub(crate) trait CodeGraphPageStoreV1 {
    fn generation(&self) -> &CodeGenerationId;
    fn pages(&self) -> &[CodeGraphPageDescriptorV1];
    fn read_page(
        &mut self,
        descriptor: &CodeGraphPageDescriptorV1,
    ) -> Result<PersistedCodeGraphPageV1, CodeIndexProductionErrorV1>;
}

fn decode_verified_page(
    descriptor: &CodeGraphPageDescriptorV1,
    encoded: &[u8],
) -> Result<PersistedCodeGraphPageV1, CodeIndexProductionErrorV1> {
    let size_bytes =
        u64::try_from(encoded.len()).map_err(|_| store_error("page length", "exceeds u64"))?;
    if size_bytes != descriptor.size_bytes {
        return Err(store_error(
            "page read",
            "byte size does not match its descriptor",
        ));
    }
    let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(encoded))
        .map_err(|error| store_error("page digest", error))?;
    if digest != descriptor.page_digest {
        return Err(store_error(
            "page read",
            "digest does not match its descriptor",
        ));
    }
    let page: PersistedCodeGraphPageV1 =
        serde_json::from_slice(encoded).map_err(|error| store_error("page decode", error))?;
    if page.file.file_occurrence_id != descriptor.file_occurrence_id
        || page.file.logical_path != descriptor.logical_path
    {
        return Err(store_error(
            "page read",
            "identity does not match its descriptor",
        ));
    }
    Ok(page)
}

pub(crate) struct SealedCodeGraphPageStoreV1<'source, 'reader> {
    source: &'source SealedGenerationFileWindowsV1,
    read_segment: &'reader mut SealedGenerationSegmentReaderV1<'reader>,
    pages: Vec<CodeGraphPageDescriptorV1>,
}

impl<'source, 'reader> SealedCodeGraphPageStoreV1<'source, 'reader> {
    pub(crate) fn new(
        source: &'source SealedGenerationFileWindowsV1,
        read_segment: &'reader mut SealedGenerationSegmentReaderV1<'reader>,
    ) -> Self {
        let pages = source
            .code_graph_pages()
            .iter()
            .map(|page| CodeGraphPageDescriptorV1 {
                file_key: page.file_key,
                file_occurrence_id: page.file_occurrence_id.clone(),
                logical_path: page.logical_path.clone(),
                page_digest: page.page_digest.clone(),
                size_bytes: page.size_bytes,
                build_footprint: page.build_footprint.clone(),
            })
            .collect();
        Self {
            source,
            read_segment,
            pages,
        }
    }
}

impl CodeGraphPageStoreV1 for SealedCodeGraphPageStoreV1<'_, '_> {
    fn generation(&self) -> &CodeGenerationId {
        self.source.generation_id()
    }

    fn pages(&self) -> &[CodeGraphPageDescriptorV1] {
        &self.pages
    }

    fn read_page(
        &mut self,
        descriptor: &CodeGraphPageDescriptorV1,
    ) -> Result<PersistedCodeGraphPageV1, CodeIndexProductionErrorV1> {
        let sealed = self
            .source
            .code_graph_pages()
            .get(descriptor.file_key as usize)
            .filter(|sealed| {
                sealed.file_occurrence_id == descriptor.file_occurrence_id
                    && sealed.logical_path == descriptor.logical_path
                    && sealed.page_digest == descriptor.page_digest
                    && sealed.size_bytes == descriptor.size_bytes
                    && sealed.build_footprint == descriptor.build_footprint
            })
            .ok_or_else(|| store_error("sealed page", "descriptor is outside its generation"))?
            .clone();
        self.source.read_code_graph_page(&sealed, self.read_segment)
    }
}

pub(crate) struct CodeGraphPageStoreWriterV1 {
    file: File,
    pages: Vec<CodeGraphPageDescriptorV1>,
    next_page: usize,
}

impl CodeGraphPageStoreWriterV1 {
    pub(crate) fn create(
        path: &Path,
        generation: &CodeGenerationId,
        projector_revision: &str,
        pages: &[CodeGraphPageDescriptorV1],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let header = serde_json::to_vec(&CodeGraphPageStoreHeaderV1 {
            revision: CODE_GRAPH_PAGE_STORE_REVISION_V1,
            generation: generation.clone(),
            projector_revision: projector_revision.to_owned(),
            pages: pages.to_vec(),
        })
        .map_err(|error| store_error("header encode", error))?;
        let header_size =
            u64::try_from(header.len()).map_err(|_| store_error("header length", "exceeds u64"))?;
        let mut file = File::create(path).map_err(|error| store_error("create", error))?;
        file.write_all(CODE_GRAPH_PAGE_STORE_MAGIC_V1)
            .and_then(|()| file.write_all(&header_size.to_le_bytes()))
            .and_then(|()| file.write_all(&header))
            .map_err(|error| store_error("header write", error))?;
        Ok(Self {
            file,
            pages: pages.to_vec(),
            next_page: 0,
        })
    }

    pub(crate) fn write_page(
        &mut self,
        descriptor: &CodeGraphPageDescriptorV1,
        page: &PersistedCodeGraphPageV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let expected = self.pages.get(self.next_page).ok_or_else(|| {
            store_error(
                "page write",
                "received more pages than its header describes",
            )
        })?;
        if expected != descriptor {
            return Err(store_error(
                "page write",
                "descriptor order does not match its header",
            ));
        }
        let encoded =
            serde_json::to_vec(page).map_err(|error| store_error("page encode", error))?;
        decode_verified_page(expected, &encoded)?;
        self.file
            .write_all(&encoded)
            .map_err(|error| store_error("page write", error))?;
        self.next_page += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<(), CodeIndexProductionErrorV1> {
        if self.next_page != self.pages.len() {
            return Err(store_error(
                "finish",
                "not every page in the header was written",
            ));
        }
        self.file
            .flush()
            .map_err(|error| store_error("finish", error))
    }
}

pub(crate) struct FileCodeGraphPageStoreV1 {
    generation: CodeGenerationId,
    pages: Vec<CodeGraphPageDescriptorV1>,
    offsets: Vec<u64>,
    file: File,
}

impl FileCodeGraphPageStoreV1 {
    pub(crate) fn open(
        path: &Path,
        projector_revision: &str,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let mut file = File::open(path).map_err(|error| store_error("open", error))?;
        let mut prefix = [0_u8; 16];
        file.read_exact(&mut prefix)
            .map_err(|error| store_error("header read", error))?;
        if &prefix[..8] != CODE_GRAPH_PAGE_STORE_MAGIC_V1 {
            return Err(store_error("header", "magic is invalid"));
        }
        let file_size = file
            .metadata()
            .map_err(|error| store_error("metadata", error))?
            .len();
        let header_size_u64 = u64::from_le_bytes(
            prefix[8..]
                .try_into()
                .map_err(|_| store_error("header", "length is invalid"))?,
        );
        if header_size_u64 > file_size.saturating_sub(16) {
            return Err(store_error("header", "length exceeds the attachment"));
        }
        let header_size = usize::try_from(header_size_u64)
            .map_err(|_| store_error("header length", "exceeds addressable memory"))?;
        let mut encoded = vec![0_u8; header_size];
        file.read_exact(&mut encoded)
            .map_err(|error| store_error("header read", error))?;
        let header: CodeGraphPageStoreHeaderV1 = serde_json::from_slice(&encoded)
            .map_err(|error| store_error("header decode", error))?;
        if header.revision != CODE_GRAPH_PAGE_STORE_REVISION_V1
            || header.projector_revision != projector_revision
        {
            return Ok(None);
        }
        let data_offset = 16_u64
            .checked_add(
                u64::try_from(header_size)
                    .map_err(|_| store_error("header length", "exceeds u64"))?,
            )
            .ok_or_else(|| store_error("header length", "offset overflows u64"))?;
        let mut next = data_offset;
        let mut offsets = Vec::with_capacity(header.pages.len());
        for (expected_key, page) in header.pages.iter().enumerate() {
            if page.file_key as usize != expected_key || page.size_bytes == 0 {
                return Err(store_error(
                    "header",
                    "pages are not canonically keyed and bounded",
                ));
            }
            offsets.push(next);
            next = next
                .checked_add(page.size_bytes)
                .ok_or_else(|| store_error("header", "page offsets overflow u64"))?;
        }
        if file_size != next {
            return Err(store_error(
                "header",
                "page sizes do not cover the attachment",
            ));
        }
        Ok(Some(Self {
            generation: header.generation,
            pages: header.pages,
            offsets,
            file,
        }))
    }
}

impl CodeGraphPageStoreV1 for FileCodeGraphPageStoreV1 {
    fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    fn pages(&self) -> &[CodeGraphPageDescriptorV1] {
        &self.pages
    }

    fn read_page(
        &mut self,
        descriptor: &CodeGraphPageDescriptorV1,
    ) -> Result<PersistedCodeGraphPageV1, CodeIndexProductionErrorV1> {
        let expected = self
            .pages
            .get(descriptor.file_key as usize)
            .filter(|expected| *expected == descriptor)
            .ok_or_else(|| store_error("page read", "descriptor is outside its store"))?;
        let offset = self.offsets[descriptor.file_key as usize];
        let size = usize::try_from(expected.size_bytes)
            .map_err(|_| store_error("page length", "exceeds addressable memory"))?;
        let mut encoded = vec![0_u8; size];
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(&mut encoded))
            .map_err(|error| store_error("page read", error))?;
        decode_verified_page(expected, &encoded)
    }
}
