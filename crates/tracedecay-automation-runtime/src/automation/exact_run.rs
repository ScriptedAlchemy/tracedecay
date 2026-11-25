//! Exact terminal publication identity shared by the registered run store and
//! the retained external-effect journal.
//!
//! The journal binds a run's staged terminal by this identity before the
//! terminal becomes visible, so recovery can publish or discard exactly that
//! terminal after any crash boundary.

use serde::{Deserialize, Serialize};
use tracedecay_domain::errors::Result;
use tracedecay_global_db::AutomationRunStagedPublicationV1;

use super::config_error;

const EXACT_RUN_PUBLICATION_SCHEMA: u32 = 2;

/// The canonical digest a staged terminal bound before publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactRunPublication {
    schema_version: u32,
    digest: String,
}

impl ExactRunPublication {
    pub(crate) fn from_staged(staged: AutomationRunStagedPublicationV1) -> Self {
        Self {
            schema_version: EXACT_RUN_PUBLICATION_SCHEMA,
            digest: staged.digest,
        }
    }

    pub(crate) fn staged_for(&self, run_id: &str) -> AutomationRunStagedPublicationV1 {
        AutomationRunStagedPublicationV1 {
            run_id: run_id.to_owned(),
            digest: self.digest.clone(),
        }
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != EXACT_RUN_PUBLICATION_SCHEMA {
            return Err(config_error(
                "exact automation run publication has an unsupported schema",
            ));
        }
        if self.digest.is_empty() {
            return Err(config_error(
                "exact automation run publication has an empty digest",
            ));
        }
        Ok(())
    }
}

/// Whether a bound terminal still had its staged payload or committed row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactRunPublishOutcome {
    Published,
    MissingPayload,
}

/// Whether an abandoned reservation's staged terminals were retired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactRunUnboundDiscardOutcome {
    Discarded,
    Retained,
}
