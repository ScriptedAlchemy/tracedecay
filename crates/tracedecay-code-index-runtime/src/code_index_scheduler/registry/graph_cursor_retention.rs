//! Cursor-bound retention of superseded graph generations.
//!
//! A graph cursor is pinned to the generation that minted it, but the only
//! thing that keeps a superseded generation's graph replay from being retired
//! is a live reader lease, and nothing held one between two pages. A publish
//! that landed between them let superseded-replay retirement collect the
//! pinned generation, and the continuation refused `Unavailable` (#1244).
//!
//! The serving owner a graph cursor was minted against is held here until
//! the cursor's authenticated expiry. Its graph store keeps the verified
//! generation lease alive, so retirement counts the generation as retained,
//! and a continuation resolves through the held owner instead of replaying
//! it from the journal. Holds lapse with the cursor; nothing here outlives
//! the mounted worktree that owns it.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{Mutex, PoisonError},
};

use tracedecay_domain::{CodeGenerationId, UtcMicros};

use super::super::LatestCodeTextGenerationV1;

struct GraphCursorHoldV1 {
    latest: LatestCodeTextGenerationV1,
    expires_at: UtcMicros,
}

#[derive(Default)]
pub struct GraphCursorRetentionV1 {
    holds: Mutex<BTreeMap<CodeGenerationId, GraphCursorHoldV1>>,
}

impl GraphCursorRetentionV1 {
    /// Hold `latest` for a cursor that stays valid until `expires_at`. A
    /// later cursor on the same generation only ever extends the hold.
    pub fn retain(
        &self,
        latest: &LatestCodeTextGenerationV1,
        expires_at: UtcMicros,
        now: UtcMicros,
    ) {
        if expires_at <= now {
            return;
        }
        let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        holds.retain(|_, hold| hold.expires_at > now);
        match holds.entry(latest.metadata().manifest().generation_id.clone()) {
            Entry::Occupied(mut occupied) => {
                let hold = occupied.get_mut();
                hold.expires_at = hold.expires_at.max(expires_at);
            }
            Entry::Vacant(vacant) => {
                vacant.insert(GraphCursorHoldV1 {
                    latest: latest.clone(),
                    expires_at,
                });
            }
        }
    }

    /// The held owner of `generation`, while an unexpired cursor pins it.
    pub fn held(
        &self,
        generation: &CodeGenerationId,
        now: UtcMicros,
    ) -> Option<LatestCodeTextGenerationV1> {
        let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        holds.retain(|_, hold| hold.expires_at > now);
        holds.get(generation).map(|hold| hold.latest.clone())
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn held_until(&self, generation: &CodeGenerationId) -> Option<UtcMicros> {
        self.holds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(generation)
            .map(|hold| hold.expires_at)
    }
}
