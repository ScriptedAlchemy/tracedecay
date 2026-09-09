use super::*;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, RetrievalGrainV1, SessionId, SymbolOccurrenceId, TemporalModeV1,
    canonical_sha256,
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
/// not
/// a locally derived name: the same authority every other scoped read binds
/// to, carried verbatim so a cursor cannot travel between projects.
#[derive(Clone, Copy, Serialize)]
pub(super) struct PrContextCursorScope<'a> {
    pub project_id: &'a str,
    pub repository_id: &'a str,
    pub worktree_id: &'a str,
    pub scope_digest: &'a str,
}

impl<'a> PrContextCursorScope<'a> {
    fn from_resolved(scope: &'a tracedecay_contracts::ResolvedScope) -> Self {
        Self {
            project_id: scope.project_id.as_str(),
            repository_id: scope.repository_id.as_str(),
            worktree_id: scope.worktree_id.as_str(),
            scope_digest: scope.scope_digest.as_str(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct PrContextCursorBinding<'a> {
    pub protocol: &'static str,
    /// The admitted checkout, when the daemon's route resolved one. A cursor
    /// minted under one project's scope cannot verify under another's.
    pub scope: Option<PrContextCursorScope<'a>>,
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
            scope: ctx
                .admitted_scope()
                .map(PrContextCursorScope::from_resolved),
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
/// foreign store's cursor from continuing this pagination. Authorization is
/// read off the admitted binding rather than asserted locally: with no
/// admitted store there is no key and no snapshot.
#[hotpath::measure(label = "mcp.git.cursor.authority")]
pub(super) async fn pr_context_cursor_authority(
    ctx: &McpToolContext<'_>,
    binding: &PrContextCursorBinding<'_>,
) -> Result<(TemporalExecutionSnapshot, GlobalDbCursorKeyProvider)> {
    let Some((session_db, authorization)) = ctx.authorized_project_session_db() else {
        return Err(TraceDecayError::project_route(
            "pr_context_cursor_authority_unavailable",
            true,
            "no admitted project session store can authenticate a PR context cursor",
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
    let graph_generation_hex = graph_digest
        .as_str()
        .strip_prefix("sha256:")
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
        PrContextCursorBinding {
            protocol: "tracedecay.pr-context.cursor.v2",
            scope,
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

    fn scope(project: &'static str) -> PrContextCursorScope<'static> {
        PrContextCursorScope {
            project_id: project,
            repository_id: "repository.pr-context",
            worktree_id: "worktree.pr-context",
            scope_digest: "sha256:pr-context-scope",
        }
    }

    /// A cursor issued for one comparison must decode back to the exact page
    /// position it froze, or pagination silently restarts or skips symbols.
    #[test]
    fn a_cursor_round_trips_to_its_own_page_position() {
        let changes = Vec::new();
        let binding = binding_for(b"/projects/round-trip", Some(scope("project.a")), &changes);
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
        let mine = snapshot_for(&binding_for(root, Some(scope("project.mine")), &changes));
        let theirs = snapshot_for(&binding_for(root, Some(scope("project.theirs")), &changes));
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
        let binding = binding_for(b"/projects/shared", Some(scope("project.a")), &changes);
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
        let before = snapshot_for(&binding_for(
            b"/projects/shared",
            Some(scope("project.a")),
            &empty,
        ));
        let after_change = snapshot_for(&binding_for(
            b"/projects/shared",
            Some(scope("project.a")),
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
}
