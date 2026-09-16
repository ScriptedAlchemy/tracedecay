use std::collections::BTreeSet;
use std::path::{Component, Path};

use rusqlite::functions::FunctionFlags;
use tracedecay_code_index::clones::{CloneExactKeyV1, CloneNormalizationClassV1};
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, RepositoryId, SymbolOccurrenceId, canonical_sha256,
};

use super::{CodeLexicalArtifactReaderV1, MAX_CLONE_EXACT_PAGE_MEMBERS_V1};
use crate::retrieval::lexical::projection::artifact::{
    CodeLexicalArtifactErrorV1, checkpoint, sqlite_error,
};

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

const GENERATED_PATH_FUNCTION: &str = "tracedecay_is_generated_path";
const PULL_REQUEST_PATH_FUNCTION: &str = "tracedecay_is_pull_request_path";

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
        if !self.layout.has_clone_index() {
            return Err(CodeLexicalArtifactErrorV1::Incompatible(
                "clone family lookup requires lexical artifact revision 15".to_owned(),
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
            Some(_) => {
                return Err(CodeLexicalArtifactErrorV1::Contract(
                    "clone family cursor does not match its artifact or request".to_owned(),
                ));
            }
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
        let after_digest = after.map(|position| position.digest.as_str()).unwrap_or("");
        let connection = self.lock_connection()?;
        install_generated_path_function(&connection)?;
        install_pull_request_path_function(&connection, pull_request_paths)?;
        let mut statement = connection
            .prepare_cached(
                "WITH families AS ( \
                    SELECT posting.class, posting.normalization_revision, posting.digest, \
                           MIN(posting.symbol_occurrence_id) AS representative, \
                           COUNT(*) AS member_count, \
                           SUM(occurrence.body_end - occurrence.body_start) \
                               - MIN(occurrence.body_end - occurrence.body_start) \
                               AS reviewable_source_bytes \
                    FROM clone_exact_postings AS posting \
                    JOIN clone_occurrences AS occurrence \
                      ON occurrence.symbol_occurrence_id = posting.symbol_occurrence_id \
                    WHERE ((:conservative AND posting.class = 1) OR (:rename AND posting.class = 2)) \
                      AND (:path IS NULL OR occurrence.path = :path \
                           OR (substr(occurrence.path, 1, length(:path)) = :path \
                               AND substr(occurrence.path, length(:path) + 1, 1) = '/')) \
                      AND (:include_generated OR tracedecay_is_generated_path(occurrence.path) = 0) \
                    GROUP BY posting.class, posting.normalization_revision, posting.digest \
                    HAVING COUNT(*) > 1 \
                       AND (NOT :pull_request \
                            OR MAX(tracedecay_is_pull_request_path(occurrence.path))) \
                 ) \
                 SELECT class, normalization_revision, digest, representative, member_count, reviewable_source_bytes \
                 FROM families \
                 WHERE NOT :has_after \
                    OR reviewable_source_bytes < :after_reviewable \
                    OR (reviewable_source_bytes = :after_reviewable AND member_count < :after_members) \
                    OR (reviewable_source_bytes = :after_reviewable AND member_count = :after_members AND class > :after_class) \
                    OR (reviewable_source_bytes = :after_reviewable AND member_count = :after_members AND class = :after_class AND normalization_revision > :after_revision) \
                    OR (reviewable_source_bytes = :after_reviewable AND member_count = :after_members AND class = :after_class AND normalization_revision = :after_revision AND digest > :after_digest) \
                 ORDER BY reviewable_source_bytes DESC, member_count DESC, class, normalization_revision, digest \
                 LIMIT :fetch",
            )
            .map_err(sqlite_error)?;
        let mut rows = statement
            .query(rusqlite::named_params! {
                ":conservative": match_classes.contains(&CloneNormalizationClassV1::Conservative),
                ":rename": match_classes.contains(&CloneNormalizationClassV1::Rename),
                ":path": path,
                ":pull_request": pull_request_paths.is_some(),
                ":include_generated": include_generated_paths,
                ":has_after": after.is_some(),
                ":after_reviewable": i64::try_from(after_reviewable)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                ":after_members": i64::try_from(after_members)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
                ":after_class": i64::from(after_class),
                ":after_revision": i64::from(after_revision),
                ":after_digest": after_digest,
                ":fetch": i64::try_from(fetch)
                    .map_err(|error| CodeLexicalArtifactErrorV1::Contract(error.to_string()))?,
            })
            .map_err(sqlite_error)?;
        let mut families = Vec::with_capacity(fetch);
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            checkpoint(control)?;
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
            let digest = ManifestDigest::new(row.get::<_, String>(2).map_err(sqlite_error)?)
                .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
            let representative =
                SymbolOccurrenceId::new(row.get::<_, String>(3).map_err(sqlite_error)?)
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

fn install_generated_path_function(
    connection: &rusqlite::Connection,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    connection
        .create_scalar_function(
            GENERATED_PATH_FUNCTION,
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let path = context.get::<String>(0)?;
                Ok(Path::new(&path).components().any(|component| {
                    matches!(
                        component,
                        Component::Normal(segment)
                            if segment
                                .to_str()
                                .is_some_and(tracedecay_domain::is_generated_dir_segment)
                    )
                }))
            },
        )
        .map_err(sqlite_error)
}

fn install_pull_request_path_function(
    connection: &rusqlite::Connection,
    paths: Option<&[String]>,
) -> Result<(), CodeLexicalArtifactErrorV1> {
    let paths = paths
        .into_iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    connection
        .create_scalar_function(
            PULL_REQUEST_PATH_FUNCTION,
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| {
                let path = context.get::<String>(0)?;
                Ok(paths.contains(&path))
            },
        )
        .map_err(sqlite_error)
}
