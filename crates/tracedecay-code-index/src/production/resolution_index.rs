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
use std::mem::size_of;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::ManifestDigest;

use super::partitioned_codec::{
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1, inflate_index_segment,
    verify_index_segment,
};
use super::resident_bytes::btree_bytes;
use super::resolution_view::ResolutionMemoryChangeV1;
use super::resolution_view::{ResolutionFileFactsV1, ResolutionFileV1};
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};
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
        if pages == 0
            || pages > MAX_PAGES_V1
            || !pages.is_power_of_two()
            || self.references.len() != pages
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
pub(super) type DefinitionPageV1 =
    BTreeMap<String, BTreeMap<String, Vec<Arc<LineageSymbolRecordV1>>>>;
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
    fn of(file: &'a dyn ResolutionFileFactsV1) -> Self {
        let mut symbols = file
            .symbols()
            .iter()
            .map(|symbol| (symbol.simple_name.as_str(), symbol.as_ref()))
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| left.1.occurrence.cmp(&right.1.occurrence));
        Self {
            path: file.logical_path(),
            symbols,
            segments: file
                .unresolved_references()
                .iter()
                .flat_map(|reference| name_segments(&reference.reference_name))
                .collect(),
            aliases: file
                .imports()
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
    encode_segment_charged(value, 0, &mut |_| Ok(()))
}

fn encode_segment_charged<T: Serialize>(
    value: &T,
    base: usize,
    retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<(PartitionedIndexSegmentDescriptorV1, Vec<u8>), CodeIndexProductionErrorV1> {
    let (bytes, decoded_size_bytes) = super::partitioned_codec::encode_compressed_rows_charged(
        value,
        PAGE_COMPRESSION_LEVEL_V1,
        base,
        retain,
    )?;
    let descriptor = PartitionedIndexSegmentDescriptorV1 {
        segment_digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
            .map_err(|error| contract(error.to_string()))?,
        segment_size_bytes: u64::try_from(bytes.len())
            .map_err(|_| contract("resolution index length exceeds u64"))?,
        decoded_size_bytes,
    };
    Ok((descriptor, bytes))
}

type BorrowedDefinitionPageV1<'a> =
    BTreeMap<&'a str, BTreeMap<&'a str, Vec<&'a LineageSymbolRecordV1>>>;
type BorrowedReferencePageV1<'a> = BTreeMap<&'a str, BTreeSet<&'a str>>;

/// Seal the resolution index of a generation whose files are `files`.
#[tracing::instrument(
    name = "code_index.sealed_encode.resolution_index",
    level = "trace",
    skip_all
)]
pub(super) fn seal_resolution_index<T: ResolutionFileV1>(
    files: &[T],
    publish: impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedResolutionIndexDescriptorV1, CodeIndexProductionErrorV1> {
    seal_resolution_index_charged(files, &mut |_| Ok(()), publish)
}

/// Each canonical partition is planned, charged, encoded and released before
/// the next one. Only the final descriptors span partitions.
pub(super) fn seal_resolution_index_charged<T: ResolutionFileV1>(
    files: &[T],
    retain: &mut dyn FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    mut publish: impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedResolutionIndexDescriptorV1, CodeIndexProductionErrorV1> {
    let mut symbols = 0_usize;
    for file in files {
        symbols = symbols.saturating_add(file.binding()?.symbols().len());
    }
    let pages = planned_pages(symbols);
    let descriptor_bytes = pages
        .saturating_mul(2)
        .saturating_add(1)
        .saturating_mul(size_of::<PartitionedIndexSegmentDescriptorV1>() + "sha256:".len() + 64);
    let mut routing = descriptor_bytes.saturating_add(pages.saturating_mul(
        size_of::<Vec<(&str, &LineageSymbolRecordV1)>>() + size_of::<Vec<(&str, &str)>>(),
    ));
    retain(u64::try_from(routing).map_err(|_| contract("resolution index charge exceeds u64"))?)?;
    let mut definition_rows = (0..pages)
        .map(|_| Vec::new())
        .collect::<Vec<Vec<(&str, &LineageSymbolRecordV1)>>>();
    let mut reference_rows = (0..pages)
        .map(|_| Vec::new())
        .collect::<Vec<Vec<(&str, &str)>>>();
    let mut aliases = BTreeSet::new();
    for file in files {
        let file = file.binding()?;
        let file_workspace = file
            .symbols()
            .len()
            .saturating_mul(size_of::<&LineageSymbolRecordV1>());
        retain(
            u64::try_from(routing.saturating_add(file_workspace))
                .map_err(|_| contract("resolution index charge exceeds u64"))?,
        )?;
        let mut selected = file
            .symbols()
            .iter()
            .map(|symbol| symbol.as_ref())
            .collect::<Vec<_>>();
        selected.sort_unstable_by(|left, right| left.occurrence.cmp(&right.occurrence));
        for symbol in selected {
            let page = page_of(&symbol.simple_name, pages);
            super::resolution_view::reserve_resolution_vec(
                &mut definition_rows[page],
                1,
                &mut |bytes: ResolutionMemoryChangeV1| {
                    bytes.apply_with(&mut routing, |routing| {
                        retain(
                            u64::try_from(routing.saturating_add(file_workspace))
                                .map_err(|_| contract("resolution index charge exceeds u64"))?,
                        )
                    })
                },
            )?;
            definition_rows[page].push((file.logical_path(), symbol));
        }
        for segment in file
            .unresolved_references()
            .iter()
            .flat_map(|reference| name_segments(&reference.reference_name))
        {
            let page = page_of(segment, pages);
            super::resolution_view::reserve_resolution_vec(
                &mut reference_rows[page],
                1,
                &mut |bytes: ResolutionMemoryChangeV1| {
                    bytes.apply_with(&mut routing, |routing| {
                        retain(
                            u64::try_from(routing)
                                .map_err(|_| contract("resolution index charge exceeds u64"))?,
                        )
                    })
                },
            )?;
            reference_rows[page].push((segment, file.logical_path()));
        }
        for binding in file.imports() {
            if let (Some(local), Some(imported)) = (
                binding.local_name.as_deref(),
                binding.imported_name.as_deref(),
            ) && local != imported
                && !aliases.contains(&(local, imported))
            {
                let before = btree_bytes(aliases.len(), size_of::<(&str, &str)>());
                let after = btree_bytes(aliases.len().saturating_add(1), size_of::<(&str, &str)>());
                routing = routing.saturating_add(after.saturating_sub(before));
                retain(
                    u64::try_from(routing)
                        .map_err(|_| contract("resolution index charge exceeds u64"))?,
                )?;
                aliases.insert((local, imported));
            }
        }
    }
    let mut definitions = Vec::with_capacity(pages);
    let mut references = Vec::with_capacity(pages);
    for mut inputs in definition_rows {
        // Group the borrowed rows before allocating the canonical page. Stable
        // sorting preserves the existing per-file occurrence order.
        let sort_bytes = inputs
            .len()
            .saturating_mul(size_of::<(&str, &LineageSymbolRecordV1)>());
        retain(
            u64::try_from(routing.saturating_add(sort_bytes))
                .map_err(|_| contract("resolution index charge exceeds u64"))?,
        )?;
        inputs.sort_by(|left, right| {
            left.1
                .simple_name
                .cmp(&right.1.simple_name)
                .then_with(|| left.0.cmp(right.0))
        });
        let names = inputs.chunk_by(|left, right| left.1.simple_name == right.1.simple_name);
        let mut current = routing.saturating_add(btree_bytes(
            names.clone().count(),
            size_of::<(&str, BTreeMap<&str, Vec<&LineageSymbolRecordV1>>)>(),
        ));
        for name in names {
            current = current.saturating_add(btree_bytes(
                name.chunk_by(|left, right| left.0 == right.0).count(),
                size_of::<(&str, Vec<&LineageSymbolRecordV1>)>(),
            ));
            current = current.saturating_add(
                name.len()
                    .saturating_mul(size_of::<&LineageSymbolRecordV1>()),
            );
        }
        // All tree bodies and exact symbol vectors coexist under one admission.
        retain(
            u64::try_from(current).map_err(|_| contract("resolution index charge exceeds u64"))?,
        )?;
        let mut rows = BorrowedDefinitionPageV1::new();
        for name in inputs.chunk_by(|left, right| left.1.simple_name == right.1.simple_name) {
            let mut files = BTreeMap::new();
            for file in name.chunk_by(|left, right| left.0 == right.0) {
                let mut symbols = Vec::with_capacity(file.len());
                for (_, symbol) in file {
                    symbols.push(*symbol);
                }
                files.insert(file[0].0, symbols);
            }
            rows.insert(name[0].1.simple_name.as_str(), files);
        }
        let (descriptor, bytes) = encode_segment_charged(&rows, current, retain)?;
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &descriptor.segment_digest,
            bytes: &bytes,
        })?;
        definitions.push(descriptor);
        routing = routing.saturating_sub(
            inputs
                .capacity()
                .saturating_mul(size_of::<(&str, &LineageSymbolRecordV1)>()),
        );
    }
    for mut inputs in reference_rows {
        inputs.sort_unstable();
        inputs.dedup();
        let segments = inputs.chunk_by(|left, right| left.0 == right.0);
        let mut current = routing.saturating_add(btree_bytes(
            segments.clone().count(),
            size_of::<(&str, BTreeSet<&str>)>(),
        ));
        for segment in segments {
            current = current.saturating_add(btree_bytes(segment.len(), size_of::<&str>()));
        }
        retain(
            u64::try_from(current).map_err(|_| contract("resolution index charge exceeds u64"))?,
        )?;
        let mut rows = BorrowedReferencePageV1::new();
        for segment in inputs.chunk_by(|left, right| left.0 == right.0) {
            let mut paths = BTreeSet::new();
            for (_, path) in segment {
                paths.insert(*path);
            }
            rows.insert(segment[0].0, paths);
        }
        let (descriptor, bytes) = encode_segment_charged(&rows, current, retain)?;
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &descriptor.segment_digest,
            bytes: &bytes,
        })?;
        references.push(descriptor);
        routing =
            routing.saturating_sub(inputs.capacity().saturating_mul(size_of::<(&str, &str)>()));
    }
    let base = routing;
    let (import_aliases, bytes) = encode_segment_charged(&aliases, base, retain)?;
    publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
        digest: &import_aliases.segment_digest,
        bytes: &bytes,
    })?;
    retain(
        u64::try_from(descriptor_bytes)
            .map_err(|_| contract("resolution index charge exceeds u64"))?,
    )?;
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
        verify_index_segment(
            &bytes,
            &descriptor.segment_digest,
            descriptor.segment_size_bytes,
        )?;
        let canonical = inflate_index_segment(&bytes, descriptor.decoded_size_bytes)?;
        serde_json::from_slice(&canonical)
            .map_err(|error| contract(format!("sealed resolution index decoding failed: {error}")))
    }

    pub(super) fn definition_page_read_bytes(
        &self,
        page: usize,
    ) -> Result<u64, CodeIndexProductionErrorV1> {
        let descriptor = self
            .descriptor
            .definitions
            .get(page)
            .ok_or_else(|| contract("sealed resolution index page is out of range"))?;
        descriptor
            .segment_size_bytes
            .checked_add(descriptor.decoded_size_bytes)
            .ok_or_else(|| contract("resolution page read size exceeds u64"))
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
#[tracing::instrument(
    name = "code_index.sparse.reseal_resolution_index",
    level = "trace",
    skip_all
)]
pub(super) fn reseal_resolution_index(
    parent: &ResolutionIndexReaderV1<'_>,
    before: &[&FileGenerationArtifactsV1],
    after: &[&FileGenerationArtifactsV1],
    mut publish: impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedResolutionIndexDescriptorV1, CodeIndexProductionErrorV1> {
    let before = before
        .iter()
        .map(|file| FileRowsV1::of(*file))
        .collect::<Vec<_>>();
    let after = after
        .iter()
        .map(|file| FileRowsV1::of(*file))
        .collect::<Vec<_>>();
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
                        .push(Arc::new((*symbol).clone()));
                }
            }
        }
        let (segment, bytes) = encode_segment(&rows)?;
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &segment.segment_digest,
            bytes: &bytes,
        })?;
        descriptor.definitions[page] = segment;
    }
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
        let (segment, bytes) = encode_segment(&rows)?;
        publish(SealedGenerationSegmentPublicationV1::ResolutionIndex {
            digest: &segment.segment_digest,
            bytes: &bytes,
        })?;
        descriptor.references[page] = segment;
    }
    Ok(descriptor)
}
