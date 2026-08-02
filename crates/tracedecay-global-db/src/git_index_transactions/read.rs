//! Daemon-side typed read executor for the code family of the S11 repository
//! read port.
//!
//! The runtime `ConcreteRepositoryReadExecutor` rejects
//! [`RepositoryReadOperationV1::Code`](tracedecay_store::RepositoryReadOperationV1)
//! ("repository attachment does not own code reads") because the git-index
//! transaction tables live in the daemon `RegisteredGlobalDb`, not in the runtime crate's
//! writer ledger. This executor answers that family here — the mirror image of
//! how [`EffectsLedgerReadExecutor`](tracedecay_rusqlite_runtime) answers the
//! effects family in the runtime crate, where those tables happen to live.
//!
//! Every projection is served by the canonical
//! [`GlobalDbGitIndexTransactionStore`] read logic (which owns the SQL); this
//! executor only adds the contract's keyset pagination on top of the store's
//! whole-set recovery listings. The store's per-record and per-repository
//! decode paths already re-validate and re-bind each row before it is returned,
//! so no decode is duplicated here.

use tracedecay_domain::{GitIndexIdempotencyKey, RepositoryId};
use tracedecay_store::{
    CodeReadOperationV1, CodeReadResultV1, CodeRecoveryCandidatesPageV1,
    CodeRecoveryCandidatesQueryV1, CodeRecoveryRepositoriesPageV1, CodeRecoveryRepositoriesQueryV1,
    GitIndexTransactionRecordV1, GitIndexTransactionStoreResult,
};

use super::store::GlobalDbGitIndexTransactionStore;

/// Answers the code family of the repository read port over one already-open
/// canonical `RegisteredGlobalDb` transaction store.
pub struct GitIndexReadExecutor<'store, 'db> {
    store: &'store GlobalDbGitIndexTransactionStore<'db>,
}

impl<'store, 'db> GitIndexReadExecutor<'store, 'db> {
    pub const fn new(store: &'store GlobalDbGitIndexTransactionStore<'db>) -> Self {
        Self { store }
    }

    /// Dispatches one closed [`CodeReadOperationV1`] to its canonical store
    /// projection and returns the matching [`CodeReadResultV1`] arm.
    pub async fn execute_read(
        &self,
        operation: &CodeReadOperationV1,
    ) -> GitIndexTransactionStoreResult<CodeReadResultV1> {
        match operation {
            CodeReadOperationV1::Preview(preview_id) => self
                .store
                .read_preview(preview_id)
                .await
                .map(|preview| CodeReadResultV1::Preview(Box::new(preview))),
            CodeReadOperationV1::TransactionRecord(idempotency_key) => self
                .store
                .read_record(idempotency_key)
                .await
                .map(|record| CodeReadResultV1::TransactionRecord(Box::new(record))),
            CodeReadOperationV1::RecoveryCandidates(query) => self
                .recovery_candidates(query)
                .await
                .map(CodeReadResultV1::RecoveryCandidates),
            CodeReadOperationV1::RecoveryRepositories(query) => self
                .recovery_repositories(query)
                .await
                .map(CodeReadResultV1::RecoveryRepositories),
        }
    }

    /// Keyset page over a repository's non-terminal recovery records, ordered by
    /// idempotency key. The store returns the whole recovery set (bounded by the
    /// number of active transactions, not the repository's full history); the
    /// walk is applied here so the contract cursor stays store-neutral.
    async fn recovery_candidates(
        &self,
        query: &CodeRecoveryCandidatesQueryV1,
    ) -> GitIndexTransactionStoreResult<CodeRecoveryCandidatesPageV1> {
        if query.limit == 0 {
            return Ok(CodeRecoveryCandidatesPageV1 {
                records: Vec::new(),
                next: None,
            });
        }
        let records = self.store.recovery_candidates(&query.repository_id).await?;
        Ok(paginate_candidates(
            records,
            query.after.as_ref(),
            query.limit,
        ))
    }

    /// Keyset page over the repositories that hold recovery records, ordered by
    /// repository id.
    async fn recovery_repositories(
        &self,
        query: &CodeRecoveryRepositoriesQueryV1,
    ) -> GitIndexTransactionStoreResult<CodeRecoveryRepositoriesPageV1> {
        if query.limit == 0 {
            return Ok(CodeRecoveryRepositoriesPageV1 {
                repositories: Vec::new(),
                next: None,
            });
        }
        let repositories = self.store.recovery_repositories().await?;
        Ok(paginate_repositories(
            repositories,
            query.after.as_ref(),
            query.limit,
        ))
    }
}

fn paginate_candidates(
    mut records: Vec<GitIndexTransactionRecordV1>,
    after: Option<&GitIndexIdempotencyKey>,
    limit: u32,
) -> CodeRecoveryCandidatesPageV1 {
    records.sort_by(|left, right| {
        left.idempotency_key
            .as_str()
            .cmp(right.idempotency_key.as_str())
    });
    let mut records: Vec<GitIndexTransactionRecordV1> = records
        .into_iter()
        .filter(|record| match after {
            Some(after) => record.idempotency_key.as_str() > after.as_str(),
            None => true,
        })
        .collect();
    let next = page_tail(&mut records, limit, |record| record.idempotency_key.clone());
    CodeRecoveryCandidatesPageV1 { records, next }
}

fn paginate_repositories(
    mut repositories: Vec<RepositoryId>,
    after: Option<&RepositoryId>,
    limit: u32,
) -> CodeRecoveryRepositoriesPageV1 {
    // `recovery_repositories()` already returns rows in `repository_id` order;
    // sort defensively so the keyset walk never depends on that ordering.
    repositories.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    let mut repositories: Vec<RepositoryId> = repositories
        .into_iter()
        .filter(|repository| match after {
            Some(after) => repository.as_str() > after.as_str(),
            None => true,
        })
        .collect();
    let next = page_tail(&mut repositories, limit, RepositoryId::clone);
    CodeRecoveryRepositoriesPageV1 { repositories, next }
}

/// Truncates an in-memory page back to `limit` and returns the last retained
/// element's cursor when more rows remain. Mirrors the over-fetch/`page_tail`
/// pattern the effects executor uses against SQL `LIMIT n + 1`.
fn page_tail<T, C>(items: &mut Vec<T>, limit: u32, cursor: impl Fn(&T) -> C) -> Option<C> {
    let limit = limit as usize;
    if items.len() > limit {
        items.truncate(limit);
        items.last().map(cursor)
    } else {
        None
    }
}
