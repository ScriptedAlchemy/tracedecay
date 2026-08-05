use std::collections::BTreeSet;

use tracedecay_api::{WorkProductHttpOperation, WorkProductOperationFamily};

#[test]
fn product_routes_are_one_canonical_family_with_typed_contracts() {
    assert_eq!(WorkProductHttpOperation::ALL.len(), 6);
    let mut paths = BTreeSet::new();
    let mut operation_ids = BTreeSet::new();
    for operation in WorkProductHttpOperation::ALL {
        assert_eq!(operation.family(), WorkProductOperationFamily::Product);
        assert!(operation.route_path().starts_with("/work/product/"));
        assert!(
            operation
                .application_route_path()
                .starts_with("/application/work/product/")
        );
        assert!(paths.insert(operation.route_path()));
        assert!(operation_ids.insert(operation.operation_id_str()));
        assert_ne!(operation.request_schema_name().as_ref(), "Value");
        assert_ne!(operation.result_schema_name().as_ref(), "Value");
    }
}

#[test]
fn only_product_reads_are_marked_read_only() {
    let effects = WorkProductHttpOperation::ALL
        .into_iter()
        .map(|operation| (operation.operation_key(), operation.is_read_only()))
        .collect::<Vec<_>>();
    assert_eq!(
        effects,
        vec![
            ("product_snapshot", true),
            ("product_projections", true),
            ("task_evidence", true),
            ("expand_task_evidence", true),
            ("generate_work_proposal", true),
            ("apply_work_command", false),
        ]
    );
}
