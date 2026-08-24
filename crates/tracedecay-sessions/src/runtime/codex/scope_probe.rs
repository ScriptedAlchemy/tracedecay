//! Pre-decode scope probe for Codex rollout frames.
//!
//! A rollout discovered under one project scope is usually owned by a
//! *different* project: the profile sweep hands every `~/.codex/sessions`
//! rollout to a scope that rejects nearly all of it. Those frames were read,
//! decoded into a `serde_json::Value` tree, and walked twice by the structural
//! validator before anything consulted their scope, because the scope test
//! lives inside the normalizer the parser calls last.
//!
//! The scope of a Codex frame is not a property of the frame: it is a property
//! of the session cwd the frame is observed under. Only two record kinds can
//! move that cwd, and [`session_meta_from_record`] and
//! [`turn_context_from_record`] both gate on exactly one thing — the top-level
//! `type` string. Every other frame leaves the cwd exactly as it found it, so
//! the verdict for such a frame is already decided before it is decoded.
//!
//! [`probe_codex_frame`] proves that property by tokenizing the frame and
//! discarding it, allocating nothing but the `type` value itself. It answers
//! [`CodexFrameScopeProbeV1::Inert`] only when it can also prove the frame
//! would clear every structural gate the authoritative parser applies before
//! reaching the normalizer, so an early rejection can never claim a different
//! coverage reason than the parser would have recorded.
//!
//! [`session_meta_from_record`]: super::meta::session_meta_from_record
//! [`turn_context_from_record`]: super::meta::turn_context_from_record

use std::cell::Cell;

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use tracedecay_domain::ObservationSourceRangeV1;
use tracedecay_runtime_core::privacy::ParseLimits;

/// The top-level `type` values that let a Codex record move the session cwd.
const SESSION_META_TYPE: &str = "session_meta";
const TURN_CONTEXT_TYPE: &str = "turn_context";

/// What a raw Codex frame can be proved to be without decoding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexFrameScopeProbeV1 {
    /// The frame is a JSON object within every structural limit the
    /// authoritative parser enforces, and its top-level `type` is neither
    /// `session_meta` nor `turn_context`. Such a frame cannot change the
    /// session cwd, so the scope verdict computed from the current cwd is the
    /// verdict the authoritative parser would reach for it.
    Inert,
    /// Nothing was proved. The frame may carry context, or it violates a gate
    /// whose own coverage reason the authoritative parser owns. Decode it.
    Undecided,
}

/// Prove — or fail to prove — that `record` cannot move the session cwd.
///
/// `range` is the frame's file-byte range. Codex admits every rollout frame in
/// the [`FileBytes`] ordering domain, which is the domain whose range/length
/// agreement the authoritative parser checks, so that check is replicated here
/// rather than assumed.
///
/// [`FileBytes`]: tracedecay_domain::ObservationOrderingDomainV1::FileBytes
pub(super) fn probe_codex_frame(
    record: &[u8],
    range: ObservationSourceRangeV1,
) -> CodexFrameScopeProbeV1 {
    let limits = ParseLimits::default_policy();
    // The three frame gates the authoritative parser applies before it decodes
    // anything. Each one has its own coverage reason, so failing any of them
    // must reach the parser rather than be answered here.
    if record.is_empty() || record.len() > limits.record_bytes {
        return CodexFrameScopeProbeV1::Undecided;
    }
    if u64::try_from(record.len()).ok() != Some(range.end().saturating_sub(range.start())) {
        return CodexFrameScopeProbeV1::Undecided;
    }

    let budget = Cell::new(Budget {
        values: 0,
        value_limit: limits.values,
        depth_limit: limits.depth,
    });
    let mut deserializer = serde_json::Deserializer::from_slice(record);
    let Ok(top_level_type) = (RootSeed { budget: &budget }).deserialize(&mut deserializer) else {
        return CodexFrameScopeProbeV1::Undecided;
    };
    // `serde_json::from_slice` rejects trailing content after the value; a
    // probe that did not would call a frame inert that the parser calls
    // malformed.
    if deserializer.end().is_err() {
        return CodexFrameScopeProbeV1::Undecided;
    }
    match top_level_type {
        TopLevelType::SessionMeta | TopLevelType::TurnContext => CodexFrameScopeProbeV1::Undecided,
        TopLevelType::Other => CodexFrameScopeProbeV1::Inert,
    }
}

/// The top-level `type` of one record, reduced to the only distinction that
/// changes scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopLevelType {
    SessionMeta,
    TurnContext,
    /// Absent, not a string, or any other string. Both context classifiers
    /// read this field with `Value::as_str`, so a non-string `type` is as
    /// inert as a missing one.
    Other,
}

impl TopLevelType {
    fn classify(value: &str) -> Self {
        match value {
            SESSION_META_TYPE => Self::SessionMeta,
            TURN_CONTEXT_TYPE => Self::TurnContext,
            _ => Self::Other,
        }
    }
}

/// The structural budget the authoritative validator enforces, tracked while
/// the frame is skipped rather than after it is materialized.
///
/// The count can exceed the validator's own for one input shape: duplicate
/// object keys collapse in a `serde_json::Value` map and are counted once
/// there, while a streaming walk sees both. That direction is safe — it can
/// only turn `Inert` into `Undecided`, which costs a decode and changes no
/// verdict.
#[derive(Clone, Copy)]
struct Budget {
    values: usize,
    value_limit: usize,
    depth_limit: usize,
}

/// Charge one value at `depth`, failing the walk when either limit is passed.
///
/// Both limits are reported as a plain deserialize error because every probe
/// failure has the same meaning: the authoritative parser decides.
fn charge<E: de::Error>(budget: &Cell<Budget>, depth: usize) -> Result<(), E> {
    let mut current = budget.get();
    current.values = current.values.saturating_add(1);
    if current.values > current.value_limit || depth > current.depth_limit {
        return Err(de::Error::custom("codex frame exceeds a structural limit"));
    }
    budget.set(current);
    Ok(())
}

/// Charge an already-materialized subtree, matching the validator's traversal.
///
/// Only the `type` value is materialized, and only so its string form can be
/// read the same way the context classifiers read it.
fn charge_value<E: de::Error>(budget: &Cell<Budget>, value: &Value, depth: usize) -> Result<(), E> {
    let mut stack = vec![(value, depth)];
    while let Some((current, current_depth)) = stack.pop() {
        charge::<E>(budget, current_depth)?;
        match current {
            Value::Object(fields) => stack.extend(
                fields
                    .values()
                    .map(|child| (child, current_depth.saturating_add(1))),
            ),
            Value::Array(items) => stack.extend(
                items
                    .iter()
                    .map(|child| (child, current_depth.saturating_add(1))),
            ),
            _ => {}
        }
    }
    Ok(())
}

/// The root of one frame: an object, or nothing this probe can speak for.
struct RootSeed<'budget> {
    budget: &'budget Cell<Budget>,
}

impl<'de> DeserializeSeed<'de> for RootSeed<'_> {
    type Value = TopLevelType;

    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(RootVisitor {
            budget: self.budget,
        })
    }
}

struct RootVisitor<'budget> {
    budget: &'budget Cell<Budget>,
}

impl<'de> Visitor<'de> for RootVisitor<'_> {
    type Value = TopLevelType;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a Codex rollout record object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        charge::<A::Error>(self.budget, 1)?;
        // A duplicated `type` resolves to its last occurrence, the same way a
        // `serde_json::Value` map does.
        let mut top_level_type = TopLevelType::Other;
        while let Some(is_type) = map.next_key_seed(RootKeySeed)? {
            if is_type {
                let value: Value = map.next_value()?;
                charge_value::<A::Error>(self.budget, &value, 2)?;
                top_level_type = value
                    .as_str()
                    .map_or(TopLevelType::Other, TopLevelType::classify);
            } else {
                map.next_value_seed(CountingSeed {
                    depth: 2,
                    budget: self.budget,
                })?;
            }
        }
        Ok(top_level_type)
    }
}

/// A root key, reduced to whether it is `type`, so no key is ever allocated.
struct RootKeySeed;

impl<'de> DeserializeSeed<'de> for RootKeySeed {
    type Value = bool;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<bool, D::Error> {
        deserializer.deserialize_str(RootKeyVisitor)
    }
}

struct RootKeyVisitor;

impl Visitor<'_> for RootKeyVisitor {
    type Value = bool;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a record field name")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<bool, E> {
        Ok(value == "type")
    }
}

/// One value that is counted and discarded rather than built.
#[derive(Clone, Copy)]
struct CountingSeed<'budget> {
    depth: usize,
    budget: &'budget Cell<Budget>,
}

impl<'de> DeserializeSeed<'de> for CountingSeed<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for CountingSeed<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        charge(self.budget, self.depth)
    }

    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_newtype_struct<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        charge::<A::Error>(self.budget, self.depth)?;
        let element = CountingSeed {
            depth: self.depth.saturating_add(1),
            budget: self.budget,
        };
        while seq.next_element_seed(element)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        charge::<A::Error>(self.budget, self.depth)?;
        let value = CountingSeed {
            depth: self.depth.saturating_add(1),
            budget: self.budget,
        };
        // Keys are not values: the structural validator walks a decoded map's
        // values only, so counting a key here would diverge from it.
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value_seed(value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
