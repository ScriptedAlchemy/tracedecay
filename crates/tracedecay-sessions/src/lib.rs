//! Provider-neutral session parsing and retrieval primitives.

pub mod compatibility;
pub mod lcm;
pub mod provider;

pub use provider::{
    EXPECTED_MESSAGE_SEARCH_PROVIDER, MESSAGE_SEARCH_PROVIDER_IDS, ProviderScope, SessionProvider,
};

/// Semantic message filter shared by full-text and LCM retrieval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionMessageType {
    #[default]
    All,
    DirectUser,
    ToolResult,
}

impl SessionMessageType {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "direct_user" => Some(Self::DirectUser),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::DirectUser => "direct_user",
            Self::ToolResult => "tool_result",
        }
    }
}
