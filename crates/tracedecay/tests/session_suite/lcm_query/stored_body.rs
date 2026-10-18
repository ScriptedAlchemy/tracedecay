use super::*;

/// A message body is stored in exactly one row. That one row serves session
/// search and LCM expand, and the credential redacted at ingest reaches
/// neither of them.
#[tokio::test]
async fn one_stored_body_serves_search_and_expand_redacted() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let secret = ["sk-proj-single-copy-", "1234567890abcdef"].concat();
    let body = format!("orchard ledger rotation uses {secret} for the nightly job");
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", std::slice::from_ref(&body)).await;

    let snapshot_path = tmp.path().join("single-copy-snapshot.db");
    db.snapshot_session_database_for_test(HostAdmissionScope::Profile, &snapshot_path)
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&snapshot_path).unwrap();
    let tables: Vec<(String, Vec<String>)> = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%'
               AND name NOT LIKE '%\\_fts\\_%' ESCAPE '\\'",
        )
        .unwrap()
        .query_map((), |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .map(|table| {
            let columns = connection
                .prepare(&format!("SELECT name FROM pragma_table_xinfo('{table}')"))
                .unwrap()
                .query_map((), |row| row.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (table, columns)
        })
        .collect();
    let mut stored_copies = Vec::new();
    for (table, columns) in &tables {
        for column in columns {
            // Generated retrieval columns derive from `content`; they are not
            // stored copies.
            if table == "lcm_raw_messages"
                && matches!(column.as_str(), "snippet_text" | "index_text")
            {
                continue;
            }
            let holding: i64 = connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM \"{table}\"
                         WHERE instr(CAST(\"{column}\" AS TEXT), 'orchard ledger rotation') > 0"
                    ),
                    (),
                    |row| row.get(0),
                )
                .unwrap();
            if holding > 0 {
                stored_copies.push(format!("{table}.{column}"));
            }
        }
    }
    assert_eq!(stored_copies, vec!["lcm_raw_messages.content".to_owned()]);
    let secret_holders = tables
        .iter()
        .flat_map(|(table, columns)| columns.iter().map(move |column| (table, column)))
        .filter(|(table, column)| {
            connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM \"{table}\"
                         WHERE instr(CAST(\"{column}\" AS TEXT), ?1) > 0"
                    ),
                    [&secret],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
                > 0
        })
        .count();
    assert_eq!(secret_holders, 0, "the credential must be redacted at rest");
    drop(connection);

    let searched = db
        .search_session_messages_for_test(
            HostAdmissionScope::Profile,
            "cursor",
            None,
            "orchard ledger",
            10,
        )
        .await
        .unwrap();
    assert_eq!(searched.len(), 1);
    assert!(searched[0].message.text.contains("orchard ledger rotation"));
    assert!(!searched[0].message.text.contains(&secret));

    let expanded = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .unwrap();
    assert!(
        expanded
            .content
            .starts_with("orchard ledger rotation uses ")
    );
    assert!(expanded.content.ends_with(" for the nightly job"));
    assert!(!expanded.content.contains(&secret));
}
