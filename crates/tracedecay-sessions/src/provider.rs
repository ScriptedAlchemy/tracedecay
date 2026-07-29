#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionProvider {
    Cursor,
    Claude,
    Codex,
    Vibe,
    Cline,
    RooCode,
    Kilo,
    Kiro,
    Hermes,
}

impl SessionProvider {
    pub const ALL: [Self; 9] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Hermes,
        Self::Kiro,
        Self::Cline,
        Self::RooCode,
        Self::Kilo,
        Self::Vibe,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Vibe => "vibe",
            Self::Cline => "cline",
            Self::RooCode => "roo-code",
            Self::Kilo => "kilo",
            Self::Kiro => "kiro",
            Self::Hermes => "hermes",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cursor" => Some(Self::Cursor),
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "vibe" => Some(Self::Vibe),
            "cline" => Some(Self::Cline),
            "roo-code" => Some(Self::RooCode),
            "kilo" => Some(Self::Kilo),
            "kiro" => Some(Self::Kiro),
            "hermes" => Some(Self::Hermes),
            _ => None,
        }
    }

    pub const fn supports_host_admission(self) -> bool {
        true
    }

    pub const fn scans_all_destinations(self) -> bool {
        matches!(self, Self::Hermes)
    }
}

pub const MESSAGE_SEARCH_PROVIDER_IDS: &[&str] = &[
    "all", "cursor", "claude", "codex", "vibe", "cline", "roo-code", "kilo", "kiro", "hermes",
];

pub const EXPECTED_MESSAGE_SEARCH_PROVIDER: &str =
    "all, cursor, claude, codex, vibe, cline, roo-code, kilo, kiro, or hermes";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderScope {
    All,
    One(SessionProvider),
}

impl ProviderScope {
    pub fn parse_optional(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|provider| !provider.is_empty()) {
            None | Some("all") => Ok(Self::All),
            Some(provider) => SessionProvider::parse(provider)
                .map(Self::One)
                .ok_or_else(|| {
                    format!(
                        "unknown session provider '{provider}' (expected {EXPECTED_MESSAGE_SEARCH_PROVIDER})"
                    )
                }),
        }
    }

    pub const fn provider(self) -> Option<SessionProvider> {
        match self {
            Self::All => None,
            Self::One(provider) => Some(provider),
        }
    }

    pub const fn provider_id(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::One(provider) => Some(provider.id()),
        }
    }

    pub const fn response_label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::One(provider) => provider.id(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderScope, SessionProvider};

    #[test]
    fn provider_ids_round_trip() {
        for provider in SessionProvider::ALL {
            assert_eq!(SessionProvider::parse(provider.id()), Some(provider));
        }
    }

    #[test]
    fn optional_scope_defaults_only_to_all() {
        assert_eq!(ProviderScope::parse_optional(None), Ok(ProviderScope::All));
        assert_eq!(
            ProviderScope::parse_optional(Some("vibe")),
            Ok(ProviderScope::One(SessionProvider::Vibe))
        );
        assert!(ProviderScope::parse_optional(Some("unknown")).is_err());
    }
}
