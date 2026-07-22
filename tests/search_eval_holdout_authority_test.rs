//! Direct holdout-label loading rejects agent/owner import machinery.

use std::fs;

use tempfile::TempDir;
use tracedecay::search_eval::holdout::load_direct_holdout_labels;

#[test]
fn direct_loader_rejects_agent_adjudicated_authority_without_owner_imports() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("labels.json");
    fs::write(
        &path,
        br#"{"schema_revision":1,"label_authority":"agent_adjudicated","judgments":[]}"#,
    )
    .unwrap();
    let err = load_direct_holdout_labels(&path, None).expect_err("agent authority rejected");
    let message = err.to_string();
    assert!(
        message.contains("parse") || message.contains("unknown variant") || message.contains("agent_adjudicated"),
        "unexpected error: {message}"
    );
}
