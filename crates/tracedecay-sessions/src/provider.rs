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
    pub fn parse_optional(value: Option<&str>) -> std::result::Result<Self, String> {
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
    use super::*;

    const PROVIDERS: [SessionProvider; 9] = [
        SessionProvider::Cursor,
        SessionProvider::Claude,
        SessionProvider::Codex,
        SessionProvider::Vibe,
        SessionProvider::Cline,
        SessionProvider::RooCode,
        SessionProvider::Kilo,
        SessionProvider::Kiro,
        SessionProvider::Hermes,
    ];

    #[test]
    fn provider_ids_round_trip_in_search_order() {
        assert_eq!(MESSAGE_SEARCH_PROVIDER_IDS.first(), Some(&"all"));
        assert_eq!(MESSAGE_SEARCH_PROVIDER_IDS.len(), PROVIDERS.len() + 1);

        for provider in PROVIDERS {
            let id = provider.id();
            assert_eq!(SessionProvider::parse(id), Some(provider));
            assert!(MESSAGE_SEARCH_PROVIDER_IDS.contains(&id));
        }

        assert_eq!(SessionProvider::parse("all"), None);
        assert_eq!(SessionProvider::parse(" Codex "), None);
        assert_eq!(SessionProvider::parse("CODEX"), None);
    }

    #[test]
    fn provider_scope_parses_optional_values_and_reports_labels() {
        assert_eq!(ProviderScope::parse_optional(None), Ok(ProviderScope::All));
        assert_eq!(
            ProviderScope::parse_optional(Some(" \t")),
            Ok(ProviderScope::All)
        );
        assert_eq!(
            ProviderScope::parse_optional(Some(" all ")),
            Ok(ProviderScope::All)
        );

        let scope = ProviderScope::parse_optional(Some(" roo-code ")).unwrap();
        assert_eq!(scope, ProviderScope::One(SessionProvider::RooCode));
        assert_eq!(scope.provider(), Some(SessionProvider::RooCode));
        assert_eq!(scope.provider_id(), Some("roo-code"));
        assert_eq!(scope.response_label(), "roo-code");

        assert_eq!(ProviderScope::All.provider(), None);
        assert_eq!(ProviderScope::All.provider_id(), None);
        assert_eq!(ProviderScope::All.response_label(), "all");
    }

    #[test]
    fn provider_scope_rejects_unknown_ids_with_expected_values() {
        let error = ProviderScope::parse_optional(Some("open-code")).unwrap_err();
        assert_eq!(
            error,
            format!(
                "unknown session provider 'open-code' (expected {EXPECTED_MESSAGE_SEARCH_PROVIDER})"
            )
        );
    }
}
