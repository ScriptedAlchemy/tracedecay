use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

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

    /// Whether this provider emits the canonical observation contract used by
    /// host admission. Vibe remains transcript-only.
    pub const fn supports_host_admission(self) -> bool {
        !matches!(self, Self::Vibe)
    }

    /// Whether this provider's driver scans every destination store in one pass,
    /// so a per-destination catch-up loop must skip it once user ingestion ran.
    pub const fn scans_all_destinations(self) -> bool {
        matches!(self, Self::Hermes)
    }
}

pub const MESSAGE_SEARCH_PROVIDER_IDS: &[&str] = &[
    "all", "cursor", "claude", "codex", "vibe", "cline", "roo-code", "kilo", "kiro", "hermes",
];

pub const EXPECTED_MESSAGE_SEARCH_PROVIDER: &str =
    "all, cursor, claude, codex, vibe, cline, roo-code, kilo, kiro, or hermes";

/// Decodes the workspace path used by Kiro's `workspace-sessions` directory.
///
/// Kiro's native encoding retains `+`, substitutes `_` for `/`, and omits
/// padding. URL-safe `-` is accepted for compatibility with derived directory
/// names. The selected engine requires canonical padding and zero trailing
/// bits, so malformed names fail closed.
pub fn decode_kiro_workspace_path(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim_end_matches('_');
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace('-', "+").replace('_', "/");
    let decoded = if normalized.contains('=') {
        STANDARD.decode(normalized)
    } else {
        STANDARD_NO_PAD.decode(normalized)
    }
    .ok()?;
    let path = String::from_utf8(decoded).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

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
    use std::path::PathBuf;

    use super::{ProviderScope, SessionProvider, decode_kiro_workspace_path};

    #[test]
    fn provider_ids_round_trip() {
        for provider in SessionProvider::ALL {
            assert_eq!(SessionProvider::parse(provider.id()), Some(provider));
        }
    }

    #[test]
    fn provider_capabilities_match_runtime_boundaries() {
        for provider in SessionProvider::ALL {
            assert_eq!(
                provider.supports_host_admission(),
                provider != SessionProvider::Vibe,
                "{} host-admission capability",
                provider.id()
            );
            assert_eq!(
                provider.scans_all_destinations(),
                provider == SessionProvider::Hermes,
                "{} destination-scan capability",
                provider.id()
            );
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

    #[test]
    fn kiro_workspace_path_preserves_native_and_url_safe_encodings() {
        assert_eq!(
            decode_kiro_workspace_path("L3RtcC9raXJvLcO_L3g"),
            Some(PathBuf::from("/tmp/kiro-ÿ/x"))
        );
        assert_eq!(
            decode_kiro_workspace_path("L1VzZXJzL-a1i-ivlS9wcm9qZWN0"),
            Some(PathBuf::from("/Users/测试/project"))
        );
    }

    #[test]
    fn kiro_workspace_path_rejects_malformed_padding_and_trailing_bits() {
        for malformed in ["Zg=", "Zg===junk", "Zh", "_"] {
            assert_eq!(
                decode_kiro_workspace_path(malformed),
                None,
                "{malformed:?} must fail closed"
            );
        }
    }
}
