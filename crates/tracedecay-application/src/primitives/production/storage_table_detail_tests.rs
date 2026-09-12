use super::extended_primitive::{STORAGE_TABLE_DETAIL_LIMIT, largest_table_details};

#[test]
fn tables_are_ranked_by_bytes_and_the_tail_is_counted() {
    let tables = (0..STORAGE_TABLE_DETAIL_LIMIT + 3)
        .map(|index| (format!("t{index:02}"), (index as u64 + 1) * 100))
        .collect();

    let details = largest_table_details(Ok(tables));

    assert_eq!(
        details.first().map(String::as_str),
        Some("table bytes total 9100 across 13 tables")
    );
    assert_eq!(
        details.get(1).map(String::as_str),
        Some("table t12 holds 1300 bytes"),
        "the largest table must lead"
    );
    assert_eq!(
        details.last().map(String::as_str),
        Some("3 smaller tables not listed")
    );
}

#[test]
fn an_unsampled_store_says_so_instead_of_reporting_no_bytes() {
    let details =
        largest_table_details(Err(tracedecay_domain::errors::TraceDecayError::Database {
            message: "reader lease timed out".to_owned(),
            operation: "sample graph-store table sizes".to_owned(),
        }));

    assert_eq!(details.len(), 1);
    assert!(
        details[0].starts_with("table sizes could not be sampled: "),
        "unexpected detail: {}",
        details[0]
    );
}

#[test]
fn a_store_with_no_tables_is_distinct_from_an_unsampled_store() {
    assert_eq!(
        largest_table_details(Ok(Vec::new())),
        vec!["table sizes reported no tables".to_owned()]
    );
}
