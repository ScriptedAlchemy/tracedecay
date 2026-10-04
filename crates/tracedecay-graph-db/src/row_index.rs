//! A sealed cold generation's row digests, keyed for point lookup.
//!
//! A layered refresh derives its digest from its base's row sum: it removes
//! the digest of every base row it shadows or hides. Reading those rows from
//! the base container meant loading the whole base graph, which grafeo holds
//! resident, so the index records each row's frame digest beside the
//! container instead, sorted by a key of its identity, and a refresh reads
//! only the records of the rows it touches.
//!
//! ```text
//! magic "TDROWIX1" | entities u64 | relations u64
//! entity records   key[16] lanes[32] ordinal u32 degree u32    (by key)
//! relation records key[16] lanes[32] from u32 to u32           (by key)
//! ```
//!
//! An entity's ordinal is its position in identity order, which relation
//! endpoints name, and its degree counts the relations incident to it, so a
//! refresh can prove it removes an entity only with every relation it
//! anchors.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::GraphDbError;
use crate::generation::{GraphRowDigestSum, RowLanes};

/// Name of a generation's row index beside its container.
pub(crate) const ROW_INDEX_FILE: &str = "rows.index";
/// Identity-order offsets of the canonical entity rows a layerable base
/// retains for exact endpoint point reads.
pub(crate) const ENTITY_ROW_OFFSETS_FILE: &str = "entity-rows.offsets";

const MAGIC: &[u8; 8] = b"TDROWIX1";
const HEADER_BYTES: u64 = 24;
const RECORD_BYTES: u64 = 56;
const ENTITY_ROW_OFFSETS_MAGIC: &[u8; 8] = b"TDENTRW1";
const ENTITY_ROW_OFFSETS_HEADER_BYTES: u64 = 16;
const ENTITY_ROW_OFFSET_BYTES: u64 = 16;

type RowKey = [u8; 16];

fn row_key(kind: &str, identity: &str) -> RowKey {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.graph-row-key.v1\0");
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0_u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

fn index_io(context: &str, error: std::io::Error) -> GraphDbError {
    GraphDbError::unavailable(format!("graph row index {context} failed: {error}"))
}

fn corrupt(message: &str) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("graph row index {message}"),
    }
}

/// A base entity as the index records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IndexedEntity {
    pub(crate) lanes: RowLanes,
    pub(crate) ordinal: u32,
    pub(crate) degree: u32,
}

/// A base relation as the index records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IndexedRelation {
    pub(crate) lanes: RowLanes,
    pub(crate) from: u32,
    pub(crate) to: u32,
}

/// Collects a generation's rows as its spill merges them, in identity
/// order, and writes the index once every row is known.
pub(crate) struct RowIndexBuilder {
    entities: Vec<(RowKey, IndexedEntity)>,
    relations: Vec<(RowKey, IndexedRelation)>,
}

impl RowIndexBuilder {
    pub(crate) fn new() -> Self {
        Self {
            entities: Vec::new(),
            relations: Vec::new(),
        }
    }

    /// Records the next entity in identity order.
    pub(crate) fn entity(&mut self, identity: &str, lanes: RowLanes) -> Result<(), GraphDbError> {
        let ordinal = u32::try_from(self.entities.len())
            .map_err(|_| corrupt("holds more entities than u32 ordinals"))?;
        self.entities.push((
            row_key("entity", identity),
            IndexedEntity {
                lanes,
                ordinal,
                degree: 0,
            },
        ));
        Ok(())
    }

    /// Records a relation between the entities at identity-order positions
    /// `from` and `to`.
    pub(crate) fn relation(
        &mut self,
        identity: &str,
        lanes: RowLanes,
        from: usize,
        to: usize,
    ) -> Result<(), GraphDbError> {
        let ordinal = |position: usize| {
            u32::try_from(position).map_err(|_| corrupt("names an entity past u32 ordinals"))
        };
        let (from, to) = (ordinal(from)?, ordinal(to)?);
        for endpoint in if from == to {
            vec![from]
        } else {
            vec![from, to]
        } {
            let entity = self
                .entities
                .get_mut(endpoint as usize)
                .ok_or_else(|| corrupt("names an endpoint it never recorded"))?;
            entity.1.degree = entity
                .1
                .degree
                .checked_add(1)
                .ok_or_else(|| corrupt("counts a degree past u32"))?;
        }
        self.relations.push((
            row_key("relation", identity),
            IndexedRelation { lanes, from, to },
        ));
        Ok(())
    }

    pub(crate) fn write(mut self, path: &Path) -> Result<(), GraphDbError> {
        self.entities.sort_unstable_by_key(|(key, _)| *key);
        self.relations.sort_unstable_by_key(|(key, _)| *key);
        if self.entities.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || self.relations.windows(2).any(|pair| pair[0].0 == pair[1].0)
        {
            return Err(corrupt("keys two rows alike"));
        }
        let file = File::create(path).map_err(|error| index_io("create", error))?;
        let mut writer = BufWriter::new(file);
        let mut write = |bytes: &[u8]| {
            writer
                .write_all(bytes)
                .map_err(|error| index_io("write", error))
        };
        write(MAGIC)?;
        write(&(self.entities.len() as u64).to_be_bytes())?;
        write(&(self.relations.len() as u64).to_be_bytes())?;
        for (key, entity) in &self.entities {
            write(key)?;
            write(&lanes_bytes(entity.lanes))?;
            write(&entity.ordinal.to_be_bytes())?;
            write(&entity.degree.to_be_bytes())?;
        }
        for (key, relation) in &self.relations {
            write(key)?;
            write(&lanes_bytes(relation.lanes))?;
            write(&relation.from.to_be_bytes())?;
            write(&relation.to.to_be_bytes())?;
        }
        writer
            .into_inner()
            .map_err(|error| index_io("flush", error.into_error()))?
            .sync_all()
            .map_err(|error| index_io("sync", error))
    }
}

fn lanes_bytes(lanes: RowLanes) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    for (slot, lane) in bytes.chunks_exact_mut(8).zip(lanes) {
        slot.copy_from_slice(&lane.to_be_bytes());
    }
    bytes
}

fn record_parts(record: &[u8; RECORD_BYTES as usize]) -> (RowKey, RowLanes, u32, u32) {
    let mut key = [0_u8; 16];
    key.copy_from_slice(&record[..16]);
    let mut lanes = [0_u64; 4];
    for (lane, word) in lanes.iter_mut().zip(record[16..48].chunks_exact(8)) {
        let mut buffer = [0_u8; 8];
        buffer.copy_from_slice(word);
        *lane = u64::from_be_bytes(buffer);
    }
    let word = |range: std::ops::Range<usize>| {
        let mut buffer = [0_u8; 4];
        buffer.copy_from_slice(&record[range]);
        u32::from_be_bytes(buffer)
    };
    (key, lanes, word(48..52), word(52..56))
}

/// An open row index: point lookups read only the records a binary search
/// visits.
pub(crate) struct RowIndex {
    path: PathBuf,
    file: Mutex<File>,
    entities: u64,
    relations: u64,
}

impl std::fmt::Debug for RowIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RowIndex")
            .field("path", &self.path)
            .field("entities", &self.entities)
            .field("relations", &self.relations)
            .finish()
    }
}

impl RowIndex {
    pub(crate) fn open(path: &Path) -> Result<Self, GraphDbError> {
        let mut file = File::open(path).map_err(|error| index_io("open", error))?;
        let mut header = [0_u8; HEADER_BYTES as usize];
        file.read_exact(&mut header)
            .map_err(|error| index_io("header read", error))?;
        if &header[..8] != MAGIC {
            return Err(corrupt("has a foreign header"));
        }
        let count = |range: std::ops::Range<usize>| {
            let mut buffer = [0_u8; 8];
            buffer.copy_from_slice(&header[range]);
            u64::from_be_bytes(buffer)
        };
        let (entities, relations) = (count(8..16), count(16..24));
        let expected = entities
            .checked_add(relations)
            .and_then(|records| records.checked_mul(RECORD_BYTES))
            .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
            .ok_or_else(|| corrupt("counts more records than a file holds"))?;
        let length = file
            .metadata()
            .map_err(|error| index_io("metadata", error))?
            .len();
        if length != expected {
            return Err(corrupt("length does not match its record counts"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            entities,
            relations,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// `(entities, relations)` the index records.
    pub(crate) fn row_counts(&self) -> (u64, u64) {
        (self.entities, self.relations)
    }

    fn record(&self, position: u64) -> Result<[u8; RECORD_BYTES as usize], GraphDbError> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| GraphDbError::unavailable("graph row index lock is poisoned"))?;
        file.seek(SeekFrom::Start(HEADER_BYTES + position * RECORD_BYTES))
            .map_err(|error| index_io("seek", error))?;
        let mut record = [0_u8; RECORD_BYTES as usize];
        file.read_exact(&mut record)
            .map_err(|error| index_io("record read", error))?;
        Ok(record)
    }

    fn find(
        &self,
        first: u64,
        count: u64,
        key: RowKey,
    ) -> Result<Option<(RowLanes, u32, u32)>, GraphDbError> {
        Ok(self
            .position(first, count, key)?
            .map(|(_, lanes, first_word, second_word)| (lanes, first_word, second_word)))
    }

    fn position(
        &self,
        first: u64,
        count: u64,
        key: RowKey,
    ) -> Result<Option<(u64, RowLanes, u32, u32)>, GraphDbError> {
        let (mut low, mut high) = (0_u64, count);
        while low < high {
            let middle = low + (high - low) / 2;
            let (found, lanes, first_word, second_word) =
                record_parts(&self.record(first + middle)?);
            match found.cmp(&key) {
                std::cmp::Ordering::Equal => {
                    return Ok(Some((first + middle, lanes, first_word, second_word)));
                }
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
            }
        }
        Ok(None)
    }

    pub(crate) fn entity(&self, identity: &str) -> Result<Option<IndexedEntity>, GraphDbError> {
        Ok(self
            .find(0, self.entities, row_key("entity", identity))?
            .map(|(lanes, ordinal, degree)| IndexedEntity {
                lanes,
                ordinal,
                degree,
            }))
    }

    pub(crate) fn relation(&self, identity: &str) -> Result<Option<IndexedRelation>, GraphDbError> {
        Ok(self
            .find(self.entities, self.relations, row_key("relation", identity))?
            .map(|(lanes, from, to)| IndexedRelation { lanes, from, to }))
    }

    /// Writes this index to `path` with every relation's lanes replaced by
    /// the lanes `relane` emits for it, entity records and endpoint ordinals
    /// unchanged: the index of the same rows read under another projection.
    /// `relane` streams each recorded relation exactly once, in strictly
    /// increasing identity order, so only one record is resident at a time.
    pub(crate) fn write_relaned(
        &self,
        path: &Path,
        relane: impl FnOnce(
            &mut dyn FnMut(&str, RowLanes) -> Result<(), GraphDbError>,
        ) -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        std::fs::copy(&self.path, path).map_err(|error| index_io("copy", error))?;
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| index_io("open", error))?;
        let mut relaned = 0_u64;
        let mut previous: Option<String> = None;
        relane(&mut |identity, lanes| {
            if previous.as_deref().is_some_and(|last| last >= identity) {
                return Err(corrupt("relanes relations out of identity order"));
            }
            previous = Some(identity.to_owned());
            let Some((position, ..)) =
                self.position(self.entities, self.relations, row_key("relation", identity))?
            else {
                return Err(corrupt("relanes a relation it does not record"));
            };
            target
                .seek(SeekFrom::Start(HEADER_BYTES + position * RECORD_BYTES + 16))
                .map_err(|error| index_io("seek", error))?;
            target
                .write_all(&lanes_bytes(lanes))
                .map_err(|error| index_io("write", error))?;
            relaned += 1;
            Ok(())
        })?;
        if relaned != self.relations {
            return Err(corrupt("relanes a different relation set than it records"));
        }
        target.sync_all().map_err(|error| index_io("sync", error))
    }

    /// The sum of every recorded row: equal to the generation's row sum
    /// exactly when the index records the generation's rows.
    pub(crate) fn row_sum(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRowDigestSum, GraphDbError> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| GraphDbError::unavailable("graph row index lock is poisoned"))?;
        file.seek(SeekFrom::Start(HEADER_BYTES))
            .map_err(|error| index_io("seek", error))?;
        let mut reader = std::io::BufReader::new(&mut *file);
        let mut sum = GraphRowDigestSum::default();
        let mut record = [0_u8; RECORD_BYTES as usize];
        for position in 0..self.entities + self.relations {
            if position % 65_536 == 0 {
                check()?;
            }
            reader
                .read_exact(&mut record)
                .map_err(|error| index_io("record read", error))?;
            sum.add_row_lanes(record_parts(&record).1);
        }
        Ok(sum)
    }
}

/// Writes canonical entity-row byte ranges in identity order without
/// retaining one offset per corpus entity in memory.
pub(crate) struct EntityRowOffsetsWriter {
    writer: BufWriter<File>,
    expected: u64,
    written: u64,
}

impl EntityRowOffsetsWriter {
    pub(crate) fn create(path: &Path, expected: usize) -> Result<Self, GraphDbError> {
        let expected =
            u64::try_from(expected).map_err(|_| corrupt("entity row offset count exceeds u64"))?;
        let file = File::create(path).map_err(|error| index_io("entity offsets create", error))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(ENTITY_ROW_OFFSETS_MAGIC)
            .and_then(|()| writer.write_all(&expected.to_be_bytes()))
            .map_err(|error| index_io("entity offsets header write", error))?;
        Ok(Self {
            writer,
            expected,
            written: 0,
        })
    }

    pub(crate) fn push(&mut self, offset: u64, length: u64) -> Result<(), GraphDbError> {
        if self.written >= self.expected || length == 0 {
            return Err(corrupt("entity row offsets exceed their declared bounds"));
        }
        self.writer
            .write_all(&offset.to_be_bytes())
            .and_then(|()| self.writer.write_all(&length.to_be_bytes()))
            .map_err(|error| index_io("entity offsets write", error))?;
        self.written += 1;
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(), GraphDbError> {
        if self.written != self.expected {
            return Err(corrupt(
                "entity row offsets do not cover every declared entity",
            ));
        }
        self.writer
            .into_inner()
            .map_err(|error| index_io("entity offsets flush", error.into_error()))?
            .sync_all()
            .map_err(|error| index_io("entity offsets sync", error))
    }
}

/// Point-readable identity-order ranges into a cold base's canonical entity
/// row file.
pub(crate) struct EntityRowOffsets {
    path: PathBuf,
    file: Mutex<File>,
    entities: u64,
    rows_bytes: u64,
}

impl EntityRowOffsets {
    pub(crate) fn open(path: &Path, rows: &Path) -> Result<Self, GraphDbError> {
        let mut file = File::open(path).map_err(|error| index_io("entity offsets open", error))?;
        let mut header = [0_u8; ENTITY_ROW_OFFSETS_HEADER_BYTES as usize];
        file.read_exact(&mut header)
            .map_err(|error| index_io("entity offsets header read", error))?;
        if &header[..8] != ENTITY_ROW_OFFSETS_MAGIC {
            return Err(corrupt("entity row offsets have a foreign header"));
        }
        let entities = u64::from_be_bytes(
            header[8..]
                .try_into()
                .map_err(|_| corrupt("entity row offset count is malformed"))?,
        );
        let expected = entities
            .checked_mul(ENTITY_ROW_OFFSET_BYTES)
            .and_then(|bytes| bytes.checked_add(ENTITY_ROW_OFFSETS_HEADER_BYTES))
            .ok_or_else(|| corrupt("entity row offset count overflows its file length"))?;
        let length = file
            .metadata()
            .map_err(|error| index_io("entity offsets metadata", error))?
            .len();
        if length != expected {
            return Err(corrupt("entity row offset length does not match its count"));
        }
        let rows_bytes = rows
            .metadata()
            .map_err(|error| index_io("entity rows metadata", error))?
            .len();
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            entities,
            rows_bytes,
        })
    }

    pub(crate) fn entities(&self) -> u64 {
        self.entities
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn row(&self, ordinal: u32) -> Result<(u64, u64), GraphDbError> {
        let ordinal = u64::from(ordinal);
        if ordinal >= self.entities {
            return Err(corrupt("entity row ordinal exceeds its offset index"));
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| GraphDbError::unavailable("graph entity offset lock is poisoned"))?;
        file.seek(SeekFrom::Start(
            ENTITY_ROW_OFFSETS_HEADER_BYTES + ordinal * ENTITY_ROW_OFFSET_BYTES,
        ))
        .map_err(|error| index_io("entity offsets seek", error))?;
        let mut record = [0_u8; ENTITY_ROW_OFFSET_BYTES as usize];
        file.read_exact(&mut record)
            .map_err(|error| index_io("entity offsets read", error))?;
        let word = |range: std::ops::Range<usize>| {
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(&record[range]);
            u64::from_be_bytes(bytes)
        };
        let (offset, length) = (word(0..8), word(8..16));
        if length == 0 {
            return Err(corrupt("entity row offset records an empty row"));
        }
        if offset
            .checked_add(length)
            .is_none_or(|end| end > self.rows_bytes)
        {
            return Err(corrupt("entity row offset exceeds its row file"));
        }
        Ok((offset, length))
    }
}
