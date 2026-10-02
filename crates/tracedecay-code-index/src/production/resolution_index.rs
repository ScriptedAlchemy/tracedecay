//! Persisted cross-file resolution indexes of one sealed generation.
//!
//! Cross-file resolution finds a reference's targets by simple name, and an
//! edit can move only the sites whose references spell a name the edited
//! files declare, directly or through an import alias. A sealed generation
//! therefore keeps three indexes beside its file segments: every symbol by
//! simple name, every file by the name segments its references spell, and
//! every import alias. Names and segments hash into a fixed number of pages,
//! each one content-addressed segment, so a successor reads only the pages
//! its lookups name and reseals only the pages its edited files reach.
//!
//! A page's bytes are the raw DEFLATE stream of its canonical JSON: maps are
//! ordered, a name's symbols are grouped by logical path, and each file's
//! symbols are in occurrence order, so a cold seal and a reseal over a parent
//! write identical bytes for identical files.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::sync::Arc;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::ManifestDigest;

use super::partitioned_codec::{
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1, inflate_index_segment,
    verify_index_segment,
};
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1, collect_bounded_ordered};
use crate::lineage::LineageSymbolRecordV1;

/// Symbols a cold seal plans per definition page.
const SYMBOLS_PER_PAGE_V1: usize = 1024;
const MAX_PAGES_V1: usize = 4096;
const PAGE_COMPRESSION_LEVEL_V1: u32 = 6;

fn contract(message: impl Into<String>) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.into())
}

/// One index segment's content address.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedIndexSegmentDescriptorV1 {
    pub(super) segment_digest: ManifestDigest,
    pub(super) segment_size_bytes: u64,
    pub(super) decoded_size_bytes: u64,
}

/// The resolution index pages one generation addresses. Both page lists
/// have the same length, fixed by the cold seal the generation descends from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedResolutionIndexDescriptorV1 {
    definitions: Vec<PartitionedIndexSegmentDescriptorV1>,
    references: Vec<PartitionedIndexSegmentDescriptorV1>,
    import_aliases: PartitionedIndexSegmentDescriptorV1,
}

impl PartitionedResolutionIndexDescriptorV1 {
    pub(super) fn segments(&self) -> impl Iterator<Item = &PartitionedIndexSegmentDescriptorV1> {
        self.definitions
            .iter()
            .chain(&self.references)
            .chain(std::iter::once(&self.import_aliases))
    }

    pub(super) fn validate(&self) -> Result<(), CodeIndexProductionErrorV1> {
        let pages = self.definitions.len();
        if pages == 0 || pages > MAX_PAGES_V1 || !pages.is_power_of_two() || self.references.len() != pages
        {
            return Err(contract(
                "sealed resolution index pages are not canonically sized",
            ));
        }
        Ok(())
    }
}

/// The page a name or segment lands on: FNV-1a over its bytes. The hash is
/// part of the sealed format and must never change within a revision.
fn page_of(key: &str, pages: usize) -> usize {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // `pages` is a power of two no larger than 4096.
    (hash as usize) & (pages - 1)
}

/// Simple name to defining file to the file's symbols of that name.
type DefinitionPageV1 = BTreeMap<String, BTreeMap<String, Vec<LineageSymbolRecordV1>>>;
/// Name segment to the files with a reference spelling it.
type ReferencePageV1 = BTreeMap<String, BTreeSet<String>>;
/// Every `(local, imported)` pair an import renames.
type ImportAliasesV1 = BTreeSet<(String, String)>;

/// The identifiers a reference or symbol name joins with `::` and `.`.
pub(super) fn name_segments(name: &str) -> impl Iterator<Item = &str> {
    name.split("::")
        .flat_map(|part| part.split('.'))
        .filter(|segment| !segment.is_empty())
}

/// The index rows one file contributes.
struct FileRowsV1<'a> {
    path: &'a str,
    /// `(simple name, record)` in occurrence order.
    symbols: Vec<(&'a str, &'a LineageSymbolRecordV1)>,
    segments: BTreeSet<&'a str>,
    aliases: BTreeSet<(&'a str, &'a str)>,
}

impl<'a> FileRowsV1<'a> {
    fn of(file: &'a FileGenerationArtifactsV1) -> Self {
        let mut symbols = file
            .artifacts
            .symbols
            .iter()
            .map(|symbol| (symbol.simple_name.as_str(), symbol.as_ref()))
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| left.1.occurrence.cmp(&right.1.occurrence));
        Self {
            path: file.authority.logical_path.as_str(),
            symbols,
            segments: file
                .artifacts
                .unresolved_references
                .iter()
                .flat_map(|reference| name_segments(&reference.reference_name))
                .collect(),
            aliases: file
                .artifacts
                .imports
                .iter()
                .filter_map(|binding| {
                    let local = binding.local_name.as_deref()?;
                    let imported = binding.imported_name.as_deref()?;
                    (local != imported).then_some((local, imported))
                })
                .collect(),
        }
    }
}

/// Pages a cold seal of `symbols` symbols plans.
fn planned_pages(symbols: usize) -> usize {
    symbols
        .div_ceil(SYMBOLS_PER_PAGE_V1)
        .max(1)
        .next_power_of_two()
        .min(MAX_PAGES_V1)
}

fn encode_segment<T: Serialize>(
    value: &T,
) -> Result<(PartitionedIndexSegmentDescriptorV1, Vec<u8>), CodeIndexProductionErrorV1> {
    let failed = |error: &dyn std::fmt::Display| {
        contract(format!("sealed resolution index encoding failed: {error}"))
    };
    let canonical = serde_json::to_vec(value).map_err(|error| failed(&error))?;
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(PAGE_COMPRESSION_LEVEL_V1));
    encoder.write_all(&canonical).map_err(|error| failed(&error))?;
    let bytes = encoder.finish().map_err(|error| failed(&error))?;
    let length = |bytes: &[u8]| {
        u64::try_from(bytes.len()).map_err(|_| failed(&"sealed resolution index length exceeds u64"))
    };
    Ok((
        PartitionedIndexSegmentDescriptorV1 {
            segment_digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
                .map_err(|error| contract(error.to_string()))?,
            segment_size_bytes: length(&bytes)?,
            decoded_size_bytes: length(&canonical)?,
        },
        bytes,
    ))
}

/// Encode `pages` on the indexing pool and publish them in page order.
fn publish_pages<T: Serialize + Sync>(
    pages: &[T],
    publish: &mut impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<Vec<PartitionedIndexSegmentDescriptorV1>, CodeIndexProductionErrorV1> {
    let encoded = collect_bounded_ordered(pages, |page, _worker| encode_segment(page))?;
    let mut descriptors = Vec::with_capacity(encoded.len());
    for (descriptor, bytes) in encoded {
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &descriptor.segment_digest,
            bytes: &bytes,
        })?;
        descriptors.push(descriptor);
    }
    Ok(descriptors)
}

type BorrowedDefinitionPageV1<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Vec<&'a LineageSymbolRecordV1>>>;
type BorrowedReferencePageV1<'a> = BTreeMap<&'a str, BTreeSet<&'a str>>;

/// Seal the resolution index of a generation whose files are `files`.
#[hotpath::measure(label = "code_index.sealed_encode.resolution_index")]
pub(super) fn seal_resolution_index(
    files: &[Arc<FileGenerationArtifactsV1>],
    mut publish: impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedResolutionIndexDescriptorV1, CodeIndexProductionErrorV1> {
    let rows = files.iter().map(|file| FileRowsV1::of(file)).collect::<Vec<_>>();
    let pages = planned_pages(rows.iter().map(|rows| rows.symbols.len()).sum());
    let mut definitions = vec![BorrowedDefinitionPageV1::new(); pages];
    let mut references = vec![BorrowedReferencePageV1::new(); pages];
    let mut aliases = BTreeSet::new();
    for file in &rows {
        for (name, symbol) in &file.symbols {
            definitions[page_of(name, pages)]
                .entry(name)
                .or_default()
                .entry(file.path)
                .or_default()
                .push(symbol);
        }
        for segment in &file.segments {
            references[page_of(segment, pages)]
                .entry(segment)
                .or_default()
                .insert(file.path);
        }
        aliases.extend(file.aliases.iter().copied());
    }
    let definitions = publish_pages(&definitions, &mut publish)?;
    let references = publish_pages(&references, &mut publish)?;
    let (import_aliases, bytes) = encode_segment(&aliases)?;
    publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
        digest: &import_aliases.segment_digest,
        bytes: &bytes,
    })?;
    Ok(PartitionedResolutionIndexDescriptorV1 {
        definitions,
        references,
        import_aliases,
    })
}

/// Reads one sealed index segment's bytes into the buffer it is handed.
pub(super) type IndexSegmentReaderV1<'r> = dyn Fn(SealedGenerationSegmentReadV1<'_>, &mut Vec<u8>) -> Result<(), CodeIndexProductionErrorV1>
    + Sync
    + 'r;

/// A sealed generation's resolution index, read one page at a time.
pub(super) struct ResolutionIndexReaderV1<'r> {
    descriptor: &'r PartitionedResolutionIndexDescriptorV1,
    read: &'r IndexSegmentReaderV1<'r>,
}

impl<'r> ResolutionIndexReaderV1<'r> {
    pub(super) fn new(
        descriptor: &'r PartitionedResolutionIndexDescriptorV1,
        read: &'r IndexSegmentReaderV1<'r>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        descriptor.validate()?;
        Ok(Self { descriptor, read })
    }

    pub(super) fn pages(&self) -> usize {
        self.descriptor.definitions.len()
    }

    pub(super) fn page_of(&self, key: &str) -> usize {
        page_of(key, self.pages())
    }

    fn decode<T: for<'de> Deserialize<'de>>(
        &self,
        descriptor: &PartitionedIndexSegmentDescriptorV1,
    ) -> Result<T, CodeIndexProductionErrorV1> {
        let mut bytes = Vec::new();
        (self.read)(
            SealedGenerationSegmentReadV1::Whole {
                digest: &descriptor.segment_digest,
                size_bytes: descriptor.segment_size_bytes,
            },
            &mut bytes,
        )?;
        verify_index_segment(&bytes, &descriptor.segment_digest, descriptor.segment_size_bytes)?;
        let canonical = inflate_index_segment(&bytes, descriptor.decoded_size_bytes)?;
        hotpath::gauge!("code_index.sparse.index_bytes_decoded").inc(canonical.len());
        serde_json::from_slice(&canonical)
            .map_err(|error| contract(format!("sealed resolution index decoding failed: {error}")))
    }

    pub(super) fn definition_page(
        &self,
        page: usize,
    ) -> Result<DefinitionPageV1, CodeIndexProductionErrorV1> {
        let descriptor = self
            .descriptor
            .definitions
            .get(page)
            .ok_or_else(|| contract("sealed resolution index page is out of range"))?;
        self.decode(descriptor)
    }

    pub(super) fn reference_page(
        &self,
        page: usize,
    ) -> Result<ReferencePageV1, CodeIndexProductionErrorV1> {
        let descriptor = self
            .descriptor
            .references
            .get(page)
            .ok_or_else(|| contract("sealed resolution index page is out of range"))?;
        self.decode(descriptor)
    }

    pub(super) fn import_aliases(&self) -> Result<ImportAliasesV1, CodeIndexProductionErrorV1> {
        self.decode(&self.descriptor.import_aliases)
    }
}

/// Reseal `parent`'s index for a successor that replaces each file of
/// `before` with the file at the same path in `after`. Only the pages a
/// replaced file's names or segments land on are read and rewritten; every
/// other page keeps its parent descriptor. Import aliases must not change:
/// a successor whose imports move is resolved whole, never resealed here.
#[hotpath::measure(label = "code_index.sparse.reseal_resolution_index")]
pub(super) fn reseal_resolution_index(
    parent: &ResolutionIndexReaderV1<'_>,
    before: &[&FileGenerationArtifactsV1],
    after: &[&FileGenerationArtifactsV1],
    mut publish: impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedResolutionIndexDescriptorV1, CodeIndexProductionErrorV1> {
    let before = before.iter().map(|file| FileRowsV1::of(file)).collect::<Vec<_>>();
    let after = after.iter().map(|file| FileRowsV1::of(file)).collect::<Vec<_>>();
    fn aliases<'a>(rows: &[FileRowsV1<'a>]) -> BTreeSet<(&'a str, &'a str)> {
        rows.iter()
            .flat_map(|file| file.aliases.iter().copied())
            .collect()
    }
    if aliases(&before) != aliases(&after) {
        return Err(contract(
            "a resealed resolution index cannot change import aliases",
        ));
    }
    let pages = parent.pages();
    let replaced = before
        .iter()
        .chain(&after)
        .map(|file| file.path)
        .collect::<BTreeSet<_>>();
    let mut definition_pages = BTreeSet::new();
    let mut reference_pages = BTreeSet::new();
    for file in before.iter().chain(&after) {
        definition_pages.extend(file.symbols.iter().map(|(name, _)| page_of(name, pages)));
        reference_pages.extend(file.segments.iter().map(|segment| page_of(segment, pages)));
    }
    let mut descriptor = parent.descriptor.clone();
    let mut rewritten = BTreeMap::new();
    for page in definition_pages {
        let mut rows = parent.definition_page(page)?;
        rows.retain(|_, files| {
            files.retain(|path, _| !replaced.contains(path.as_str()));
            !files.is_empty()
        });
        for file in &after {
            for (name, symbol) in &file.symbols {
                if page_of(name, pages) == page {
                    rows.entry((*name).to_owned())
                        .or_default()
                        .entry(file.path.to_owned())
                        .or_default()
                        .push((*symbol).clone());
                }
            }
        }
        rewritten.insert(page, encode_segment(&rows)?);
    }
    for (page, (segment, bytes)) in rewritten {
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &segment.segment_digest,
            bytes: &bytes,
        })?;
        descriptor.definitions[page] = segment;
    }
    let mut rewritten = BTreeMap::new();
    for page in reference_pages {
        let mut rows = parent.reference_page(page)?;
        rows.retain(|_, files| {
            files.retain(|path| !replaced.contains(path.as_str()));
            !files.is_empty()
        });
        for file in &after {
            for segment in &file.segments {
                if page_of(segment, pages) == page {
                    rows.entry((*segment).to_owned())
                        .or_default()
                        .insert(file.path.to_owned());
                }
            }
        }
        rewritten.insert(page, encode_segment(&rows)?);
    }
    for (page, (segment, bytes)) in rewritten {
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &segment.segment_digest,
            bytes: &bytes,
        })?;
        descriptor.references[page] = segment;
    }
    Ok(descriptor)
}
