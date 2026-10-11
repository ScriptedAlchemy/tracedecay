use tracedecay_code_index::clones::{CloneExactKeyV1, CloneNormalizationClassV1};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, SymbolOccurrenceId, canonical_sha256,
};

use super::super::clone_codec::{digest_from_key, digest_key};
use super::super::row_codec::symbol_id_from_key;
use super::{CodeLexicalArtifactReaderV1, MAX_CLONE_EXACT_PAGE_MEMBERS_V1};
use crate::retrieval::lexical::projection::artifact::{
    CodeLexicalArtifactErrorV1, checkpoint, sqlite_error,
};
use crate::retrieval::ports::RETRIEVAL_CANDIDATE_BATCH_SIZE;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct CloneFamilyCursorV1 {
    artifact_digest: ManifestDigest,
    generation: CodeGenerationId,
    request_digest: ManifestDigest,
    after: CloneFamilyCursorPositionV1,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct CloneFamilyCursorPositionV1 {
    reviewable_source_bytes: u64,
    member_count: usize,
    class: CloneNormalizationClassV1,
    normalization_revision: u16,
    digest: ManifestDigest,
}

impl CloneFamilyCursorV1 {
    fn encode(&self) -> Result<String, CodeLexicalArtifactErrorV1> {
        serde_json::to_vec(self)
            .map(hex::encode)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))
    }

    fn decode(encoded: &str) -> Result<Self, CodeLexicalArtifactErrorV1> {
        let bytes = hex::decode(encoded)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneExactFamilyArtifactCandidateV1 {
    pub key: CloneExactKeyV1,
    pub representative: SymbolOccurrenceId,
    pub member_count: usize,
    pub reviewable_source_bytes: u64,
    pub continuation: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneExactFamilyArtifactPageV1 {
    pub families: Vec<CloneExactFamilyArtifactCandidateV1>,
    pub next_cursor: Option<String>,
}

impl CodeLexicalArtifactReaderV1 {
    #[allow(clippy::too_many_arguments)] // mirrors sibling clone page readers' filter/cursor surface
    pub fn clone_exact_family_page(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        match_classes: &[CloneNormalizationClassV1],
        path: Option<&str>,
        pull_request_paths: Option<&[String]>,
        pull_request_scope_digest: Option<&ManifestDigest>,
        include_generated_paths: bool,
        cursor: Option<&str>,
        limit: usize,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CloneExactFamilyArtifactPageV1, CodeLexicalArtifactErrorV1> {
        checkpoint(control)?;
        if self.metadata.repository_id.as_ref() != Some(repository_id) {
            return Err(CodeLexicalArtifactErrorV1::Missing(
                "clone family repository authority is unavailable".to_owned(),
            ));
        }
        if limit == 0 || limit > MAX_CLONE_EXACT_PAGE_MEMBERS_V1 {
            return Err(CodeLexicalArtifactErrorV1::Contract(format!(
                "clone family page limit must be within 1..={MAX_CLONE_EXACT_PAGE_MEMBERS_V1}"
            )));
        }
        let mut match_classes = match_classes.to_vec();
        match_classes.sort();
        match_classes.dedup();
        let request_digest = canonical_sha256(&(
            "tracedecay.clone-family-request.v1",
            self.receipt.artifact_digest(),
            project_id,
            repository_id,
            &match_classes,
            path,
            pull_request_paths,
            pull_request_scope_digest,
            include_generated_paths,
        ))
        .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let cursor = cursor.map(CloneFamilyCursorV1::decode).transpose()?;
        let after = match cursor.as_ref() {
            Some(cursor)
                if cursor.artifact_digest == *self.receipt.artifact_digest()
                    && cursor.generation == self.metadata.generation
                    && cursor.request_digest == request_digest =>
            {
                Some(&cursor.after)
            }
            Some(_) => return Err(CodeLexicalArtifactErrorV1::StaleCloneFamilyCursor),
            None => None,
        };
        let fetch = limit.checked_add(1).ok_or_else(|| {
            CodeLexicalArtifactErrorV1::Contract("clone family page limit overflowed".to_owned())
        })?;
        let after_reviewable = after
            .map(|position| position.reviewable_source_bytes)
            .unwrap_or_default();
        let after_members = after
            .map(|position| position.member_count)
            .unwrap_or_default();
        let after_class = after
            .map(|position| position.class as u8)
            .unwrap_or_default();
        let after_revision = after
            .map(|position| position.normalization_revision)
            .unwrap_or_default();
        let after_digest = after
            .map(|position| digest_key(&position.digest))
            .transpose()?
            .unwrap_or_default();
        // Path policy is bound as JSON arrays the query walks with
        // `json_each`, so the statement needs no registered function.
        let generated_segments =
            serde_json::to_string(tracedecay_domain::GENERATED_DIR_SEGMENTS)
                .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let pull_request_paths_json = serde_json::to_string(pull_request_paths.unwrap_or_default())
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let connection = self.lock_connection(Some(&|| checkpoint(control)))?;
        let mut statement = connection
            .prepare_cached(
                "WITH families AS ( \
                    SELECT posting.class, posting.normalization_revision, posting.digest, \
                           MIN(occurrence.symbol_key) AS representative, \
                           COUNT(*) AS member_count, \
                           SUM(occurrence.body_end - occurrence.body_start) \
                               - MIN(occurrence.body_end - occurrence.body_start) \
                               AS reviewable_source_bytes \
                    FROM clone_exact_postings AS posting \
                    JOIN clone_occurrences AS occurrence \
                      ON occurrence.ordinal = posting.occurrence_ordinal \
                    WHERE ((?1 AND posting.class = 1) OR (?2 AND posting.class = 2)) \
                      AND (?3 IS NULL OR occurrence.path = ?3 \
                           OR (substr(occurrence.path, 1, length(?3)) = ?3 \
                               AND substr(occurrence.path, length(?3) + 1, 1) = '/')) \
                      AND (?5 OR NOT EXISTS ( \
                           SELECT 1 FROM json_each(?13) AS segment \
                           WHERE instr('/' || occurrence.path || '/', '/' || segment.value || '/') > 0)) \
                    GROUP BY posting.class, posting.normalization_revision, posting.digest \
                    HAVING COUNT(*) > 1 \
                       AND (NOT ?4 \
                            OR MAX(occurrence.path IN (SELECT value FROM json_each(?14)))) \
                 ) \
                 SELECT class, normalization_revision, digest, representative, member_count, reviewable_source_bytes \
                 FROM families \
                 WHERE NOT ?6 \
                    OR reviewable_source_bytes < ?7 \
                    OR (reviewable_source_bytes = ?7 AND member_count < ?8) \
                    OR (reviewable_source_bytes = ?7 AND member_count = ?8 AND class > ?9) \
                    OR (reviewable_source_bytes = ?7 AND member_count = ?8 AND class = ?9 AND normalization_revision > ?10) \
                    OR (reviewable_source_bytes = ?7 AND member_count = ?8 AND class = ?9 AND normalization_revision = ?10 AND digest > ?11) \
                 ORDER BY reviewable_source_bytes DESC, member_count DESC, class, normalization_revision, digest \
                 LIMIT ?12",
            )
            .map_err(sqlite_error)?;
        let after_reviewable = i64::try_from(after_reviewable)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let after_members = i64::try_from(after_members)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let fetch_rows = i64::try_from(fetch)
            .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?;
        let mut rows = statement
            .query(tracedecay_sql::params![
                match_classes.contains(&CloneNormalizationClassV1::Conservative),
                match_classes.contains(&CloneNormalizationClassV1::Rename),
                path,
                pull_request_paths.is_some(),
                include_generated_paths,
                after.is_some(),
                after_reviewable,
                after_members,
                i64::from(after_class),
                i64::from(after_revision),
                after_digest.as_slice(),
                fetch_rows,
                generated_segments,
                pull_request_paths_json,
            ])
            .map_err(sqlite_error)?;
        let mut families = Vec::with_capacity(fetch);
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            if families
                .len()
                .is_multiple_of(RETRIEVAL_CANDIDATE_BATCH_SIZE)
            {
                checkpoint(control)?;
            }
            let class = match row.get::<_, i64>(0).map_err(sqlite_error)? {
                1 => CloneNormalizationClassV1::Conservative,
                2 => CloneNormalizationClassV1::Rename,
                other => {
                    return Err(CodeLexicalArtifactErrorV1::Corrupt(format!(
                        "clone family has unknown normalization class {other}"
                    )));
                }
            };
            let normalization_revision = u16::try_from(row.get::<_, i64>(1).map_err(sqlite_error)?)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            let digest = digest_from_key(&row.get::<_, Vec<u8>>(2).map_err(sqlite_error)?)?;
            let representative =
                SymbolOccurrenceId::new(symbol_id_from_key(row.get_ref(3).map_err(sqlite_error)?)?)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            let member_count = usize::try_from(row.get::<_, i64>(4).map_err(sqlite_error)?)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            let reviewable_source_bytes =
                u64::try_from(row.get::<_, i64>(5).map_err(sqlite_error)?)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            let continuation = CloneFamilyCursorV1 {
                artifact_digest: self.receipt.artifact_digest().clone(),
                generation: self.metadata.generation.clone(),
                request_digest: request_digest.clone(),
                after: CloneFamilyCursorPositionV1 {
                    reviewable_source_bytes,
                    member_count,
                    class,
                    normalization_revision,
                    digest: digest.clone(),
                },
            }
            .encode()?;
            families.push(CloneExactFamilyArtifactCandidateV1 {
                key: CloneExactKeyV1 {
                    class,
                    normalization_revision,
                    digest,
                },
                representative,
                member_count,
                reviewable_source_bytes,
                continuation,
            });
        }
        checkpoint(control)?;
        let next_cursor = (families.len() > limit)
            .then(|| {
                families
                    .get(limit - 1)
                    .map(|family| family.continuation.clone())
            })
            .flatten();
        families.truncate(limit);
        Ok(CloneExactFamilyArtifactPageV1 {
            families,
            next_cursor,
        })
    }
}
