//! A sealed code graph's resolution inputs, kept beside its sealed store so a
//! refresh can re-resolve without decoding the files it did not change.
//!
//! One gzip stream of JSON lines: a header naming the generation and the
//! projector revision, one line per snapshot file (its segment digest, the
//! file reduced to what cross-file resolution reads, and its symbol
//! bindings), then the generation's resolution outputs. A refresh reuses a
//! file's line exactly when its segment digest is unchanged: segments are
//! content addressed, so equal digests mean equal resolution inputs.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, FileOccurrenceId, ManifestDigest,
    SanitizedCodeSnapshotV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
};

use crate::chunks::CodeIndexUnresolvedReferenceV1;
use crate::graph_projection::CodeGraphSymbolBindingV1;

use super::CodeIndexProductionErrorV1;
use super::sealed_codec::PersistedFileGenerationArtifactsV1;

/// Bumped whenever a line's shape or a reduced page's meaning changes; a
/// base recorded under another revision offers no reuse and the refresh
/// builds cold.
pub(super) const CODE_GRAPH_BASE_INPUTS_REVISION_V1: u32 = 1;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum CodeGraphBaseInputsLineV1 {
    Header {
        revision: u32,
        generation: CodeGenerationId,
        projector_revision: String,
    },
    File(Box<CodeGraphBaseFileV1>),
    Resolution {
        cross_file_edges: Vec<CanonicalRelationEdgeV1>,
        unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    },
}

/// One base snapshot file: its segment and graph inputs when present.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodeGraphBaseFileV1 {
    pub(super) file_occurrence_id: FileOccurrenceId,
    pub(super) segment_digest: Option<ManifestDigest>,
    pub(super) page: Option<PersistedFileGenerationArtifactsV1>,
    pub(super) bindings: BTreeMap<SymbolOccurrenceId, CodeGraphSymbolBindingV1>,
}

/// A base generation's inputs, read back whole.
pub(super) struct CodeGraphBaseInputsV1 {
    pub(super) generation: CodeGenerationId,
    pub(super) files: BTreeMap<FileOccurrenceId, CodeGraphBaseFileV1>,
    pub(super) cross_file_edges: Vec<CanonicalRelationEdgeV1>,
    pub(super) unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
}

fn inputs_io(context: &str, error: impl std::fmt::Display) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(format!("code graph base inputs {context}: {error}"))
}

/// Streams a cold build's inputs as its files decode.
pub(crate) struct CodeGraphBaseInputsWriterV1 {
    encoder: GzEncoder<BufWriter<File>>,
}

impl CodeGraphBaseInputsWriterV1 {
    pub(crate) fn create(
        path: &Path,
        generation: &CodeGenerationId,
        projector_revision: &str,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let file = File::create(path).map_err(|error| inputs_io("create", error))?;
        let mut writer = Self {
            encoder: GzEncoder::new(BufWriter::new(file), Compression::fast()),
        };
        writer.line(&CodeGraphBaseInputsLineV1::Header {
            revision: CODE_GRAPH_BASE_INPUTS_REVISION_V1,
            generation: generation.clone(),
            projector_revision: projector_revision.to_owned(),
        })?;
        Ok(writer)
    }

    fn line(&mut self, line: &CodeGraphBaseInputsLineV1) -> Result<(), CodeIndexProductionErrorV1> {
        serde_json::to_writer(&mut self.encoder, line)
            .map_err(|error| inputs_io("encode", error))?;
        self.encoder
            .write_all(b"\n")
            .map_err(|error| inputs_io("write", error))
    }

    pub(super) fn file(
        &mut self,
        file: CodeGraphBaseFileV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        self.line(&CodeGraphBaseInputsLineV1::File(Box::new(file)))
    }

    /// Records every snapshot file sealed without a segment.
    pub(super) fn unsegmented_files(
        &mut self,
        snapshot: &SanitizedCodeSnapshotV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        for file in &snapshot.files {
            if file.disposition != SnapshotFileDispositionV1::Present {
                self.file(CodeGraphBaseFileV1 {
                    file_occurrence_id: file.file_occurrence_id.clone(),
                    segment_digest: None,
                    page: None,
                    bindings: BTreeMap::new(),
                })?;
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        cross_file_edges: Vec<CanonicalRelationEdgeV1>,
        unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        self.line(&CodeGraphBaseInputsLineV1::Resolution {
            cross_file_edges,
            unresolved_calls,
        })?;
        self.encoder
            .finish()
            .and_then(|mut writer| writer.flush())
            .map_err(|error| inputs_io("finish", error))
    }
}

/// Reads a base's inputs; `None` when they were recorded under another
/// revision or projector, which the refresh answers with a cold build.
#[hotpath::measure(label = "code_index.graph.layered.read_base_inputs")]
pub(super) fn read_code_graph_base_inputs(
    path: &Path,
    projector_revision: &str,
) -> Result<Option<CodeGraphBaseInputsV1>, CodeIndexProductionErrorV1> {
    let file = File::open(path).map_err(|error| inputs_io("open", error))?;
    let mut lines = BufReader::new(GzDecoder::new(BufReader::new(file))).lines();
    let mut next = || -> Result<Option<CodeGraphBaseInputsLineV1>, CodeIndexProductionErrorV1> {
        lines
            .next()
            .transpose()
            .map_err(|error| inputs_io("read", error))?
            .map(|line| serde_json::from_str(&line).map_err(|error| inputs_io("decode", error)))
            .transpose()
    };
    let generation = match next()? {
        Some(CodeGraphBaseInputsLineV1::Header {
            revision,
            generation,
            projector_revision: recorded,
        }) => {
            if revision != CODE_GRAPH_BASE_INPUTS_REVISION_V1 || recorded != projector_revision {
                return Ok(None);
            }
            generation
        }
        _ => return Err(inputs_io("header", "the stream does not begin with one")),
    };
    let mut files = BTreeMap::new();
    loop {
        match next()? {
            Some(CodeGraphBaseInputsLineV1::File(file)) => {
                if files
                    .insert(file.file_occurrence_id.clone(), *file)
                    .is_some()
                {
                    return Err(inputs_io("file", "a snapshot file appears twice"));
                }
            }
            Some(CodeGraphBaseInputsLineV1::Resolution {
                cross_file_edges,
                unresolved_calls,
            }) => {
                if next()?.is_some() {
                    return Err(inputs_io("resolution", "lines follow the resolution"));
                }
                return Ok(Some(CodeGraphBaseInputsV1 {
                    generation,
                    files,
                    cross_file_edges,
                    unresolved_calls,
                }));
            }
            Some(CodeGraphBaseInputsLineV1::Header { .. }) => {
                return Err(inputs_io("header", "a second header"));
            }
            None => return Err(inputs_io("resolution", "the stream ends before it")),
        }
    }
}
