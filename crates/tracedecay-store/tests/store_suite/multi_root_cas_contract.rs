use tracedecay_domain::{ScopeSetId, ScopeSetRevision};
use tracedecay_store::runtime::{
    AuthorizedScopeSetRecordV1, ScopeSetCompareAndSwapV1, ScopeSetStoreContractError,
};

use tracedecay_domain::test_fixtures::digest;

fn record(revision: u64) -> AuthorizedScopeSetRecordV1 {
    AuthorizedScopeSetRecordV1::new(
        ScopeSetId::new("scope-set.fixture").unwrap(),
        ScopeSetRevision::new(revision).unwrap(),
        digest(char::from_digit(revision as u32, 16).unwrap()),
        format!("{{\"revision\":{revision}}}").into_bytes(),
    )
    .unwrap()
}

#[test]
fn scope_set_cas_requires_creation_or_one_exact_next_revision() {
    ScopeSetCompareAndSwapV1::new(None, record(1)).unwrap();
    ScopeSetCompareAndSwapV1::new(Some(ScopeSetRevision::new(1).unwrap()), record(2)).unwrap();

    assert_eq!(
        ScopeSetCompareAndSwapV1::new(None, record(2)).unwrap_err(),
        ScopeSetStoreContractError::NonSequentialRevision
    );
    assert_eq!(
        ScopeSetCompareAndSwapV1::new(Some(ScopeSetRevision::new(1).unwrap()), record(3))
            .unwrap_err(),
        ScopeSetStoreContractError::NonSequentialRevision
    );
}
