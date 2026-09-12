use super::*;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, RetrievalGrainV1, SessionId, SymbolOccurrenceId, TemporalModeV1,
    canonical_sha256, sha256_hex_suffix,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_session_temporal_store::GlobalDbCursorKeyProvider;
use tracedecay_temporal_query::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::ports::SessionCursorAuthenticator;
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

const PR_CONTEXT_CURSOR_SESSION: &str = "session.daemon.pr-context";

/// Canonical identity of the checkout a cursor was minted for.
///
/// This is `TraceDecay`'s own resolved project/repository/worktree identity,
/// not a locally derived name: the same authority every other scoped read
/// binds to, carried verbatim so a cursor cannot travel between projects.
///
/// It is deliberately the three fields
/// [`tracedecay_contracts::ResolvedScope::identifies_same_checkout`] compares,
/// and not the scope digest. The digest also covers the git reference the
/// scope was resolved under, which changes on every ordinary branch switch —
/// binding cursors to it would invalidate in-flight pagination whenever HEAD
/// moved, while proving nothing about which checkout is being read.
#[derive(Clone, Copy, Serialize)]
pub(super) struct PrContextCursorScope<'a> {
    pub project_id: &'a str,
    pub repository_id: &'a str,
    pub worktree_id: &'a str,
}

impl<'a> PrContextCursorScope<'a> {
    fn from_resolved(scope: &'a tracedecay_contracts::ResolvedScope) -> Self {
        Self {
            project_id: scope.project_id.as_str(),
            repository_id: scope.repository_id.as_str(),
            worktree_id: scope.worktree_id.as_str(),
        }
    }
}

/// The logical shard the store that signs this cursor was opened for.
///
/// Cursor denial is a store-level outcome: a cursor is "not yours" when it was
/// minted against another store, and that store's own registered shard is what
/// names it. Carrying the shard makes the denial hold even between two stores
/// that happen to serve the same checkout.
///
/// The shard's logical scope is carried whole through its canonical
/// serialization rather than reduced to the project it mentions. Reducing it
/// would give a project's `Project`, `ProjectSessions`, and `Code` shards one
/// identity, so cursors minted against different stores for one project would
/// verify interchangeably.
#[derive(Clone, Copy, Serialize)]
pub(super) struct PrContextCursorStore<'a> {
    pub brain_id: &'a str,
    pub profile_id: &'a str,
    pub scope: &'a tracedecay_store::StoreShardScopeV1,
}

#[derive(Serialize)]
pub(super) struct PrContextCursorBinding<'a> {
    pub protocol: &'static str,
    /// The admitted checkout, when the daemon's route resolved one. A cursor
    /// minted under one project's scope cannot verify under another's.
    pub scope: Option<PrContextCursorScope<'a>>,
    /// The registered store that signs and verifies this cursor.
    pub store: Option<PrContextCursorStore<'a>>,
    /// The worktree root exactly as the filesystem stores it.
    ///
    /// `Path::to_string_lossy` maps every unpaired byte onto the same
    /// replacement character, so two genuinely different non-UTF-8 checkouts
    /// would flatten to one identical binding string and mint interchangeable
    /// cursors. The native OS bytes are the filesystem's own identity and
    /// distinguish those roots.
    pub project_root: &'a [u8],
    pub base_oid: &'a str,
    pub head_oid: &'a str,
    pub merge_base: &'a str,
    pub graph_generation: &'a str,
    pub maximum_symbols: usize,
    pub changes: &'a [GitFileChange],
}

impl<'a> PrContextCursorBinding<'a> {
    /// Binds one PR comparison to the checkout this call is admitted for.
    pub fn new(
        ctx: &'a McpToolContext<'_>,
        project_root: &'a [u8],
        comparison: PrContextCursorComparison<'a>,
    ) -> Self {
        Self {
            protocol: "tracedecay.pr-context.cursor.v2",
            scope: Some(PrContextCursorScope::from_resolved(ctx.admitted_scope())),
            store: ctx
                .authorized_project_session_db()
                .map(|(lease, _)| PrContextCursorStore::from_shard(&lease.binding().shard_id)),
            project_root,
            base_oid: comparison.base_oid,
            head_oid: comparison.head_oid,
            merge_base: comparison.merge_base,
            graph_generation: comparison.graph_generation,
            maximum_symbols: comparison.maximum_symbols,
            changes: comparison.changes,
        }
    }

    /// Digest of the checkout this cursor belongs to, and nothing else.
    ///
    /// A cursor whose identity digest differs is another checkout's, however
    /// similar its comparison looks.
    fn identity_digest(&self) -> Result<ManifestDigest> {
        canonical_sha256(&(
            "tracedecay.pr-context.cursor.identity.v1",
            self.protocol,
            &self.scope,
            &self.store,
            self.project_root,
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to bind PR context cursor identity: {error}"),
        })
    }

    /// Digest of the comparison this page was frozen against.
    fn request_digest(&self) -> Result<ManifestDigest> {
        canonical_sha256(self).map_err(|error| TraceDecayError::Config {
            message: format!("failed to bind PR context cursor: {error}"),
        })
    }
}

impl<'a> PrContextCursorStore<'a> {
    fn from_shard(shard: &'a tracedecay_store::StoreShardIdV1) -> Self {
        Self {
            brain_id: shard.brain_id.as_str(),
            profile_id: shard.profile_id.as_str(),
            scope: &shard.scope,
        }
    }
}

/// The comparison a page of PR context is frozen against.
#[derive(Clone, Copy)]
pub(super) struct PrContextCursorComparison<'a> {
    pub base_oid: &'a str,
    pub head_oid: &'a str,
    pub merge_base: &'a str,
    pub graph_generation: &'a str,
    pub maximum_symbols: usize,
    pub changes: &'a [GitFileChange],
}

/// Why an offered PR-context cursor cannot be honored.
///
/// The distinction is the caller's: a stale cursor means "restart this
/// pagination", while a denied one means "this cursor is not yours". Flattening
/// both into one opaque config error hides an attempted cross-scope read.
fn pr_context_cursor_refusal(error: &CursorError) -> TraceDecayError {
    let (reason_code, detail) = match error {
        // Authentication and binding failures: the cursor verifies as some
        // other request's, or as nobody's. Either way this request may not
        // continue from it.
        CursorError::Tampered
        | CursorError::KeyIdMismatch
        | CursorError::KeyVersionMismatch
        | CursorError::UnknownOrExpiredKey
        | CursorError::InvalidKeyMaterial
        | CursorError::WrongAccess
        | CursorError::RootMismatch
        | CursorError::SessionMismatch => (
            "pr_context_cursor_denied",
            "PR context cursor was issued for a different project, store, or worktree root",
        ),
        // Everything else means the snapshot this cursor froze has moved on,
        // so the page set it names no longer exists.
        CursorError::WrongRequest
        | CursorError::Malformed
        | CursorError::Expired
        | CursorError::KeyUnavailable
        | CursorError::FilterMismatch
        | CursorError::TemporalModeMismatch
        | CursorError::GrainMismatch
        | CursorError::SchemaMismatch
        | CursorError::RankingMismatch
        | CursorError::ConfigurationMismatch
        | CursorError::GenerationMismatch
        | CursorError::ParticipantManifestMismatch
        | CursorError::EpochMismatch
        | CursorError::CandidateCohortMismatch
        | CursorError::SourceWatermarkMismatch
        | CursorError::ProjectionWatermarkMismatch
        | CursorError::IndexWatermarkMismatch
        | CursorError::SummaryWatermarkMismatch
        | CursorError::SortKeyMismatch => (
            "pr_context_cursor_invalid",
            "PR context cursor no longer matches this comparison; restart pagination",
        ),
    };
    // Neither outcome is retryable with the same cursor: a denied cursor never
    // becomes this request's, and a stale one needs a fresh first page.
    TraceDecayError::project_route(reason_code, false, format!("{detail}: {error}"))
}

#[derive(Serialize, Deserialize)]
struct PrContextCursorKey<'a> {
    symbol_occurrence_id: &'a str,
    impact_nodes_admitted: usize,
    direct_call_edges_admitted: usize,
    impact_bytes_admitted: usize,
}

#[derive(Debug)]
pub(super) struct PrContextCursorPosition {
    pub after: SymbolOccurrenceId,
    pub impact_nodes_admitted: usize,
    pub direct_call_edges_admitted: usize,
    pub impact_bytes_admitted: usize,
}

/// Opens the cursor authority for this call's admitted project store.
///
/// The signing key is the store's own pre-provisioned cursor key, so a cursor
/// minted here can only be verified by the same store — that is what keeps a
/// foreign store's cursor from continuing this pagination. Attached means
/// admitted; an absent lease is the typed denied state.
#[hotpath::measure(label = "mcp.git.cursor.authority")]
pub(super) async fn pr_context_cursor_authority(
    ctx: &McpToolContext<'_>,
    binding: &PrContextCursorBinding<'_>,
) -> Result<(TemporalExecutionSnapshot, GlobalDbCursorKeyProvider)> {
    let Some((session_db, authorization)) = ctx.authorized_project_session_db() else {
        // Attached means admitted; absent is the typed denied state.
        return Err(TraceDecayError::project_route(
            "pr_context_cursor_denied",
            false,
            "this request is not authorized to read the admitted project session store",
        ));
    };
    let session_db: &RegisteredGlobalDb = session_db;
    let authenticator = hotpath::future!(
        session_db.load_preprovisioned_session_cursor_key_provider_result(),
        label = "mcp.git.cursor.key_provider"
    )
    .await
    .map_err(|error| {
        TraceDecayError::project_route(
            "pr_context_cursor_authority_unavailable",
            true,
            format!("pre-provisioned PR context cursor key is unavailable: {error}"),
        )
    })?;
    let key = authenticator.active_key_ref().clone();
    let snapshot = pr_context_cursor_snapshot(binding, key, authorization)?;
    Ok((snapshot, authenticator))
}

/// Binds one PR comparison into the snapshot every cursor authenticates
/// against.
///
/// Identity and comparison are hashed into *separate* bindings on purpose. The
/// root and access digests carry only the admitted scope and the byte-exact
/// worktree root, so `verify_cursor` reports a cursor from another project or
/// checkout as a root/access mismatch; the request digest carries the frozen
/// comparison, so a comparison that moved on reports as a changed request
/// instead. Collapsing both into one digest makes those two outcomes
/// indistinguishable, and the caller cannot tell "restart pagination" from
/// "this cursor is not yours".
fn pr_context_cursor_snapshot(
    binding: &PrContextCursorBinding<'_>,
    key: tracedecay_domain::SignedCursorKeyRefV1,
    authorization: ValidatedAuthorization,
) -> Result<TemporalExecutionSnapshot> {
    let (identity_digest, digest) = hotpath::measure_block!(
        "mcp.git.cursor.binding_digest",
        (binding.identity_digest()?, binding.request_digest()?)
    );
    let graph_digest = canonical_sha256(&(
        "tracedecay.pr-context.graph-generation.v1",
        binding.graph_generation,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to bind PR context graph generation: {error}"),
    })?;
    let graph_generation_hex = sha256_hex_suffix(graph_digest.as_str())
        .and_then(|hex| hex.get(..16))
        .ok_or_else(|| TraceDecayError::Config {
            message: "invalid PR context graph generation digest".to_owned(),
        })?;
    let graph_generation = u64::from_str_radix(graph_generation_hex, 16)
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid PR context graph generation watermark: {error}"),
        })?
        .max(1);
    let request = TemporalSnapshotRequest::new(
        SessionId::new(PR_CONTEXT_CURSOR_SESSION).map_err(|error| TraceDecayError::Config {
            message: format!("invalid PR context cursor session: {error}"),
        })?,
        identity_digest.as_str(),
        digest.as_str(),
        identity_digest.as_str(),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid PR context cursor binding: {error}"),
    })?;
    let configuration_digest = BindingDigest::new("configuration_digest", digest.as_str())
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid PR context cursor configuration: {error}"),
        })?;
    TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation: graph_generation,
            source: 1,
            projection: 1,
            index: 1,
            summary: 1,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest,
        },
        Some(key),
        authorization,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid PR context cursor snapshot: {error}"),
    })
}

#[hotpath::measure(label = "mcp.git.cursor.decode")]
pub(super) fn decode_pr_context_cursor(
    encoded: &str,
    snapshot: &TemporalExecutionSnapshot,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
) -> Result<PrContextCursorPosition> {
    let sort_key = verify_cursor(encoded, snapshot, authenticator)
        .map_err(|error| pr_context_cursor_refusal(&error))?;
    let key: PrContextCursorKey<'_> =
        serde_json::from_str(&sort_key.stable_id).map_err(|_| TraceDecayError::Config {
            message: "invalid PR context cursor key".to_owned(),
        })?;
    Ok(PrContextCursorPosition {
        after: SymbolOccurrenceId::new(key.symbol_occurrence_id.to_owned()).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid PR context symbol cursor: {error}"),
            }
        })?,
        impact_nodes_admitted: key.impact_nodes_admitted,
        direct_call_edges_admitted: key.direct_call_edges_admitted,
        impact_bytes_admitted: key.impact_bytes_admitted,
    })
}

#[hotpath::measure(label = "mcp.git.cursor.encode")]
pub(super) fn encode_pr_context_cursor(
    after: &SymbolOccurrenceId,
    impact_nodes_admitted: usize,
    direct_call_edges_admitted: usize,
    impact_bytes_admitted: usize,
    snapshot: &TemporalExecutionSnapshot,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
) -> Result<String> {
    let stable_id = serde_json::to_string(&PrContextCursorKey {
        symbol_occurrence_id: after.as_str(),
        impact_nodes_admitted,
        direct_call_edges_admitted,
        impact_bytes_admitted,
    })
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to encode PR context cursor key: {error}"),
    })?;
    encode_cursor(
        snapshot,
        &StableSortKey {
            normalized_score_micros: 0,
            knowledge_at_micros: 0,
            stable_id,
        },
        authenticator,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to issue PR context cursor: {error}"),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt as _;

    use super::*;
    use tracedecay_domain::{SessionCursorKeyIdV1, SessionCursorVersionV1, SignedCursorKeyRefV1};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
    use tracedecay_temporal_query::ports::InMemoryCursorAuthenticator;

    fn cursor_key() -> SignedCursorKeyRefV1 {
        SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new("key.pr-context.fixture").expect("key id"),
            version: SessionCursorVersionV1::new(1).expect("key version"),
        }
    }

    fn authenticator() -> InMemoryCursorAuthenticator {
        InMemoryCursorAuthenticator::new(cursor_key(), vec![7_u8; 32]).expect("in-memory key")
    }

    fn binding_for<'a>(
        root: &'a [u8],
        scope: Option<PrContextCursorScope<'a>>,
        changes: &'a [GitFileChange],
    ) -> PrContextCursorBinding<'a> {
        binding_bound_to(root, scope, None, changes)
    }

    /// The logical shard a registered project session store reports.
    fn session_shard(project: &str) -> tracedecay_store::StoreShardScopeV1 {
        tracedecay_store::StoreShardScopeV1::ProjectSessions {
            project_id: tracedecay_domain::ProjectId::new(project).expect("project id"),
        }
    }

    fn binding_bound_to<'a>(
        root: &'a [u8],
        scope: Option<PrContextCursorScope<'a>>,
        store: Option<PrContextCursorStore<'a>>,
        changes: &'a [GitFileChange],
    ) -> PrContextCursorBinding<'a> {
        PrContextCursorBinding {
            protocol: "tracedecay.pr-context.cursor.v2",
            scope,
            store,
            project_root: root,
            base_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            head_oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            merge_base: "cccccccccccccccccccccccccccccccccccccccc",
            graph_generation: "generation.pr-context.fixture",
            maximum_symbols: 25,
            changes,
        }
    }

    fn snapshot_for(binding: &PrContextCursorBinding<'_>) -> TemporalExecutionSnapshot {
        pr_context_cursor_snapshot(binding, cursor_key(), ValidatedAuthorization::Authorized)
            .expect("snapshot binds")
    }

    fn position() -> (SymbolOccurrenceId, usize, usize, usize) {
        (
            SymbolOccurrenceId::new("occurrence.pr-context.fixture".to_owned())
                .expect("occurrence id"),
            11,
            22,
            33,
        )
    }

    /// A real resolved scope, minted through the same contract every admitted
    /// read binds to, so these tests exercise canonical identity rather than a
    /// hand-rolled stand-in.
    fn resolved(project: &str, reference: Option<&str>) -> tracedecay_contracts::ResolvedScope {
        tracedecay_contracts::ResolvedScope::new(
            tracedecay_domain::ProjectId::new(project.to_owned()).expect("project id"),
            tracedecay_domain::RepositoryId::new("repository.pr-context".to_owned())
                .expect("repository id"),
            tracedecay_domain::WorktreeId::new("worktree.pr-context".to_owned())
                .expect("worktree id"),
            reference.map(|value| tracedecay_domain::RefId::new(value).expect("reference")),
        )
        .expect("resolved scope")
    }

    /// A cursor issued for one comparison must decode back to the exact page
    /// position it froze, or pagination silently restarts or skips symbols.
    #[test]
    fn a_cursor_round_trips_to_its_own_page_position() {
        let changes = Vec::new();
        let scope = resolved("project.a", None);
        let binding = binding_for(
            b"/projects/round-trip",
            Some(PrContextCursorScope::from_resolved(&scope)),
            &changes,
        );
        let snapshot = snapshot_for(&binding);
        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &snapshot, &authenticator)
                .expect("cursor issues");
        let decoded = decode_pr_context_cursor(&encoded, &snapshot, &authenticator)
            .expect("its own cursor decodes");

        assert_eq!(decoded.after.as_str(), after.as_str());
        assert_eq!(decoded.impact_nodes_admitted, nodes);
        assert_eq!(decoded.direct_call_edges_admitted, edges);
        assert_eq!(decoded.impact_bytes_admitted, bytes);
    }

    /// Two worktree roots that differ only in bytes no UTF-8 string can
    /// represent must not share a cursor identity.
    ///
    /// `Path::to_string_lossy` maps every unpaired byte onto U+FFFD, so both
    /// roots below collapse to the same string; a cursor bound to that string
    /// would verify against either checkout.
    #[cfg(unix)]
    #[test]
    fn distinct_non_utf8_roots_cannot_share_a_cursor() {
        let left = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/projects/a\xff"));
        let right = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/projects/a\xfe"));
        assert_eq!(
            left.to_string_lossy(),
            right.to_string_lossy(),
            "fixture must be a pair that a lossy conversion would merge"
        );

        let changes = Vec::new();
        let left_bytes =
            tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(left.as_os_str());
        let right_bytes =
            tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(right.as_os_str());
        let left_snapshot = snapshot_for(&binding_for(&left_bytes, None, &changes));
        let right_snapshot = snapshot_for(&binding_for(&right_bytes, None, &changes));
        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &left_snapshot, &authenticator)
                .expect("cursor issues");

        let refusal = decode_pr_context_cursor(&encoded, &right_snapshot, &authenticator)
            .expect_err("a cursor from a different root must not decode");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_denied"),
            "got {refusal}"
        );
    }

    /// A cursor minted under one project's admitted scope must be denied under
    /// another's, even when the root and the compared commits are identical.
    #[test]
    fn a_cursor_from_a_foreign_project_scope_is_denied() {
        let changes = Vec::new();
        let root = b"/projects/shared";
        let my_scope = resolved("project.mine", None);
        let their_scope = resolved("project.theirs", None);
        let mine = snapshot_for(&binding_for(
            root,
            Some(PrContextCursorScope::from_resolved(&my_scope)),
            &changes,
        ));
        let theirs = snapshot_for(&binding_for(
            root,
            Some(PrContextCursorScope::from_resolved(&their_scope)),
            &changes,
        ));
        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &theirs, &authenticator)
                .expect("cursor issues");

        let refusal = decode_pr_context_cursor(&encoded, &mine, &authenticator)
            .expect_err("a foreign project's cursor must not decode");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_denied"),
            "got {refusal}"
        );
    }

    /// A cursor signed by a different store's key fails authentication, which
    /// is a denial rather than a stale page: the store, not the comparison,
    /// is what does not match.
    #[test]
    fn a_cursor_from_a_foreign_store_key_is_denied() {
        let changes = Vec::new();
        let scope = resolved("project.a", None);
        let binding = binding_for(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&scope)),
            &changes,
        );
        let snapshot = snapshot_for(&binding);
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &snapshot, &authenticator())
                .expect("cursor issues");

        let foreign_store =
            InMemoryCursorAuthenticator::new(cursor_key(), vec![9_u8; 32]).expect("foreign key");
        let refusal = decode_pr_context_cursor(&encoded, &snapshot, &foreign_store)
            .expect_err("a foreign store's key must not verify");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_denied"),
            "got {refusal}"
        );
    }

    /// A comparison that moved on is a stale cursor, not a denied one: the
    /// caller should restart pagination rather than be told the cursor is
    /// someone else's.
    #[test]
    fn a_cursor_from_a_moved_comparison_is_invalid() {
        let changes = vec![GitFileChange {
            path: "src/lib.rs".to_owned(),
            status: "modified",
        }];
        let empty = Vec::new();
        let scope = resolved("project.a", None);
        let before = snapshot_for(&binding_for(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&scope)),
            &empty,
        ));
        let after_change = snapshot_for(&binding_for(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&scope)),
            &changes,
        ));
        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &before, &authenticator)
                .expect("cursor issues");

        let refusal = decode_pr_context_cursor(&encoded, &after_change, &authenticator)
            .expect_err("a moved comparison must refuse its old cursor");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_invalid"),
            "got {refusal}"
        );
    }

    /// Switching branches does not move the checkout, so a page opened on one
    /// branch must continue on another. Only the reference differs between the
    /// two scopes below, and the reference-sensitive `scope_digest` differs
    /// with it — binding cursor identity to that digest would break pagination
    /// on every ordinary branch switch.
    #[test]
    fn a_cursor_survives_a_branch_switch_on_the_same_checkout() {
        let changes = Vec::new();
        let registered = resolved("project.a", Some("refs/heads/main"));
        let switched = resolved("project.a", Some("refs/heads/feature"));
        assert_ne!(
            registered.scope_digest, switched.scope_digest,
            "fixture must differ in the reference-sensitive digest"
        );
        assert!(registered.identifies_same_checkout(&switched));

        let opened = snapshot_for(&binding_for(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&registered)),
            &changes,
        ));
        let continued = snapshot_for(&binding_for(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&switched)),
            &changes,
        ));
        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &opened, &authenticator)
                .expect("cursor issues");

        let decoded = decode_pr_context_cursor(&encoded, &continued, &authenticator)
            .expect("the same checkout on another branch must continue its own pagination");
        assert_eq!(decoded.after.as_str(), after.as_str());
    }

    /// An absent session-store lease is the typed denied state: the first
    /// authorized-only cursor read refuses rather than degrading into a
    /// missing capability. The authorized half uses a real registered store.
    #[tokio::test]
    async fn an_unauthorized_store_denies_the_cursor_authority() {
        let home = tempfile::tempdir().expect("temp home");
        let project_id =
            tracedecay_domain::ProjectId::new("project.pr-context".to_owned()).expect("project id");
        let runtime = RegisteredGlobalDbTestRuntime::project(
            home.path().join("profile"),
            home.path().join("checkout"),
            project_id.clone(),
        )
        .await
        .expect("registered project store");
        let lease = runtime
            .project_database_arc()
            .expect("registered project lease");
        // The daemon provisions this store's signing key at project open; the
        // authority path below reads it back exactly as production does.
        lease
            .ensure_active_session_cursor_key_result()
            .await
            .expect("provision the store's cursor signing key");
        let scope = tracedecay_contracts::ResolvedScope::new(
            project_id,
            tracedecay_domain::RepositoryId::new("repository.pr-context".to_owned())
                .expect("repository id"),
            tracedecay_domain::WorktreeId::new("worktree.pr-context".to_owned())
                .expect("worktree id"),
            None,
        )
        .expect("resolved scope");
        let changes = Vec::new();
        let comparison = || PrContextCursorComparison {
            base_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            head_oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            merge_base: "cccccccccccccccccccccccccccccccccccccccc",
            graph_generation: "generation.pr-context.fixture",
            maximum_symbols: 25,
            changes: &changes,
        };
        let denied_project = crate::tool_context::tests::project_bundle(home.path(), &scope, None);
        let denied_context = crate::tool_context::tests::fixture_context(&denied_project);
        let root = tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(
            denied_context.project_root().as_os_str(),
        );
        let denied_binding = PrContextCursorBinding::new(&denied_context, &root, comparison());
        let refusal = pr_context_cursor_authority(&denied_context, &denied_binding)
            .await
            .expect_err("an unauthorized store must not mint a cursor authority");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_denied"),
            "got {refusal}"
        );

        let authorized_project =
            crate::tool_context::tests::project_bundle(home.path(), &scope, Some(lease));
        let authorized_context = crate::tool_context::tests::fixture_context(&authorized_project);
        let authorized_binding =
            PrContextCursorBinding::new(&authorized_context, &root, comparison());
        pr_context_cursor_authority(&authorized_context, &authorized_binding)
            .await
            .expect("the same store, authorized, opens its own cursor authority");
    }

    /// Two stores can serve the same checkout — a registered project store and
    /// a differently registered one for the same worktree. A cursor minted
    /// against one must not verify against the other, so the store's own
    /// registered shard is part of cursor identity.
    #[test]
    fn a_cursor_from_a_foreign_bound_store_is_denied() {
        let changes = Vec::new();
        let scope = resolved("project.a", None);
        let shard = session_shard("project.a");
        let mine = snapshot_for(&binding_bound_to(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&scope)),
            Some(PrContextCursorStore {
                brain_id: "brain.mine",
                profile_id: "profile.mine",
                scope: &shard,
            }),
            &changes,
        ));
        let theirs = snapshot_for(&binding_bound_to(
            b"/projects/shared",
            Some(PrContextCursorScope::from_resolved(&scope)),
            Some(PrContextCursorStore {
                brain_id: "brain.theirs",
                profile_id: "profile.theirs",
                scope: &shard,
            }),
            &changes,
        ));
        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();

        let encoded =
            encode_pr_context_cursor(&after, nodes, edges, bytes, &theirs, &authenticator)
                .expect("cursor issues");

        let refusal = decode_pr_context_cursor(&encoded, &mine, &authenticator)
            .expect_err("a foreign store's cursor must not decode");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_denied"),
            "got {refusal}"
        );
    }

    /// One project has several registered shards — its session store, its
    /// project store, and its code stores. They are different stores, so
    /// reducing the shard to the project it mentions would let a cursor minted
    /// against one verify against another.
    #[test]
    fn a_cursor_cannot_travel_between_shard_families_of_one_project() {
        let changes = Vec::new();
        let scope = resolved("project.a", None);
        let sessions = session_shard("project.a");
        let project = tracedecay_store::StoreShardScopeV1::Project {
            project_id: tracedecay_domain::ProjectId::new("project.a").expect("project id"),
        };

        let store_for = |shard| {
            binding_bound_to(
                b"/projects/shared",
                Some(PrContextCursorScope::from_resolved(&scope)),
                Some(PrContextCursorStore {
                    brain_id: "brain.local",
                    profile_id: "profile.local",
                    scope: shard,
                }),
                &changes,
            )
        };
        // Denial is decided by cursor identity, so the two stores must not
        // share one. Without this the cursor merely reads as stale.
        assert_ne!(
            store_for(&sessions)
                .identity_digest()
                .expect("session store identity"),
            store_for(&project)
                .identity_digest()
                .expect("project store identity"),
            "two shard families of one project must not share a cursor identity"
        );

        let session_binding = snapshot_for(&store_for(&sessions));
        let project_binding = snapshot_for(&store_for(&project));

        let authenticator = authenticator();
        let (after, nodes, edges, bytes) = position();
        let encoded = encode_pr_context_cursor(
            &after,
            nodes,
            edges,
            bytes,
            &project_binding,
            &authenticator,
        )
        .expect("cursor issues");

        let refusal = decode_pr_context_cursor(&encoded, &session_binding, &authenticator)
            .expect_err("the project shard's cursor must not decode against the session shard");
        assert_eq!(
            refusal.project_route_context().map(|(reason, _, _)| reason),
            Some("pr_context_cursor_denied"),
            "got {refusal}"
        );
    }
}
