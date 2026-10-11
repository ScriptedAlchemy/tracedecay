//! Each stream returns its `<name>.golden` content: the receipt trace, then
//! the logical dump of its store.

use std::collections::BTreeMap;

mod ledger;
mod payloads;
mod registered;
mod stores;

pub async fn render_all() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("ledger.golden".to_owned(), ledger::ledger().await),
        ("code.golden".to_owned(), ledger::code().await),
        ("profile.golden".to_owned(), payloads::profile().await),
        ("project.golden".to_owned(), payloads::project().await),
        ("sessions.golden".to_owned(), payloads::sessions().await),
        (
            "work_and_workflow.golden".to_owned(),
            stores::work_and_workflow().await,
        ),
        ("remote.golden".to_owned(), stores::remote().await),
        (
            "normalizations.golden".to_owned(),
            super::dump::normalization_listing(),
        ),
    ])
}
