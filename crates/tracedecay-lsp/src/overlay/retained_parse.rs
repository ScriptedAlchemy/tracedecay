//! Session-local adapter around the canonical retained parsing leaf.

use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseError, ParseInputEdit, ParseLimits, ParseReport,
    RetainedParseDocument,
};

/// Parser state exposed with an ephemeral overlay snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlayParseState {
    Ready(ParseReport),
    Unavailable(OverlayParseUnavailable),
}

/// Typed reason an accepted text overlay has no current retained tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayParseUnavailable {
    UnsupportedLanguage,
    SourceTooLarge,
    InvalidEdit,
    IdentityMismatch,
    GrammarRejected,
    TimedOut,
    ParseFailed,
}

impl From<&ParseError> for OverlayParseUnavailable {
    fn from(error: &ParseError) -> Self {
        match error {
            ParseError::UnsupportedLanguage { .. } => Self::UnsupportedLanguage,
            ParseError::SourceTooLarge { .. } => Self::SourceTooLarge,
            ParseError::InvalidEdit { .. } => Self::InvalidEdit,
            ParseError::IdentityMismatch => Self::IdentityMismatch,
            ParseError::GrammarRejected { .. } => Self::GrammarRejected,
            ParseError::TimedOut { .. } => Self::TimedOut,
            ParseError::ParseFailed => Self::ParseFailed,
        }
    }
}

pub(super) struct RetainedOverlayParse {
    document: Option<RetainedParseDocument>,
    state: OverlayParseState,
}

impl RetainedOverlayParse {
    pub(super) fn open(identity: ParseDocumentIdentity, language_id: &str, source: &str) -> Self {
        match RetainedParseDocument::open(identity, language_id, source, ParseLimits::default()) {
            Ok((document, report)) => Self {
                document: Some(document),
                state: OverlayParseState::Ready(report),
            },
            Err(error) => Self {
                document: None,
                state: OverlayParseState::Unavailable((&error).into()),
            },
        }
    }

    pub(super) fn update(
        &mut self,
        next_identity: ParseDocumentIdentity,
        language_id: &str,
        edits: &[ParseInputEdit],
        source: &str,
        full_replacement: bool,
    ) {
        let result = if let Some(document) = self.document.as_mut() {
            let report = if full_replacement {
                document.replace(next_identity, source)
            } else {
                document.apply_edits(next_identity, edits, source)
            };
            report.map(|report| (None, report))
        } else {
            RetainedParseDocument::open(next_identity, language_id, source, ParseLimits::default())
                .map(|(document, report)| (Some(document), report))
        };
        match result {
            Ok((replacement, report)) => {
                if let Some(document) = replacement {
                    self.document = Some(document);
                }
                self.state = OverlayParseState::Ready(report);
            }
            Err(error) => {
                self.document = None;
                self.state = OverlayParseState::Unavailable((&error).into());
            }
        }
    }

    pub(super) fn state(&self) -> &OverlayParseState {
        &self.state
    }
}
