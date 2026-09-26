//! Compact clone-token streams.
//!
//! A repository pass emits tens of millions of clone tokens and a generation
//! keeps every stream for its whole life. As one enum per token with two
//! string handles that cost 48 bytes a token plus an allocation per
//! identifier, and a rename stream repeated its conservative stream in full
//! to change a few percent of the texts. Here a token is one 32-bit code
//! naming its grammar kind by number, followed by a second code only when its
//! text differs from its kind; those texts share one arena per stream, and a
//! rename stream is its conservative stream plus the renamed positions.
//! Serialization writes the same token objects the wire and every digest have
//! always read, so no persisted byte depends on this layout.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ts_provider::{grammar_kind_id, grammar_kind_name};

/// One clone token, borrowed from the stream that holds it.
#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConservativeCloneTokenV1<'a> {
    StructureStart { syntax_kind: &'a str },
    Syntax { syntax_kind: &'a str, text: &'a str },
    StructureEnd { syntax_kind: &'a str },
}

impl<'a> ConservativeCloneTokenV1<'a> {
    #[must_use]
    pub const fn syntax_kind(self) -> &'a str {
        match self {
            Self::StructureStart { syntax_kind }
            | Self::Syntax { syntax_kind, .. }
            | Self::StructureEnd { syntax_kind } => syntax_kind,
        }
    }

    #[must_use]
    pub const fn text(self) -> Option<&'a str> {
        match self {
            Self::Syntax { text, .. } => Some(text),
            Self::StructureStart { .. } | Self::StructureEnd { .. } => None,
        }
    }
}

/// The wire token, read before it is folded into a stream.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OwnedCloneTokenV1 {
    StructureStart { syntax_kind: String },
    Syntax { syntax_kind: String, text: String },
    StructureEnd { syntax_kind: String },
}

impl OwnedCloneTokenV1 {
    fn as_token(&self) -> ConservativeCloneTokenV1<'_> {
        match self {
            Self::StructureStart { syntax_kind } => ConservativeCloneTokenV1::StructureStart {
                syntax_kind: syntax_kind.as_str(),
            },
            Self::Syntax { syntax_kind, text } => ConservativeCloneTokenV1::Syntax {
                syntax_kind: syntax_kind.as_str(),
                text: text.as_str(),
            },
            Self::StructureEnd { syntax_kind } => ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: syntax_kind.as_str(),
            },
        }
    }
}

/// Why a stream could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CloneTokenStreamErrorV1 {
    #[error("clone token stream exceeds its 32-bit code space")]
    CodeSpaceExhausted,
    #[error("clone rename position {position} is not a syntax token of its stream")]
    RenameNotSyntax { position: u32 },
    #[error("clone rename positions are not strictly ascending")]
    RenameUnordered,
}

const VARIANT_MASK: u32 = 0b11;
const VARIANT_START: u32 = 0;
const VARIANT_END: u32 = 1;
/// A syntax token whose text is its kind.
const VARIANT_SYNTAX_KIND_TEXT: u32 = 2;
/// A syntax token followed by the arena index of its text.
const VARIANT_SYNTAX_TEXT: u32 = 3;
/// The kind is a string of the stream's arena, not a grammar kind number.
const LOCAL_KIND: u32 = 0b100;
const KIND_SHIFT: u32 = 3;
const MAX_KIND_VALUE: u32 = u32::MAX >> KIND_SHIFT;

#[derive(Default)]
struct StringArenaBuilderV1 {
    bytes: String,
    ends: Vec<u32>,
}

impl StringArenaBuilderV1 {
    fn push(&mut self, value: &str) -> Result<u32, CloneTokenStreamErrorV1> {
        let index = u32::try_from(self.ends.len())
            .map_err(|_| CloneTokenStreamErrorV1::CodeSpaceExhausted)?;
        self.bytes.push_str(value);
        let end = u32::try_from(self.bytes.len())
            .map_err(|_| CloneTokenStreamErrorV1::CodeSpaceExhausted)?;
        self.ends.push(end);
        Ok(index)
    }

    fn finish(self) -> StringArenaV1 {
        StringArenaV1 {
            bytes: self.bytes.into_boxed_str(),
            ends: self.ends.into_boxed_slice(),
        }
    }
}

struct StringArenaV1 {
    bytes: Box<str>,
    ends: Box<[u32]>,
}

impl StringArenaV1 {
    fn get(&self, index: u32) -> Option<&str> {
        let index = usize::try_from(index).ok()?;
        let end = usize::try_from(*self.ends.get(index)?).ok()?;
        let start = match index.checked_sub(1) {
            Some(previous) => usize::try_from(*self.ends.get(previous)?).ok()?,
            None => 0,
        };
        self.bytes.get(start..end)
    }

    fn retained_bytes(&self) -> usize {
        self.bytes
            .len()
            .saturating_add(std::mem::size_of_val::<[u32]>(&self.ends))
    }
}

struct CloneTokenCodesV1 {
    len: usize,
    codes: Box<[u32]>,
    strings: StringArenaV1,
}

struct CloneTokenRenamesV1 {
    positions: Box<[u32]>,
    texts: StringArenaV1,
}

struct CloneTokenStreamBuilderV1 {
    len: u32,
    codes: Vec<u32>,
    strings: StringArenaBuilderV1,
}

impl CloneTokenStreamBuilderV1 {
    fn with_capacity(tokens: usize) -> Self {
        Self {
            len: 0,
            codes: Vec::with_capacity(tokens),
            strings: StringArenaBuilderV1::default(),
        }
    }

    fn push(&mut self, token: ConservativeCloneTokenV1<'_>) -> Result<(), CloneTokenStreamErrorV1> {
        let (variant, kind, text) = match token {
            ConservativeCloneTokenV1::StructureStart { syntax_kind } => {
                (VARIANT_START, syntax_kind, None)
            }
            ConservativeCloneTokenV1::StructureEnd { syntax_kind } => {
                (VARIANT_END, syntax_kind, None)
            }
            ConservativeCloneTokenV1::Syntax { syntax_kind, text } if text == syntax_kind => {
                (VARIANT_SYNTAX_KIND_TEXT, syntax_kind, None)
            }
            ConservativeCloneTokenV1::Syntax { syntax_kind, text } => {
                (VARIANT_SYNTAX_TEXT, syntax_kind, Some(text))
            }
        };
        let (kind, local) = match grammar_kind_id(kind) {
            Some(id) => (id, 0),
            None => (self.strings.push(kind)?, LOCAL_KIND),
        };
        if kind > MAX_KIND_VALUE {
            return Err(CloneTokenStreamErrorV1::CodeSpaceExhausted);
        }
        self.len = self
            .len
            .checked_add(1)
            .ok_or(CloneTokenStreamErrorV1::CodeSpaceExhausted)?;
        self.codes.push((kind << KIND_SHIFT) | local | variant);
        if let Some(text) = text {
            let text = self.strings.push(text)?;
            self.codes.push(text);
        }
        Ok(())
    }

    fn finish(self) -> CloneTokenStreamV1 {
        CloneTokenStreamV1 {
            tokens: Arc::new(CloneTokenCodesV1 {
                len: self.len as usize,
                codes: self.codes.into_boxed_slice(),
                strings: self.strings.finish(),
            }),
            renames: None,
        }
    }
}

/// An immutable clone-token stream. Clones share the stream.
#[derive(Clone)]
pub struct CloneTokenStreamV1 {
    tokens: Arc<CloneTokenCodesV1>,
    renames: Option<Arc<CloneTokenRenamesV1>>,
}

impl CloneTokenStreamV1 {
    #[must_use]
    pub fn empty() -> Self {
        CloneTokenStreamBuilderV1::with_capacity(0).finish()
    }

    /// Fold `tokens` into a stream.
    pub fn from_tokens<'t>(
        tokens: impl IntoIterator<Item = ConservativeCloneTokenV1<'t>>,
    ) -> Result<Self, CloneTokenStreamErrorV1> {
        let tokens = tokens.into_iter();
        let mut builder = CloneTokenStreamBuilderV1::with_capacity(tokens.size_hint().0);
        for token in tokens {
            builder.push(token)?;
        }
        Ok(builder.finish())
    }

    /// This stream with the syntax tokens at `renames` positions (strictly
    /// ascending) carrying the given texts instead. The result shares this
    /// stream's tokens and holds only the renamed positions.
    pub fn renamed<'t>(
        &self,
        renames: impl IntoIterator<Item = (u32, &'t str)>,
    ) -> Result<Self, CloneTokenStreamErrorV1> {
        if self.renames.is_some() {
            return Self::from_tokens(self.iter())?.renamed(renames);
        }
        let mut positions = Vec::new();
        let mut texts = StringArenaBuilderV1::default();
        let mut tokens = self.iter().enumerate();
        for (position, text) in renames {
            if positions.last().is_some_and(|last| *last >= position) {
                return Err(CloneTokenStreamErrorV1::RenameUnordered);
            }
            let is_syntax = tokens
                .by_ref()
                .find(|(index, _)| u32::try_from(*index).is_ok_and(|index| index == position))
                .is_some_and(|(_, token)| matches!(token, ConservativeCloneTokenV1::Syntax { .. }));
            if !is_syntax {
                return Err(CloneTokenStreamErrorV1::RenameNotSyntax { position });
            }
            positions.push(position);
            texts.push(text)?;
        }
        Ok(Self {
            tokens: Arc::clone(&self.tokens),
            renames: Some(Arc::new(CloneTokenRenamesV1 {
                positions: positions.into_boxed_slice(),
                texts: texts.finish(),
            })),
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.len == 0
    }

    #[must_use]
    pub fn iter(&self) -> CloneTokenIterV1<'_> {
        CloneTokenIterV1 {
            tokens: &self.tokens,
            renames: self.renames.as_deref(),
            code: 0,
            position: 0,
            rename: 0,
        }
    }

    /// The tokens at `range` as their own stream, or `None` when the range
    /// leaves the stream.
    pub fn slice(&self, range: Range<usize>) -> Result<Option<Self>, CloneTokenStreamErrorV1> {
        if range.start > range.end || range.end > self.len() {
            return Ok(None);
        }
        Self::from_tokens(self.iter().skip(range.start).take(range.len())).map(Some)
    }

    /// Whether `other` reads this stream's token codes, as a rename stream
    /// over its conservative stream does.
    #[must_use]
    pub fn shares_tokens_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tokens, &other.tokens)
    }

    /// Bytes this stream's token codes and texts hold.
    #[must_use]
    pub fn token_retained_bytes(&self) -> usize {
        std::mem::size_of::<CloneTokenCodesV1>()
            .saturating_add(std::mem::size_of_val::<[u32]>(&self.tokens.codes))
            .saturating_add(self.tokens.strings.retained_bytes())
    }

    /// Bytes this stream's renamed positions hold beyond the tokens it reads.
    #[must_use]
    pub fn rename_retained_bytes(&self) -> usize {
        self.renames.as_deref().map_or(0, |renames| {
            std::mem::size_of::<CloneTokenRenamesV1>()
                .saturating_add(std::mem::size_of_val::<[u32]>(&renames.positions))
                .saturating_add(renames.texts.retained_bytes())
        })
    }
}

impl Default for CloneTokenStreamV1 {
    fn default() -> Self {
        Self::empty()
    }
}

impl PartialEq for CloneTokenStreamV1 {
    fn eq(&self, other: &Self) -> bool {
        let same_renames = match (&self.renames, &other.renames) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        };
        (self.shares_tokens_with(other) && same_renames)
            || (self.len() == other.len() && self.iter().eq(other.iter()))
    }
}

impl Eq for CloneTokenStreamV1 {}

impl Hash for CloneTokenStreamV1 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.len());
        for token in self.iter() {
            token.hash(state);
        }
    }
}

impl fmt::Debug for CloneTokenStreamV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl Serialize for CloneTokenStreamV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for CloneTokenStreamV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StreamVisitor;

        impl<'de> Visitor<'de> for StreamVisitor {
            type Value = CloneTokenStreamV1;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence of clone tokens")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut builder =
                    CloneTokenStreamBuilderV1::with_capacity(sequence.size_hint().unwrap_or(0));
                while let Some(token) = sequence.next_element::<OwnedCloneTokenV1>()? {
                    builder
                        .push(token.as_token())
                        .map_err(serde::de::Error::custom)?;
                }
                Ok(builder.finish())
            }
        }

        deserializer.deserialize_seq(StreamVisitor)
    }
}

impl<'a> IntoIterator for &'a CloneTokenStreamV1 {
    type Item = ConservativeCloneTokenV1<'a>;
    type IntoIter = CloneTokenIterV1<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The tokens of one stream, in order.
#[derive(Clone)]
pub struct CloneTokenIterV1<'a> {
    tokens: &'a CloneTokenCodesV1,
    renames: Option<&'a CloneTokenRenamesV1>,
    code: usize,
    position: usize,
    rename: usize,
}

impl<'a> CloneTokenIterV1<'a> {
    fn next_code(&mut self) -> Option<u32> {
        let code = self.tokens.codes.get(self.code).copied()?;
        self.code += 1;
        Some(code)
    }

    /// The renamed text of the token at the current position. Positions are
    /// validated ascending syntax positions when the rename is built.
    fn renamed_text(&mut self, position: usize) -> Option<&'a str> {
        let renames = self.renames?;
        let next = usize::try_from(*renames.positions.get(self.rename)?).ok()?;
        if next != position {
            return None;
        }
        let text = renames.texts.get(u32::try_from(self.rename).ok()?)?;
        self.rename += 1;
        Some(text)
    }
}

impl<'a> Iterator for CloneTokenIterV1<'a> {
    type Item = ConservativeCloneTokenV1<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let code = self.next_code()?;
        let kind = if code & LOCAL_KIND == 0 {
            grammar_kind_name(code >> KIND_SHIFT)?
        } else {
            self.tokens.strings.get(code >> KIND_SHIFT)?
        };
        let position = self.position;
        self.position += 1;
        Some(match code & VARIANT_MASK {
            VARIANT_START => ConservativeCloneTokenV1::StructureStart { syntax_kind: kind },
            VARIANT_END => ConservativeCloneTokenV1::StructureEnd { syntax_kind: kind },
            VARIANT_SYNTAX_KIND_TEXT => ConservativeCloneTokenV1::Syntax {
                syntax_kind: kind,
                text: self.renamed_text(position).unwrap_or(kind),
            },
            _ => {
                let text = self.next_code()?;
                let text = self.tokens.strings.get(text)?;
                ConservativeCloneTokenV1::Syntax {
                    syntax_kind: kind,
                    text: self.renamed_text(position).unwrap_or(text),
                }
            }
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.tokens.len.saturating_sub(self.position);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CloneTokenIterV1<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(syntax_kind: &str) -> ConservativeCloneTokenV1<'_> {
        ConservativeCloneTokenV1::StructureStart { syntax_kind }
    }

    fn end(syntax_kind: &str) -> ConservativeCloneTokenV1<'_> {
        ConservativeCloneTokenV1::StructureEnd { syntax_kind }
    }

    fn syntax<'a>(syntax_kind: &'a str, text: &'a str) -> ConservativeCloneTokenV1<'a> {
        ConservativeCloneTokenV1::Syntax { syntax_kind, text }
    }

    #[test]
    fn a_stream_reads_back_its_tokens_and_serializes_the_wire_objects() {
        let tokens = [
            start("block"),
            syntax("identifier", "value"),
            syntax("(", "("),
            syntax("fixture_only_kind", "fixture_only_text"),
            end("block"),
        ];
        let stream = CloneTokenStreamV1::from_tokens(tokens).expect("stream");

        assert_eq!(stream.iter().collect::<Vec<_>>(), tokens);
        assert_eq!(stream.len(), 5);
        assert_eq!(
            serde_json::to_string(&stream).expect("serialize"),
            r#"[{"kind":"structure_start","syntax_kind":"block"},{"kind":"syntax","syntax_kind":"identifier","text":"value"},{"kind":"syntax","syntax_kind":"(","text":"("},{"kind":"syntax","syntax_kind":"fixture_only_kind","text":"fixture_only_text"},{"kind":"structure_end","syntax_kind":"block"}]"#
        );
        let decoded: CloneTokenStreamV1 =
            serde_json::from_str(&serde_json::to_string(&stream).expect("serialize"))
                .expect("deserialize");
        assert_eq!(decoded, stream);
        assert_eq!(decoded.iter().collect::<Vec<_>>(), tokens);
    }

    #[test]
    fn a_grammar_kind_costs_one_code_and_its_text_a_second_only_when_it_differs() {
        let stream = CloneTokenStreamV1::from_tokens([
            start("block"),
            syntax("(", "("),
            syntax("identifier", "value"),
            end("block"),
        ])
        .expect("stream");

        assert_eq!(stream.tokens.codes.len(), 5);
        assert_eq!(&*stream.tokens.strings.bytes, "value");
    }

    #[test]
    fn a_rename_shares_its_tokens_and_holds_only_renamed_positions() {
        let conservative = CloneTokenStreamV1::from_tokens([
            syntax("identifier", "input"),
            syntax("(", "("),
            syntax("identifier", "input"),
        ])
        .expect("stream");
        let renamed = conservative
            .renamed([(0, "$0"), (2, "$0")])
            .expect("rename");

        assert!(renamed.shares_tokens_with(&conservative));
        assert_eq!(
            renamed.iter().collect::<Vec<_>>(),
            [
                syntax("identifier", "$0"),
                syntax("(", "("),
                syntax("identifier", "$0")
            ]
        );
        assert_ne!(renamed, conservative);
        assert_eq!(
            renamed,
            CloneTokenStreamV1::from_tokens([
                syntax("identifier", "$0"),
                syntax("(", "("),
                syntax("identifier", "$0"),
            ])
            .expect("stream"),
            "equality is by content, not by layout"
        );
        assert_eq!(
            renamed.rename_retained_bytes(),
            std::mem::size_of::<CloneTokenRenamesV1>() + 8 + 4 + 8
        );
    }

    #[test]
    fn a_rename_refuses_positions_that_are_not_ascending_syntax_tokens() {
        let conservative =
            CloneTokenStreamV1::from_tokens([start("block"), syntax("identifier", "x")])
                .expect("stream");

        assert_eq!(
            conservative.renamed([(0, "$0")]).err(),
            Some(CloneTokenStreamErrorV1::RenameNotSyntax { position: 0 })
        );
        assert_eq!(
            conservative.renamed([(1, "$0"), (1, "$1")]).err(),
            Some(CloneTokenStreamErrorV1::RenameUnordered)
        );
        assert_eq!(
            conservative.renamed([(2, "$0")]).err(),
            Some(CloneTokenStreamErrorV1::RenameNotSyntax { position: 2 })
        );
    }

    #[test]
    fn a_slice_is_its_own_stream_and_an_outside_range_is_none() {
        let stream = CloneTokenStreamV1::from_tokens([
            start("block"),
            syntax("identifier", "a"),
            syntax("identifier", "b"),
            end("block"),
        ])
        .expect("stream")
        .renamed([(2, "$1")])
        .expect("rename");

        assert_eq!(
            stream
                .slice(1..3)
                .expect("slice")
                .expect("inside")
                .iter()
                .collect::<Vec<_>>(),
            [syntax("identifier", "a"), syntax("identifier", "$1")]
        );
        assert!(stream.slice(3..5).expect("slice").is_none());
    }
}
