use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use tracedecay_application::{
    BRANCH_DIFF_CAPABILITY_ID_V1, BRANCH_SEARCH_CAPABILITY_ID_V1, BranchChangedSymbolV1,
    BranchDiffRequestV1, BranchDiffResultV1, BranchDiffSummaryV1, BranchDiffSymbolV1,
    BranchQueryControlsV1, BranchQueryFuture, BranchQueryOutcomeV1, BranchQueryPartialReasonV1,
    BranchQueryPort, BranchQueryRequestV1, BranchQueryResultV1, BranchQueryUnavailableReasonV1,
    BranchSearchRequestV1, BranchSearchResultV1, BranchSnapshotIdentityV1,
};
use tracedecay_domain::{
    ManifestDigest, RetrievalGrainV1, SessionId, TemporalModeV1, UtcMicros, canonical_sha256,
};
use tracedecay_temporal_query::{
    cursor::{StableSortKey, encode_cursor, verify_cursor},
    ports::{
        BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
        TemporalWatermarks,
    },
};

use super::{
    BRANCH_QUERY_BINDING_DOMAIN_V1, BRANCH_QUERY_DEFAULT_DEADLINE_MICROS,
    BRANCH_SEARCH_CANDIDATE_LIMIT, BranchGraphSymbol, BranchResolutionOutcome,
    BranchRevalidationOutcome, DaemonBranchQueryExecutor,
};

impl BranchQueryPort for DaemonBranchQueryExecutor {
    fn execute<'a>(
        &'a self,
        request: BranchQueryRequestV1,
        controls: BranchQueryControlsV1,
    ) -> BranchQueryFuture<'a> {
        Box::pin(async move {
            if request.validate().is_err() {
                return BranchQueryOutcomeV1::Unavailable {
                    reason: BranchQueryUnavailableReasonV1::InvalidRequest,
                };
            }
            let control = QueryControl::new(controls);
            if let Some(terminal) = control.terminal() {
                return terminal.outcome();
            }
            match request {
                BranchQueryRequestV1::Search(request) => {
                    self.execute_search(request, &control).await
                }
                BranchQueryRequestV1::Diff(request) => self.execute_diff(request, &control).await,
            }
        })
    }
}

impl DaemonBranchQueryExecutor {
    async fn execute_search(
        &self,
        request: BranchSearchRequestV1,
        control: &QueryControl,
    ) -> BranchQueryOutcomeV1 {
        let snapshot = match controlled(
            self.resolver
                .resolve(&request.branch, BRANCH_SEARCH_CAPABILITY_ID_V1),
            control,
        )
        .await
        {
            Controlled::Value(BranchResolutionOutcome::Resolved(snapshot)) => snapshot,
            Controlled::Value(BranchResolutionOutcome::Denied) => {
                return BranchQueryOutcomeV1::Denied;
            }
            Controlled::Value(BranchResolutionOutcome::Stale(reason)) => {
                return BranchQueryOutcomeV1::Stale { reason };
            }
            Controlled::Value(BranchResolutionOutcome::Unavailable(reason)) => {
                return BranchQueryOutcomeV1::Unavailable { reason };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        };
        let mut items = match controlled(
            snapshot
                .graph
                .search(&request.query, BRANCH_SEARCH_CANDIDATE_LIMIT + 1),
            control,
        )
        .await
        {
            Controlled::Value(Ok(items)) => items,
            Controlled::Value(Err(_)) => {
                return BranchQueryOutcomeV1::Unavailable {
                    reason: BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable,
                };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        };
        if let Some(terminal) = control.terminal() {
            return terminal.outcome();
        }
        items.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.file.cmp(&right.file))
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.id.cmp(&right.id))
                .then_with(|| left.signature.cmp(&right.signature))
        });
        let truncated = items.len() > BRANCH_SEARCH_CANDIDATE_LIMIT;
        items.truncate(BRANCH_SEARCH_CANDIDATE_LIMIT);
        let total = items.len() as u64;
        let binding = match canonical_sha256(&(
            BRANCH_QUERY_BINDING_DOMAIN_V1,
            "search",
            &snapshot.identity,
            &request.branch,
            &request.query,
            request.limit,
        )) {
            Ok(binding) => binding,
            Err(_) => {
                return BranchQueryOutcomeV1::Unavailable {
                    reason: BranchQueryUnavailableReasonV1::CursorUnavailable,
                };
            }
        };
        let (start, end, next_cursor) = match self.page_bounds(
            &snapshot.identity,
            &binding,
            request.cursor.as_deref(),
            request.limit as usize,
            items.len(),
        ) {
            Ok(page) => page,
            Err(reason) => return BranchQueryOutcomeV1::Unavailable { reason },
        };
        let result = BranchQueryResultV1::Search(BranchSearchResultV1 {
            snapshot: snapshot.identity.clone(),
            total,
            items: items[start..end].to_vec(),
            next_cursor,
        });
        match controlled(
            self.resolver
                .revalidate(&snapshot, BRANCH_SEARCH_CAPABILITY_ID_V1),
            control,
        )
        .await
        {
            Controlled::Value(BranchRevalidationOutcome::Current) => {}
            Controlled::Value(BranchRevalidationOutcome::Denied) => {
                return BranchQueryOutcomeV1::Denied;
            }
            Controlled::Value(BranchRevalidationOutcome::Stale(reason)) => {
                return BranchQueryOutcomeV1::Stale { reason };
            }
            Controlled::Value(BranchRevalidationOutcome::Unavailable(reason)) => {
                return BranchQueryOutcomeV1::Unavailable { reason };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        }
        if truncated {
            BranchQueryOutcomeV1::Partial {
                result,
                reason: BranchQueryPartialReasonV1::ResultLimitReached,
            }
        } else {
            BranchQueryOutcomeV1::Complete { result }
        }
    }

    async fn execute_diff(
        &self,
        request: BranchDiffRequestV1,
        control: &QueryControl,
    ) -> BranchQueryOutcomeV1 {
        let (base, head) =
            match controlled(self.resolver.comparison_targets(&request), control).await {
                Controlled::Value(Ok(targets)) => targets,
                Controlled::Value(Err(reason)) => {
                    return BranchQueryOutcomeV1::Unavailable { reason };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            };
        let base_snapshot = match controlled(
            self.resolver.resolve(&base, BRANCH_DIFF_CAPABILITY_ID_V1),
            control,
        )
        .await
        {
            Controlled::Value(BranchResolutionOutcome::Resolved(snapshot)) => snapshot,
            Controlled::Value(BranchResolutionOutcome::Denied) => {
                return BranchQueryOutcomeV1::Denied;
            }
            Controlled::Value(BranchResolutionOutcome::Stale(reason)) => {
                return BranchQueryOutcomeV1::Stale { reason };
            }
            Controlled::Value(BranchResolutionOutcome::Unavailable(reason)) => {
                return BranchQueryOutcomeV1::Unavailable { reason };
            }
            Controlled::Terminal(terminal) => return terminal.outcome(),
        };
        let head_snapshot = if base == head {
            base_snapshot.clone()
        } else {
            match controlled(
                self.resolver.resolve(&head, BRANCH_DIFF_CAPABILITY_ID_V1),
                control,
            )
            .await
            {
                Controlled::Value(BranchResolutionOutcome::Resolved(snapshot)) => snapshot,
                Controlled::Value(BranchResolutionOutcome::Denied) => {
                    return BranchQueryOutcomeV1::Denied;
                }
                Controlled::Value(BranchResolutionOutcome::Stale(reason)) => {
                    return BranchQueryOutcomeV1::Stale { reason };
                }
                Controlled::Value(BranchResolutionOutcome::Unavailable(reason)) => {
                    return BranchQueryOutcomeV1::Unavailable { reason };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            }
        };
        let (base_nodes, head_nodes) = if base == head {
            (Vec::new(), Vec::new())
        } else {
            (base_snapshot.symbols.clone(), head_snapshot.symbols.clone())
        };
        let (added, removed, changed) = match diff_symbols(
            base_nodes,
            head_nodes,
            request.file.as_deref(),
            request.kind.as_deref(),
            control,
        ) {
            Ok(diff) => diff,
            Err(terminal) => return terminal.outcome(),
        };
        if let Some(terminal) = control.terminal() {
            return terminal.outcome();
        }
        let summary = BranchDiffSummaryV1 {
            added: added.len() as u64,
            removed: removed.len() as u64,
            changed: changed.len() as u64,
        };
        let total = added.len() + removed.len() + changed.len();
        let binding = match canonical_sha256(&(
            BRANCH_QUERY_BINDING_DOMAIN_V1,
            "diff",
            &base_snapshot.identity,
            &head_snapshot.identity,
            &request.file,
            &request.kind,
            request.limit,
        )) {
            Ok(binding) => binding,
            Err(_) => {
                return BranchQueryOutcomeV1::Unavailable {
                    reason: BranchQueryUnavailableReasonV1::CursorUnavailable,
                };
            }
        };
        let (start, end, next_cursor) = match self.page_bounds(
            &base_snapshot.identity,
            &binding,
            request.cursor.as_deref(),
            request.limit as usize,
            total,
        ) {
            Ok(page) => page,
            Err(reason) => return BranchQueryOutcomeV1::Unavailable { reason },
        };
        let (added, removed, changed) = paginate_diff(added, removed, changed, start, end);
        let result = BranchQueryResultV1::Diff(BranchDiffResultV1 {
            base: base_snapshot.identity.clone(),
            head: head_snapshot.identity.clone(),
            note: (base == head).then(|| format!("base and head are the same branch: '{base}'")),
            summary,
            added,
            removed,
            changed,
            next_cursor,
        });
        for snapshot in [&base_snapshot, &head_snapshot] {
            match controlled(
                self.resolver
                    .revalidate(snapshot, BRANCH_DIFF_CAPABILITY_ID_V1),
                control,
            )
            .await
            {
                Controlled::Value(BranchRevalidationOutcome::Current) => {}
                Controlled::Value(BranchRevalidationOutcome::Denied) => {
                    return BranchQueryOutcomeV1::Denied;
                }
                Controlled::Value(BranchRevalidationOutcome::Stale(reason)) => {
                    return BranchQueryOutcomeV1::Stale { reason };
                }
                Controlled::Value(BranchRevalidationOutcome::Unavailable(reason)) => {
                    return BranchQueryOutcomeV1::Unavailable { reason };
                }
                Controlled::Terminal(terminal) => return terminal.outcome(),
            }
        }
        BranchQueryOutcomeV1::Complete { result }
    }

    fn page_bounds(
        &self,
        identity: &BranchSnapshotIdentityV1,
        request_binding: &ManifestDigest,
        cursor: Option<&str>,
        page_size: usize,
        total: usize,
    ) -> Result<(usize, usize, Option<String>), BranchQueryUnavailableReasonV1> {
        let generation_hex = identity
            .generation
            .generation_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(BranchQueryUnavailableReasonV1::CursorUnavailable)?;
        let generation = u64::from_str_radix(&generation_hex[..16], 16)
            .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?
            .max(1);
        let request = TemporalSnapshotRequest::new(
            SessionId::new("branch-query")
                .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?,
            identity.scope_digest.as_str(),
            request_binding.as_str(),
            identity.authorization.access_digest.as_str(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
        )
        .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?;
        let configuration_digest = BindingDigest::new(
            "branch query configuration digest",
            identity.authorization.configuration_digest.as_str(),
        )
        .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?;
        let snapshot = TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation,
                source: generation,
                projection: generation,
                index: generation,
                summary: generation,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest,
            },
            Some(self.cursor_key.clone()),
            tracedecay_temporal_query::resolution::ValidatedAuthorization::Authorized,
        )
        .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?;
        let start = match cursor {
            Some(cursor) => {
                let key = verify_cursor(cursor, &snapshot, self.cursor_authenticator.as_ref())
                    .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?;
                key.stable_id
                    .strip_prefix("branch-query-offset:")
                    .and_then(|offset| offset.parse::<usize>().ok())
                    .filter(|offset| *offset <= total)
                    .ok_or(BranchQueryUnavailableReasonV1::CursorUnavailable)?
            }
            None => 0,
        };
        let end = start.saturating_add(page_size).min(total);
        let next_cursor = if end < total {
            Some(
                encode_cursor(
                    &snapshot,
                    &StableSortKey {
                        normalized_score_micros: 0,
                        knowledge_at_micros: 0,
                        stable_id: format!("branch-query-offset:{end}"),
                    },
                    self.cursor_authenticator.as_ref(),
                )
                .map_err(|_| BranchQueryUnavailableReasonV1::CursorUnavailable)?,
            )
        } else {
            None
        };
        Ok((start, end, next_cursor))
    }
}

fn paginate_diff(
    added: Vec<BranchDiffSymbolV1>,
    removed: Vec<BranchDiffSymbolV1>,
    changed: Vec<BranchChangedSymbolV1>,
    start: usize,
    end: usize,
) -> (
    Vec<BranchDiffSymbolV1>,
    Vec<BranchDiffSymbolV1>,
    Vec<BranchChangedSymbolV1>,
) {
    let added_len = added.len();
    let removed_len = removed.len();
    let added_page = added
        .into_iter()
        .skip(start)
        .take(
            end.saturating_sub(start)
                .min(added_len.saturating_sub(start)),
        )
        .collect();
    let removed_start = start.saturating_sub(added_len);
    let removed_end = end.saturating_sub(added_len).min(removed_len);
    let removed_page = removed
        .into_iter()
        .skip(removed_start)
        .take(removed_end.saturating_sub(removed_start))
        .collect();
    let changed_start = start.saturating_sub(added_len + removed_len);
    let changed_end = end
        .saturating_sub(added_len + removed_len)
        .min(changed.len());
    let changed_page = changed
        .into_iter()
        .skip(changed_start)
        .take(changed_end.saturating_sub(changed_start))
        .collect();
    (added_page, removed_page, changed_page)
}

fn diff_symbols(
    base_nodes: Vec<BranchGraphSymbol>,
    head_nodes: Vec<BranchGraphSymbol>,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    control: &QueryControl,
) -> Result<
    (
        Vec<BranchDiffSymbolV1>,
        Vec<BranchDiffSymbolV1>,
        Vec<BranchChangedSymbolV1>,
    ),
    QueryTerminal,
> {
    let admitted = |symbol: &BranchGraphSymbol| {
        file_filter.is_none_or(|filter| symbol.file == filter || symbol.file.starts_with(filter))
            && kind_filter.is_none_or(|filter| symbol.kind == filter)
    };
    let mut base = BTreeMap::new();
    for symbol in base_nodes {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        if admitted(&symbol) {
            base.insert((symbol.file.clone(), symbol.qualified_name.clone()), symbol);
        }
    }
    let mut head = BTreeMap::new();
    for symbol in head_nodes {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        if admitted(&symbol) {
            head.insert((symbol.file.clone(), symbol.qualified_name.clone()), symbol);
        }
    }
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for (identity, symbol) in &head {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        match base.get(identity) {
            None => added.push(diff_symbol(symbol)),
            Some(base_symbol) if base_symbol.signature != symbol.signature => {
                changed.push(BranchChangedSymbolV1 {
                    name: symbol.name.clone(),
                    qualified_name: symbol.qualified_name.clone(),
                    kind: symbol.kind.clone(),
                    file: symbol.file.clone(),
                    line: symbol.line,
                    base_signature: base_symbol.signature.clone(),
                    head_signature: symbol.signature.clone(),
                });
            }
            Some(_) => {}
        }
    }
    for (identity, symbol) in &base {
        if let Some(terminal) = control.terminal() {
            return Err(terminal);
        }
        if !head.contains_key(identity) {
            removed.push(diff_symbol(symbol));
        }
    }
    Ok((added, removed, changed))
}

fn diff_symbol(symbol: &BranchGraphSymbol) -> BranchDiffSymbolV1 {
    BranchDiffSymbolV1 {
        name: symbol.name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        kind: symbol.kind.clone(),
        file: symbol.file.clone(),
        line: symbol.line,
        signature: symbol.signature.clone(),
    }
}

#[derive(Clone, Copy)]
enum QueryTerminal {
    Cancelled,
    TimedOut,
}

impl QueryTerminal {
    const fn outcome(self) -> BranchQueryOutcomeV1 {
        match self {
            Self::Cancelled => BranchQueryOutcomeV1::Cancelled,
            Self::TimedOut => BranchQueryOutcomeV1::TimedOut,
        }
    }
}

struct QueryControl {
    deadline: tracedecay_application::Deadline,
    cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl QueryControl {
    fn new(controls: BranchQueryControlsV1) -> Self {
        let now = tracedecay_application::now_micros();
        Self {
            deadline: controls
                .deadline
                .unwrap_or(tracedecay_application::Deadline {
                    expires_at: UtcMicros(
                        now.0.saturating_add(BRANCH_QUERY_DEFAULT_DEADLINE_MICROS),
                    ),
                }),
            cancellation: controls.cancellation,
        }
    }

    fn terminal(&self) -> Option<QueryTerminal> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
        {
            Some(QueryTerminal::Cancelled)
        } else if self
            .deadline
            .is_elapsed_at(tracedecay_application::now_micros())
        {
            Some(QueryTerminal::TimedOut)
        } else {
            None
        }
    }

    fn remaining(&self) -> Duration {
        let remaining = self
            .deadline
            .expires_at
            .0
            .saturating_sub(tracedecay_application::now_micros().0)
            .max(0) as u64;
        Duration::from_micros(remaining)
    }
}

enum Controlled<T> {
    Value(T),
    Terminal(QueryTerminal),
}

async fn controlled<T>(future: impl Future<Output = T>, control: &QueryControl) -> Controlled<T> {
    if let Some(terminal) = control.terminal() {
        return Controlled::Terminal(terminal);
    }
    let future = future;
    tokio::pin!(future);
    let deadline = tokio::time::sleep(control.remaining());
    tokio::pin!(deadline);
    if control.cancellation.is_none() {
        return tokio::select! {
            value = &mut future => Controlled::Value(value),
            () = &mut deadline => Controlled::Terminal(QueryTerminal::TimedOut),
        };
    }
    let mut cancellation_poll = tokio::time::interval(Duration::from_millis(10));
    cancellation_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            value = &mut future => return Controlled::Value(value),
            () = &mut deadline => return Controlled::Terminal(QueryTerminal::TimedOut),
            _ = cancellation_poll.tick() => {
                if control.cancellation.as_ref().is_some_and(
                    tracedecay_application::CancellationSignal::is_cancelled,
                ) {
                    return Controlled::Terminal(QueryTerminal::Cancelled);
                }
            }
        }
    }
}
