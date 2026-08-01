//! The session-evidence contract automation states its queries in.
//!
//! `automation::runner` builds evidence for the memory curator, session
//! reflector, and skill writer by grepping stored session transcripts. The
//! LCM query engine that answers those greps lives in `tracedecay-sessions`
//! behind a runtime this crate must not open for itself, so the request
//! selectors and the hit shape are declared here and the execution arrives
//! through `runner::retrieval`'s `AutomationSessionRetrieval` port.
//!
//! These deliberately mirror `sessions::lcm`'s selectors rather than reusing
//! them: this crate states *what evidence automation wants*, and the session
//! runtime decides how to satisfy it. The serde representations match, so the
//! root adapter is a field-for-field conversion.
//!
//! Root wiring: the root converts between these and
//! `sessions::lcm::{LcmScope, LcmGrepSort, LcmGrepHit}` in the adapter it
//! registers as `AutomationSessionRetrieval`. `SEAMS.md` tracks the row.

use serde::{Deserialize, Serialize};

/// How wide a session-evidence grep may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmScope {
    /// The current session only.
    Current,
    /// One explicitly named session.
    Session,
    /// Every session in scope for the request.
    All,
}

/// Ordering applied to session-evidence hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmGrepSort {
    /// Newest knowledge first.
    Recency,
    /// Best match first.
    Relevance,
    /// Relevance, tie-broken by recency.
    Hybrid,
}

impl std::str::FromStr for LcmGrepSort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "recency" => Ok(Self::Recency),
            "relevance" => Ok(Self::Relevance),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(()),
        }
    }
}

/// One session-transcript match, as automation cites it in evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmGrepHit {
    pub kind: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub node_id: Option<String>,
    pub store_id: Option<i64>,
    /// Raw-message role (`assistant`/`user`/`tool`/`system`); `None` for
    /// summary nodes and rows ingested before roles were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub snippet: String,
}
