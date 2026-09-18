//! `tracedecay_redundancy` over the production MCP `tools/call` path.
//!
//! Expected families are the copies written into the fixture. Reviewable
//! bytes are the extra body bytes a reviewer would read: the sum of those
//! body lengths minus the shortest copy that is already the canonical one.
//! A ranking that counts the wrong bodies, or a rename-normalized pair that
//! leaks into the conservative class, cannot match.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_code_index_runtime::code_index_scheduler::identity::{
    repository_id_for, worktree_id_for,
};

use crate::support::{
    extract_real_server_text, extract_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const LEDGER_INTERIOR: &str = "\
    let one = parse(input);\n\
    let two = transform(one);\n\
    let three = validate(two);\n\
    let four = persist(three);\n\
    let five = audit(four);\n\
    let six = publish(five);\n\
    finish(six, input, one, two, three, four, five);\n\
";

const AUDIT_INTERIOR: &str = "\
    let one = parse(input);\n\
    let two = transform(one);\n\
    let three = validate(two);\n\
    let four = persist(three);\n\
    finish(four, input, one, two, three);\n\
";

const RENAMED_LEFT_INTERIOR: &str = "\
    let parsed = load(source);\n\
    let shaped = reshape(parsed);\n\
    let checked = review(shaped);\n\
    let stored = save(checked);\n\
    emit(stored, source, parsed, shaped, checked);\n\
";

const RENAMED_RIGHT_INTERIOR: &str = "\
    let loaded = load(payload);\n\
    let mapped = reshape(loaded);\n\
    let reviewed = review(mapped);\n\
    let written = save(reviewed);\n\
    emit(written, payload, loaded, mapped, reviewed);\n\
";

fn function_source(name: &str, parameter: &str, interior: &str) -> String {
    format!("pub fn {name}({parameter}: Input) {{\n{interior}}}\n")
}

fn body_span(name: &str, parameter: &str, interior: &str) -> (u64, u64) {
    let source = function_source(name, parameter, interior);
    let start =
        u64::try_from(source.find('{').expect("function body")).expect("body start fits in u64");
    let end = u64::try_from(source.rfind('}').expect("function body end") + 1)
        .expect("body end fits in u64");
    (start, end)
}

fn body_len(name: &str, parameter: &str, interior: &str) -> u64 {
    let (start, end) = body_span(name, parameter, interior);
    end - start
}

/// Extra source a reviewer reads after keeping the shortest copy.
fn reviewable_bytes(lengths: &[u64]) -> u64 {
    let shortest = lengths.iter().copied().min().expect("at least one body");
    lengths.iter().sum::<u64>() - shortest
}

fn write_function(project: &Path, relative: &str, name: &str, parameter: &str, interior: &str) {
    let path = project.join(relative);
    fs::create_dir_all(path.parent().expect("function parent")).unwrap();
    fs::write(path, function_source(name, parameter, interior)).unwrap();
}

fn write_redundancy_project(project: &Path) {
    for (relative, name) in [
        ("src/ledger/alpha.rs", "alpha"),
        ("src/ledger/bravo.rs", "bravo"),
        ("src/ledger/charlie.rs", "charlie"),
    ] {
        write_function(project, relative, name, "input", LEDGER_INTERIOR);
    }
    for (relative, name) in [
        ("src/audit/delta.rs", "delta"),
        ("src/audit/echo.rs", "echo"),
    ] {
        write_function(project, relative, name, "input", AUDIT_INTERIOR);
    }
    write_function(
        project,
        "src/renamed/left.rs",
        "renamed_left",
        "source",
        RENAMED_LEFT_INTERIOR,
    );
    write_function(
        project,
        "src/renamed/right.rs",
        "renamed_right",
        "payload",
        RENAMED_RIGHT_INTERIOR,
    );
    write_function(
        project,
        "src/solo.rs",
        "solo",
        "input",
        "    leftover(input);\n",
    );
}

struct FixtureIdentity {
    project_id: String,
    repository_id: String,
    worktree_id: String,
}

fn identity_text(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .expect("identity serializes")
        .as_str()
        .expect("identity is a string")
        .to_owned()
}

async fn open_fixture() -> (
    crate::support::ProductionCompositionFixture,
    Arc<McpServer>,
    FixtureIdentity,
) {
    let fixture = production_composition_fixture_with_sources(write_redundancy_project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server")
        .clone();
    warm_code_index_search(&server, "alpha").await;
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("registered project identity");
    let repository_id = identity_text(
        repository_id_for(&fixture.project_root).expect("fixture repository identity"),
    );
    let worktree_id =
        identity_text(worktree_id_for(&fixture.project_root).expect("fixture worktree identity"));
    (
        fixture,
        server,
        FixtureIdentity {
            project_id,
            repository_id,
            worktree_id,
        },
    )
}

fn request(
    identity: &FixtureIdentity,
    match_classes: &[&str],
    scope: Value,
    family_limit: u32,
    member_limit: u32,
    work_limit: u32,
    cursor: Option<&str>,
) -> Value {
    json!({
        "project_id": identity.project_id,
        "repository_id": identity.repository_id,
        "match_classes": match_classes,
        "scope": scope,
        "include_generated_paths": false,
        "family_limit": family_limit,
        "member_limit": member_limit,
        "work_limit": work_limit,
        "cursor": cursor,
        "format": "json",
    })
}

async fn redundancy_json(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_redundancy", arguments).await;
    assert_ne!(
        result["isError"],
        json!(true),
        "tracedecay_redundancy failed: {}",
        extract_real_server_text(&result)
    );
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_redundancy did not return JSON: {error}\n{text}")
    })
}

fn assert_object_keys(value: &Value, expected: &[&str]) {
    let mut keys = value
        .as_object()
        .unwrap_or_else(|| panic!("expected an object: {value}"))
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    let mut expected = expected
        .iter()
        .map(|key| (*key).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(keys, expected, "{value}");
}

fn assert_nonempty_string(value: &Value, label: &str) -> String {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{label} is not a string: {value}"));
    assert!(!text.is_empty(), "{label} is empty");
    text.to_owned()
}

fn stable_member(member: &Value, identity: &FixtureIdentity) -> Value {
    assert_object_keys(
        member,
        &[
            "body_span",
            "path",
            "project_id",
            "repository_id",
            "snapshot_digest",
            "source_generation",
            "symbol_occurrence_id",
            "worktree_id",
        ],
    );
    assert_nonempty_string(&member["snapshot_digest"], "snapshot_digest");
    assert_nonempty_string(&member["symbol_occurrence_id"], "symbol_occurrence_id");
    json!({
        "path": member["path"],
        "project_id": identity.project_id,
        "repository_id": identity.repository_id,
        "worktree_id": identity.worktree_id,
        "body_span": member["body_span"],
    })
}

fn stable_family(group: &Value, identity: &FixtureIdentity, generation: &str) -> Value {
    assert_object_keys(
        group,
        &[
            "family",
            "generated_members",
            "reviewable_source_bytes",
            "total_member_count",
        ],
    );
    let family = &group["family"];
    assert_object_keys(
        family,
        &[
            "complete",
            "family_digest",
            "match_class",
            "member_count",
            "members",
            "next_cursor",
            "normalization_revision",
            "representative_payload_digest",
        ],
    );
    assert_nonempty_string(&family["family_digest"], "family_digest");
    assert_nonempty_string(
        &family["representative_payload_digest"],
        "representative_payload_digest",
    );
    let mut members = family["members"]
        .as_array()
        .unwrap_or_else(|| panic!("members is not an array: {group}"))
        .iter()
        .map(|member| {
            assert_eq!(member["source_generation"], generation, "{member}");
            assert_eq!(member["project_id"], identity.project_id, "{member}");
            assert_eq!(member["repository_id"], identity.repository_id, "{member}");
            assert_eq!(member["worktree_id"], identity.worktree_id, "{member}");
            stable_member(member, identity)
        })
        .collect::<Vec<_>>();
    members.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    json!({
        "match_class": family["match_class"],
        "normalization_revision": family["normalization_revision"],
        "member_count": family["member_count"],
        "members": members,
        "complete": family["complete"],
        "next_cursor": family["next_cursor"],
        "total_member_count": group["total_member_count"],
        "reviewable_source_bytes": group["reviewable_source_bytes"],
        "generated_members": group["generated_members"],
    })
}

fn assert_payload_keys(payload: &Value) {
    assert_object_keys(
        payload,
        &[
            "coverage",
            "families",
            "next_cursor",
            "ranked_by",
            "source_generation",
        ],
    );
}

fn observed_families(payload: &Value, identity: &FixtureIdentity) -> Vec<Value> {
    assert_payload_keys(payload);
    assert_eq!(payload["ranked_by"], "reviewable_source_bytes", "{payload}");
    let generation = assert_nonempty_string(&payload["source_generation"], "source_generation");
    payload["families"]
        .as_array()
        .unwrap_or_else(|| panic!("families is not an array: {payload}"))
        .iter()
        .map(|group| stable_family(group, identity, &generation))
        .collect()
}

fn member(
    identity: &FixtureIdentity,
    path: &str,
    name: &str,
    parameter: &str,
    interior: &str,
) -> Value {
    let (start_byte, end_byte) = body_span(name, parameter, interior);
    json!({
        "path": path,
        "project_id": identity.project_id,
        "repository_id": identity.repository_id,
        "worktree_id": identity.worktree_id,
        "body_span": {"start_byte": start_byte, "end_byte": end_byte},
    })
}

fn exact_family(
    identity: &FixtureIdentity,
    members: Vec<Value>,
    total_member_count: u64,
    lengths: &[u64],
    complete: bool,
    next_cursor: Option<&str>,
) -> Value {
    json!({
        "match_class": "conservative_exact",
        "normalization_revision": 1,
        "member_count": members.len(),
        "members": members,
        "complete": complete,
        "next_cursor": next_cursor,
        "total_member_count": total_member_count,
        "reviewable_source_bytes": reviewable_bytes(lengths),
        "generated_members": [],
    })
}

fn ledger_lengths() -> [u64; 3] {
    [
        body_len("alpha", "input", LEDGER_INTERIOR),
        body_len("bravo", "input", LEDGER_INTERIOR),
        body_len("charlie", "input", LEDGER_INTERIOR),
    ]
}

fn audit_lengths() -> [u64; 2] {
    [
        body_len("delta", "input", AUDIT_INTERIOR),
        body_len("echo", "input", AUDIT_INTERIOR),
    ]
}

fn ledger_members(identity: &FixtureIdentity) -> Vec<Value> {
    vec![
        member(
            identity,
            "src/ledger/alpha.rs",
            "alpha",
            "input",
            LEDGER_INTERIOR,
        ),
        member(
            identity,
            "src/ledger/bravo.rs",
            "bravo",
            "input",
            LEDGER_INTERIOR,
        ),
        member(
            identity,
            "src/ledger/charlie.rs",
            "charlie",
            "input",
            LEDGER_INTERIOR,
        ),
    ]
}

fn audit_members(identity: &FixtureIdentity) -> Vec<Value> {
    vec![
        member(
            identity,
            "src/audit/delta.rs",
            "delta",
            "input",
            AUDIT_INTERIOR,
        ),
        member(
            identity,
            "src/audit/echo.rs",
            "echo",
            "input",
            AUDIT_INTERIOR,
        ),
    ]
}

fn assert_no_product_claims(payload: &Value) {
    let rendered = payload.to_string();
    for claim in [
        "dead code",
        "equivalent implementation",
        "safe to merge",
        "guaranteed removable lines",
    ] {
        assert!(!rendered.contains(claim), "{claim} leaked into {payload}");
    }
}

#[tokio::test]
async fn redundancy_ranks_exact_copies_by_duplicated_body_bytes() {
    let ledger = ledger_lengths();
    let audit = audit_lengths();
    assert!(
        reviewable_bytes(&ledger) > reviewable_bytes(&audit),
        "the ledger fixture must rank ahead of the audit fixture"
    );
    let (fixture, server, identity) = open_fixture().await;
    let repository = json!({ "kind": "repository" });

    let full = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            repository.clone(),
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    assert_no_product_claims(&full);
    assert_eq!(full["next_cursor"], Value::Null, "{full}");
    assert_eq!(
        full["coverage"],
        json!({"status": "complete", "examined_families": 2, "examined_members": 5}),
        "{full}"
    );
    assert_eq!(
        observed_families(&full, &identity),
        vec![
            exact_family(&identity, ledger_members(&identity), 3, &ledger, true, None),
            exact_family(&identity, audit_members(&identity), 2, &audit, true, None),
        ],
        "{full}"
    );
    let absent = full.to_string();
    assert!(!absent.contains("src/renamed/"), "{full}");
    assert!(!absent.contains("src/solo.rs"), "{full}");

    let ledger_only = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            json!({"kind": "path", "path": "src/ledger"}),
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    assert_eq!(ledger_only["next_cursor"], Value::Null, "{ledger_only}");
    assert_eq!(
        ledger_only["coverage"],
        json!({"status": "complete", "examined_families": 1, "examined_members": 3}),
        "{ledger_only}"
    );
    assert_eq!(
        observed_families(&ledger_only, &identity),
        vec![exact_family(
            &identity,
            ledger_members(&identity),
            3,
            &ledger,
            true,
            None,
        )],
        "{ledger_only}"
    );

    let one_file = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            json!({"kind": "path", "path": "src/ledger/alpha.rs"}),
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    assert_eq!(one_file["families"], json!([]), "{one_file}");
    assert_eq!(one_file["next_cursor"], Value::Null, "{one_file}");
    assert_eq!(
        one_file["coverage"],
        json!({"status": "complete", "examined_families": 0, "examined_members": 0}),
        "{one_file}"
    );

    let missing = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            json!({"kind": "path", "path": "src/missing"}),
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    assert_eq!(missing["families"], json!([]), "{missing}");
    assert_eq!(
        missing["coverage"],
        json!({"status": "complete", "examined_families": 0, "examined_members": 0}),
        "{missing}"
    );

    let capped = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            json!({"kind": "path", "path": "src/ledger"}),
            10,
            2,
            30,
            None,
        ),
    )
    .await;
    let capped_families = observed_families(&capped, &identity);
    assert_eq!(capped_families.len(), 1, "{capped}");
    assert_eq!(capped_families[0]["total_member_count"], 3, "{capped}");
    assert_eq!(capped_families[0]["member_count"], 2, "{capped}");
    assert_eq!(capped_families[0]["complete"], false, "{capped}");
    assert_eq!(
        capped_families[0]["reviewable_source_bytes"],
        json!(reviewable_bytes(&ledger)),
        "{capped}"
    );
    assert!(
        capped_families[0]["next_cursor"]
            .as_str()
            .is_some_and(|cursor| !cursor.is_empty()),
        "{capped}"
    );
    let capped_paths = capped_families[0]["members"]
        .as_array()
        .expect("capped members")
        .iter()
        .map(|member| member["path"].as_str().expect("path"))
        .collect::<Vec<_>>();
    assert!(
        capped_paths
            .iter()
            .all(|path| path.starts_with("src/ledger/")),
        "{capped}"
    );
    assert_eq!(
        capped["coverage"],
        json!({"status": "complete", "examined_families": 1, "examined_members": 2}),
        "{capped}"
    );

    let first_page = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            repository.clone(),
            1,
            10,
            30,
            None,
        ),
    )
    .await;
    let cursor = first_page["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("family page must continue: {first_page}"))
        .to_owned();
    assert!(!cursor.is_empty(), "{first_page}");
    assert_eq!(
        first_page["coverage"],
        json!({"status": "partial", "reason": "family_limit", "examined_families": 1, "examined_members": 3}),
        "{first_page}"
    );
    assert_eq!(
        observed_families(&first_page, &identity),
        vec![exact_family(
            &identity,
            ledger_members(&identity),
            3,
            &ledger,
            true,
            None,
        )],
        "{first_page}"
    );

    let second_page = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            repository,
            1,
            10,
            30,
            Some(&cursor),
        ),
    )
    .await;
    assert_eq!(second_page["next_cursor"], Value::Null, "{second_page}");
    assert_eq!(
        second_page["coverage"],
        json!({"status": "complete", "examined_families": 1, "examined_members": 2}),
        "{second_page}"
    );
    assert_eq!(
        observed_families(&second_page, &identity),
        vec![exact_family(
            &identity,
            audit_members(&identity),
            2,
            &audit,
            true,
            None,
        )],
        "{second_page}"
    );

    let changed = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            json!({
                "kind": "pull_request",
                "provider": "github",
                "pull_request_id": "1277",
                "head_commit_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "changed_paths": ["src/ledger/alpha.rs"],
            }),
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    assert_eq!(changed["next_cursor"], Value::Null, "{changed}");
    assert_eq!(
        changed["coverage"],
        json!({"status": "complete", "examined_families": 1, "examined_members": 3}),
        "{changed}"
    );
    assert_eq!(
        observed_families(&changed, &identity),
        vec![exact_family(
            &identity,
            ledger_members(&identity),
            3,
            &ledger,
            true,
            None,
        )],
        "{changed}"
    );

    let ledger_bytes = reviewable_bytes(&ledger);
    let markdown = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_redundancy",
            json!({
                "project_id": identity.project_id,
                "repository_id": identity.repository_id,
                "match_classes": ["conservative_exact"],
                "scope": {"kind": "path", "path": "src/ledger"},
                "include_generated_paths": false,
                "family_limit": 10,
                "member_limit": 10,
                "work_limit": 30,
            }),
        )
        .await
        .expect("omitted-format redundancy call");
    assert!(
        markdown.error.is_none(),
        "omitted-format redundancy call failed: {:?}",
        markdown.error
    );
    let markdown = extract_text(markdown.result.as_ref().expect("redundancy markdown"));
    // Default markdown stops before nested member paths. The caller-visible
    // row is the ranking summary; JSON above pins the paths and spans.
    assert!(
        markdown.contains("**ranked_by:** reviewable_source_bytes\n"),
        "{markdown}"
    );
    assert!(
        markdown
            .contains("**examined_families:** 1\n**examined_members:** 3\n**status:** complete\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("match_class=conservative_exact"),
        "{markdown}"
    );
    assert!(markdown.contains("member_count=3"), "{markdown}");
    assert!(markdown.contains("normalization_revision=1"), "{markdown}");
    assert!(
        markdown.contains(&format!("**reviewable_source_bytes:** {ledger_bytes}\n")),
        "{markdown}"
    );
    assert!(
        markdown.contains("**total_member_count:** 3\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("**generated_members:** none"),
        "{markdown}"
    );
    assert!(
        !markdown.starts_with('{'),
        "omitted format must not be JSON: {markdown}"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn redundancy_reports_renames_only_under_the_rename_class_and_refuses_foreign_repositories() {
    let (fixture, server, identity) = open_fixture().await;
    let renamed_scope = json!({"kind": "path", "path": "src/renamed"});

    let conservative = redundancy_json(
        &server,
        request(
            &identity,
            &["conservative_exact"],
            renamed_scope.clone(),
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    assert_eq!(conservative["families"], json!([]), "{conservative}");
    assert_eq!(
        conservative["coverage"],
        json!({"status": "complete", "examined_families": 0, "examined_members": 0}),
        "{conservative}"
    );

    let renamed = redundancy_json(
        &server,
        request(
            &identity,
            &["rename_normalized_exact"],
            renamed_scope,
            10,
            10,
            30,
            None,
        ),
    )
    .await;
    let left = body_len("renamed_left", "source", RENAMED_LEFT_INTERIOR);
    let right = body_len("renamed_right", "payload", RENAMED_RIGHT_INTERIOR);
    assert_eq!(renamed["next_cursor"], Value::Null, "{renamed}");
    assert_eq!(
        renamed["coverage"],
        json!({"status": "complete", "examined_families": 1, "examined_members": 2}),
        "{renamed}"
    );
    assert_eq!(
        observed_families(&renamed, &identity),
        vec![json!({
            "match_class": "rename_normalized_exact",
            "normalization_revision": 1,
            "member_count": 2,
            "members": [
                member(&identity, "src/renamed/left.rs", "renamed_left", "source", RENAMED_LEFT_INTERIOR),
                member(&identity, "src/renamed/right.rs", "renamed_right", "payload", RENAMED_RIGHT_INTERIOR),
            ],
            "complete": true,
            "next_cursor": null,
            "total_member_count": 2,
            "reviewable_source_bytes": reviewable_bytes(&[left, right]),
            "generated_members": [],
        })],
        "{renamed}"
    );

    let mut retired = request(
        &identity,
        &["conservative_exact"],
        json!({"kind": "repository"}),
        1,
        1,
        3,
        None,
    );
    retired["max_pairs"] = json!(4);
    let retired = handle_real_server_tool_call_raw(&server, "tracedecay_redundancy", retired).await;
    assert_eq!(retired["error"]["code"], -32603, "{retired}");
    assert_eq!(
        retired["error"]["message"],
        "tool execution failed: config error: invalid arguments for tracedecay_redundancy: unknown field `max_pairs`, expected one of `project_id`, `repository_id`, `match_classes`, `scope`, `include_generated_paths`, `family_limit`, `member_limit`, `work_limit`, `cursor`",
        "{retired}"
    );

    let unauthorized = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_redundancy",
        json!({
            "project_id": identity.project_id,
            "repository_id": "repository.unauthorized",
            "match_classes": ["conservative_exact"],
            "scope": {"kind": "repository"},
            "include_generated_paths": false,
            "family_limit": 10,
            "member_limit": 10,
            "work_limit": 30,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(
        unauthorized["error"],
        json!({
            "code": -32602,
            "message": "tool project route failed: reason_code=redundancy-repository-not-authorized retryable=false: the selected repository is outside the authorized repository scope",
            "data": {
                "tool": "tracedecay_redundancy",
                "reason_code": "redundancy-repository-not-authorized",
                "retryable": false,
                "detail": "the selected repository is outside the authorized repository scope",
            }
        }),
        "{unauthorized}"
    );

    fixture.harness.shutdown().await;
}
