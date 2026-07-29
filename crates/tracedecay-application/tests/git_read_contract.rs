//! External-implementor compatibility for the read-only Git contracts.
//!
//! Extracting these contracts into application split `historical_blob` out of
//! `GitReadPort` into the `GitHistoricalBlobReadPort` supertrait, so an
//! out-of-tree adapter now has to implement two traits where one used to be
//! enough. This test lives outside the owning crate on purpose: it compiles
//! only against the published surface, so it fails if any piece an external
//! implementor needs stops being reachable.

use tracedecay_application::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GIT_HISTORY_MAX_COUNT_LIMIT, GitBlameRequest,
    GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1, GitHistoricalBlobV1, GitHistoryRequest,
    GitIntelligenceError, GitReadPort,
};
use tracedecay_domain::{
    GitBlameV1, GitDiffScopeV1, GitDiffV1, GitHistoryV1, GitOidV1, GitStatusV1, HunkRefV1,
    ManifestDigest, RepositoryId, WorktreeId,
};

/// Stands in for an out-of-tree Git adapter.
struct ExternalGitReader {
    repository: RepositoryId,
    worktree: WorktreeId,
    commit: GitOidV1,
    blob: GitOidV1,
    bytes: Vec<u8>,
}

impl ExternalGitReader {
    fn new() -> Self {
        Self {
            repository: RepositoryId::new("repository.external.fixture").expect("repository id"),
            worktree: WorktreeId::new("worktree.external.fixture").expect("worktree id"),
            commit: GitOidV1::new("a".repeat(40)).expect("commit oid"),
            blob: GitOidV1::new("b".repeat(40)).expect("blob oid"),
            bytes: b"external blob".to_vec(),
        }
    }

    /// A typed refusal an external adapter is allowed to return. Constructing it
    /// here also proves the error variants are reachable, not just the enum.
    fn unsupported(operation: &str) -> GitIntelligenceError {
        GitIntelligenceError::ReadOnlyViolation(operation.to_owned())
    }
}

impl GitHistoricalBlobReadPort for ExternalGitReader {
    fn historical_blob(
        &self,
        request: &GitHistoricalBlobRequestV1,
    ) -> Result<GitHistoricalBlobV1, GitIntelligenceError> {
        if request.max_bytes > GIT_HISTORICAL_BLOB_MAX_BYTES {
            return Err(GitIntelligenceError::HistoricalBlobBoundExceeded {
                bound: GIT_HISTORICAL_BLOB_MAX_BYTES,
                actual: request.max_bytes,
            });
        }
        Ok(GitHistoricalBlobV1 {
            repository: self.repository.clone(),
            worktree: self.worktree.clone(),
            commit: request.commit.clone(),
            path: request.path.clone(),
            blob_oid: Some(self.blob.clone()),
            bytes: request.include_bytes.then(|| self.bytes.clone()),
        })
    }
}

impl GitReadPort for ExternalGitReader {
    fn status(&self) -> Result<GitStatusV1, GitIntelligenceError> {
        Err(Self::unsupported("status"))
    }

    fn diff(&self, _scope: &GitDiffScopeV1) -> Result<GitDiffV1, GitIntelligenceError> {
        Err(Self::unsupported("diff"))
    }

    fn history(&self, request: &GitHistoryRequest) -> Result<GitHistoryV1, GitIntelligenceError> {
        assert!(
            request.max_count <= GIT_HISTORY_MAX_COUNT_LIMIT,
            "external adapters must be able to observe the published history bound"
        );
        Err(Self::unsupported("history"))
    }

    fn blame(&self, _request: &GitBlameRequest) -> Result<GitBlameV1, GitIntelligenceError> {
        Err(Self::unsupported("blame"))
    }

    fn hunk_refs(
        &self,
        _scope: &GitDiffScopeV1,
        _preview_id: &str,
        _snapshot_digest: &ManifestDigest,
    ) -> Result<Vec<HunkRefV1>, GitIntelligenceError> {
        Err(Self::unsupported("hunk-refs"))
    }
}

fn blob_request(reader: &ExternalGitReader, include_bytes: bool) -> GitHistoricalBlobRequestV1 {
    GitHistoricalBlobRequestV1 {
        commit: reader.commit.clone(),
        path: "src/lib.rs".to_owned(),
        max_bytes: GIT_HISTORICAL_BLOB_MAX_BYTES,
        include_bytes,
    }
}

#[test]
fn an_external_type_can_implement_both_read_ports() {
    let reader = ExternalGitReader::new();
    let request = blob_request(&reader, true);

    let blob = GitHistoricalBlobReadPort::historical_blob(&reader, &request)
        .expect("external adapter serves its own historical blob");
    assert_eq!(blob.path, request.path);
    assert_eq!(blob.commit, request.commit);
    assert_eq!(blob.bytes.as_deref(), Some(b"external blob".as_slice()));

    let without_bytes =
        GitHistoricalBlobReadPort::historical_blob(&reader, &blob_request(&reader, false))
            .expect("an absent-bytes read is still a successful read");
    assert!(without_bytes.bytes.is_none());
    assert_eq!(without_bytes.blob_oid, blob.blob_oid);
}

#[test]
fn both_ports_stay_usable_through_trait_objects() {
    let reader = ExternalGitReader::new();
    let request = blob_request(&reader, false);

    let blob_port: &dyn GitHistoricalBlobReadPort = &reader;
    let read_port: &dyn GitReadPort = &reader;

    assert_eq!(
        blob_port
            .historical_blob(&request)
            .expect("narrow port read")
            .path,
        read_port
            .historical_blob(&request)
            .expect("full port inherits the narrow read")
            .path,
        "the supertrait method must resolve identically through either object"
    );
    assert!(
        matches!(
            read_port.status(),
            Err(GitIntelligenceError::ReadOnlyViolation(_))
        ),
        "an external adapter may refuse an operation with a typed error"
    );
}

/// `GitReadPort` implies `GitHistoricalBlobReadPort`, so a caller that accepts
/// the full port never has to ask for the narrow one as a separate bound. This
/// is a compile-time proof of the split's shape.
#[test]
fn the_full_read_port_implies_the_historical_blob_port() {
    fn accepts_blob_port<T: GitHistoricalBlobReadPort>(reader: &T, path: &str) -> String {
        let request = GitHistoricalBlobRequestV1 {
            commit: GitOidV1::new("c".repeat(40)).expect("commit oid"),
            path: path.to_owned(),
            max_bytes: 1,
            include_bytes: false,
        };
        reader
            .historical_blob(&request)
            .expect("narrow bound read")
            .path
    }

    fn accepts_read_port<T: GitReadPort>(reader: &T, path: &str) -> String {
        accepts_blob_port(reader, path)
    }

    assert_eq!(
        accepts_read_port(&ExternalGitReader::new(), "src/main.rs"),
        "src/main.rs"
    );
}

#[test]
fn published_read_bounds_are_enforceable_by_an_external_adapter() {
    let reader = ExternalGitReader::new();
    let over_bound = GitHistoricalBlobRequestV1 {
        max_bytes: GIT_HISTORICAL_BLOB_MAX_BYTES + 1,
        ..blob_request(&reader, false)
    };

    assert!(
        matches!(
            reader.historical_blob(&over_bound),
            Err(GitIntelligenceError::HistoricalBlobBoundExceeded { bound, actual })
                if bound == GIT_HISTORICAL_BLOB_MAX_BYTES
                    && actual == GIT_HISTORICAL_BLOB_MAX_BYTES + 1
        ),
        "the byte bound and its typed rejection must both be reachable externally"
    );

    let history = GitHistoryRequest {
        max_count: GIT_HISTORY_MAX_COUNT_LIMIT,
        ..GitHistoryRequest::default()
    };
    assert!(matches!(
        reader.history(&history),
        Err(GitIntelligenceError::ReadOnlyViolation(_))
    ));
}
