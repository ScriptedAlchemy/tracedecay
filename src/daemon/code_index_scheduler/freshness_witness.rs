//! Durable source-freshness evidence for one activated code generation.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use tracedecay_code_index::production::CodeIndexIgnoredSourceAdmissionV1;

use super::{CodeIndexSchedulerErrorV1, StaticLanguageRegistry, classification, sha256_hex};
use crate::code_index::languages::LanguageRegistry;

const FRESHNESS_WITNESS_FILE_NAME: &str = "freshness_witness.v1";

/// A cheap stat-level signature over every ordinary or explicitly admitted
/// source candidate. Ignored admissions are part of the signature even though
/// gix deliberately omits them from its ordinary candidate set.
pub(super) fn worktree_stat_signature_for(
    project_root: &Path,
    ignored_source_admissions: &[CodeIndexIgnoredSourceAdmissionV1],
) -> Result<String, CodeIndexSchedulerErrorV1> {
    let repository = gix::open(project_root)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let classification = classification::WorktreeChangeClassificationV1::classify(&repository)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let registry = StaticLanguageRegistry::new();
    let mut candidate_paths = classification.candidate_paths();
    candidate_paths.extend(
        ignored_source_admissions
            .iter()
            .map(|admission| admission.logical_path.clone()),
    );
    let mut buf = Vec::new();
    for logical_path in candidate_paths {
        let absolute = project_root.join(&logical_path);
        let Some(extension) = absolute.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if registry
            .descriptor_for_extension(&extension.to_lowercase())
            .is_none()
        {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&absolute) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let mtime_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0u128, |elapsed| elapsed.as_nanos());
        buf.extend_from_slice(logical_path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&metadata.len().to_le_bytes());
        buf.extend_from_slice(&mtime_nanos.to_le_bytes());
        buf.push(0xff);
    }
    Ok(format!("sha256:{}", sha256_hex(&buf)))
}

/// Durable binding between one sealed generation and the exact ordinary plus
/// ignored-source state against which it was reconciled.
pub(super) struct RestoreFreshnessWitnessV1 {
    pub generation_id: String,
    pub git_metadata_signature: String,
    pub stat_signature: String,
    pub repository_parse_identity_digest: String,
    pub ignored_source_admissions_digest: String,
    pub ignored_source_paths: Vec<String>,
}

impl RestoreFreshnessWitnessV1 {
    fn witness_path(store_root: &Path) -> PathBuf {
        store_root.join(FRESHNESS_WITNESS_FILE_NAME)
    }

    fn encode(&self) -> String {
        let mut fields = vec![
            self.generation_id.clone(),
            self.git_metadata_signature.clone(),
            self.stat_signature.clone(),
            self.repository_parse_identity_digest.clone(),
            self.ignored_source_admissions_digest.clone(),
            self.ignored_source_paths.len().to_string(),
        ];
        fields.extend(self.ignored_source_paths.iter().cloned());
        fields.push(String::new());
        fields.join("\n")
    }

    fn decode(contents: &str) -> Option<Self> {
        let fields = contents.lines().collect::<Vec<_>>();
        if fields.len() < 6 {
            return None;
        }
        let path_count = fields[5].parse::<usize>().ok()?;
        if fields.len() != 6usize.checked_add(path_count)?
            || fields[..5].iter().any(|field| field.is_empty())
            || fields[6..].iter().any(|path| path.is_empty())
        {
            return None;
        }
        Some(Self {
            generation_id: fields[0].to_owned(),
            git_metadata_signature: fields[1].to_owned(),
            stat_signature: fields[2].to_owned(),
            repository_parse_identity_digest: fields[3].to_owned(),
            ignored_source_admissions_digest: fields[4].to_owned(),
            ignored_source_paths: fields[6..].iter().map(|path| (*path).to_owned()).collect(),
        })
    }

    pub(super) fn load(store_root: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(Self::witness_path(store_root)).ok()?;
        Self::decode(&contents)
    }

    /// Atomic replacement makes a torn witness indistinguishable from an
    /// absent witness: either state safely forces a full reconcile on restart.
    pub(super) fn persist(&self, store_root: &Path) {
        let path = Self::witness_path(store_root);
        let temp = store_root.join(format!("{FRESHNESS_WITNESS_FILE_NAME}.tmp"));
        if std::fs::write(&temp, self.encode()).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
}
