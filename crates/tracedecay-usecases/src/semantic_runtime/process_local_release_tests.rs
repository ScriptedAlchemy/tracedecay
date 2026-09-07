//! Process-local generation-handle release: project unmount must drop the
//! semantic registries that would otherwise keep a retired generation resident.

use std::path::Path;
use std::sync::Arc;

use crate::semantic_runtime::{
    project_semantic_activation_gate, unregister_project_semantic_runtime,
};

#[test]
fn project_unregistration_drops_the_process_local_activation_gate() {
    let project_root = Path::new("/tmp/tracedecay-retention-process-local-release");
    let gate = project_semantic_activation_gate(project_root);
    let probe = Arc::downgrade(&gate);
    assert!(
        probe.upgrade().is_some(),
        "the gate must be live before unmount"
    );
    drop(gate);

    unregister_project_semantic_runtime(project_root);

    assert!(
        probe.upgrade().is_none(),
        "project unmount must release the last strong activation-gate handle"
    );
}
