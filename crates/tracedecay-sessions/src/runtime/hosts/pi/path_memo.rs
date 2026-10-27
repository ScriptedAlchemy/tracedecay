//! Durable convergence for one Pi path, under its canonical rewrite witness.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_domain::ObservationScopeV1;

use super::{PROVIDER, admission_error, invalid_frame, source_digest};
use crate::admission::HostAdmission;
use crate::runtime::ingest::quarantine::{Witness, stat_if_present};
use crate::runtime::source::{TranscriptIngestResult, run_blocking_transcript_section};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Disposition {
    Complete,
    InvalidHeader,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Memo {
    witness: Witness,
    status: Disposition,
}

pub(super) struct PathMemo {
    key: String,
    stored: Option<String>,
    decoded: Option<Memo>,
}

pub(super) fn file_witness(path: &Path) -> TranscriptIngestResult<Option<Witness>> {
    stat_if_present(path, "stat Pi transcript for convergence")?
        .as_ref()
        .map_or(Ok(None), |metadata| Witness::settled_file(path, metadata))
}

pub(super) async fn observe_file_witness(path: &Path) -> TranscriptIngestResult<Option<Witness>> {
    let path = path.to_path_buf();
    run_blocking_transcript_section(PROVIDER, move || file_witness(&path)).await?
}

impl PathMemo {
    pub(super) async fn read(
        facade: &dyn HostAdmission,
        scope: &ObservationScopeV1,
        path: &Path,
    ) -> TranscriptIngestResult<Self> {
        let key = format!("host-memo://pi/file/v1/{}", source_digest(path));
        let stored = facade
            .read_session_backfill_state(scope, &key)
            .await
            .map_err(admission_error)?;
        let decoded = stored
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| invalid_frame())?;
        Ok(Self {
            key,
            stored,
            decoded,
        })
    }

    pub(super) fn disposition(&self, witness: Option<Witness>) -> Option<Disposition> {
        let witness = witness?;
        self.decoded
            .as_ref()
            .filter(|memo| memo.witness == witness)
            .map(|memo| memo.status)
    }

    /// The expected value was read before examining/admitting the file. A
    /// concurrent writer wins; the next pass re-observes its decision instead
    /// of replacing it with an older file's outcome.
    pub(super) async fn record(
        &self,
        facade: &dyn HostAdmission,
        scope: &ObservationScopeV1,
        witness: Witness,
        status: Disposition,
    ) -> TranscriptIngestResult<bool> {
        let replacement =
            serde_json::to_string(&Memo { witness, status }).map_err(|_| invalid_frame())?;
        facade
            .compare_and_swap_session_backfill_state(
                scope,
                &self.key,
                self.stored.as_deref(),
                &replacement,
            )
            .await
            .map_err(admission_error)
    }
}
