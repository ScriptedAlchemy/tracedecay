use crate::chunks::CodeIndexImportEvidenceV1;

use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};

pub(super) fn derive_import_evidence(
    files: &[FileGenerationArtifactsV1],
) -> Vec<CodeIndexImportEvidenceV1> {
    derive_import_evidence_from(files.iter())
}

fn derive_import_evidence_from<'a>(
    files: impl Iterator<Item = &'a FileGenerationArtifactsV1>,
) -> Vec<CodeIndexImportEvidenceV1> {
    let mut files = files.collect::<Vec<_>>();
    files.sort_by(|left, right| {
        (
            &left.authority.logical_path,
            &left.artifacts.chunks.document.file_occurrence_id,
        )
            .cmp(&(
                &right.authority.logical_path,
                &right.artifacts.chunks.document.file_occurrence_id,
            ))
    });
    files
        .into_iter()
        .flat_map(|file| file.artifacts.imports.iter().cloned())
        .collect()
}

pub(super) fn validate_import_evidence(
    files: &[&FileGenerationArtifactsV1],
    imports: &[CodeIndexImportEvidenceV1],
) -> Result<(), CodeIndexProductionErrorV1> {
    for file in files {
        file.artifacts
            .validate_generation_import_authority(&file.extraction)
            .map_err(CodeIndexProductionErrorV1::Chunk)?;
        if file
            .artifacts
            .imports
            .iter()
            .any(|row| row.logical_path != file.authority.logical_path)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "published import evidence does not match its file authority".to_owned(),
            ));
        }
    }
    if derive_import_evidence_from(files.iter().copied()) != imports {
        return Err(CodeIndexProductionErrorV1::Contract(
            "published import aggregate does not match file artifacts".to_owned(),
        ));
    }
    Ok(())
}
