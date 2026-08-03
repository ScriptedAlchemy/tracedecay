//! Graph-store read-only compatibility evidence across the canonical runtime and immutable SQLite.
//!
//! The helper is deliberately exercised only through its closed, typed protocol.
//! Runtime-side SQL below captures fixed read-only SQLite metadata; it is not a
//! helper escape hatch and no SQL is ever sent across the subprocess boundary.

#[path = "../common/mod.rs"]
mod common;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay::{
    db::Database,
    types::{Edge, EdgeKind, FileRecord, Node},
};
use tracedecay_domain::LocatorDigest;
use tracedecay_rusqlite_runtime::{
    exact_sql::{ExactSqlHandle, ExactSqlRow, ExactSqlRows, ExactSqlStatement, ExactSqlValue},
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};
use tracedecay_sqlite_parity_protocol::{
    CopiedDatabase, CopiedSnapshotProvenance, DatabaseKind, SnapshotFileIdentity,
};
use tracedecay_store::{
    AdmissionConfigV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, StorageRuntimeErrorV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::support::{
    DatabaseArtifactInventory, DatabaseArtifactKind, IsolatedTempRoot, assert_artifacts_unchanged,
    inventory_database_artifacts, invoke_rusqlite_parity, snapshot_content_digest,
};

const PROTOCOL_VERSION: u16 = 1;
const HELPER_SNAPSHOT_AUTHORITY_IDENTITY: &str = "storage-runtime-suite:graph-helper-snapshot";
const GRAPH_FILE: &str = "src/storage_runtime_graph.rs";
const PRIMARY_NODE_ID: &str = "function:storage-runtime-parity-primary";
const SECONDARY_NODE_ID: &str = "function:storage-runtime-parity-secondary";
const FTS_QUERY: &str = "\"paritytoken\"";
const FTS_LIMIT: u16 = 10;
const METADATA_ROWS: &[(&str, &str)] = &[
    ("storage_runtime_graph_marker", "read-only-parity"),
    ("last_sync_at", "1700000001"),
    ("last_full_sync_at", "1700000002"),
    ("last_sync_duration_ms", "73"),
];

const SCHEMA_SQL: &str = "
    SELECT type, name, tbl_name, sql
    FROM sqlite_schema
    WHERE type IN ('table', 'index', 'trigger', 'view')
    ORDER BY type, name
    LIMIT 10001";
const FTS_SQL: &str = "
    SELECT rowid, rank, snippet(nodes_fts, 0, '<mark>', '</mark>', '…', 24)
    FROM nodes_fts
    WHERE nodes_fts MATCH ?1
    ORDER BY rank, rowid
    LIMIT ?2";

#[derive(Debug, Eq, PartialEq)]
struct SchemaObjectEvidence {
    kind: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct SchemaEvidence {
    schema_version: i64,
    user_version: i64,
    objects: Vec<SchemaObjectEvidence>,
}

#[derive(Debug, PartialEq)]
struct FtsMatchEvidence {
    rowid: i64,
    rank: f64,
    snippet: String,
}

#[derive(Debug)]
struct GraphRuntimeEvidence {
    schema: SchemaEvidence,
    foreign_keys: bool,
    page_size: u32,
    journal_mode: String,
    quick_check: Vec<String>,
    integrity_check: Vec<String>,
    table_counts: BTreeMap<String, u64>,
    fts_matches: Vec<FtsMatchEvidence>,
}

#[derive(Clone)]
struct NoTypedReads;

impl ReaderQueryExecutor for NoTypedReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        unreachable!("graph parity uses only the bounded exact-SQL read channel")
    }
}

struct CanonicalGraphReader {
    handle: ExactSqlHandle,
    _readers: ReaderPool<NoTypedReads>,
}

impl CanonicalGraphReader {
    fn open(path: &Path) -> Self {
        let path = path
            .canonicalize()
            .expect("canonicalize graph runtime baseline");
        let binding: StoreRuntimeBindingV1 = serde_json::from_value(json!({
            "shard_id": {
                "brain_id": "brain.storage-runtime-suite",
                "profile_id": "profile.storage-runtime-suite",
                "scope": {
                    "kind": "project",
                    "project_id": "project.storage-runtime-suite.graph"
                }
            },
            "incarnation": 1,
            "authority_epoch": 1
        }))
        .expect("construct graph runtime baseline binding");
        let digest = hex::encode(Sha256::digest(path.as_os_str().as_encoded_bytes()));
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(1).expect("valid graph runtime incarnation"),
            LocatorDigest::new(format!("sha256:{digest}"))
                .expect("valid graph runtime locator digest"),
        );
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path)
                .expect("valid graph runtime reader locator"),
            AdmissionConfigV1::default().readers,
            NoTypedReads,
        )
        .expect("start canonical graph runtime readers");
        let handle = ExactSqlHandle::attach_read_only(&readers);
        Self {
            handle,
            _readers: readers,
        }
    }

    fn query(&self, sql: &str, params: Vec<ExactSqlValue>) -> ExactSqlRows {
        self.handle
            .query(
                ExactSqlStatement::new(sql.to_string(), params)
                    .expect("valid fixed graph runtime query"),
                Duration::from_secs(5),
            )
            .unwrap_or_else(|error| panic!("execute fixed graph runtime query {sql:?}: {error}"))
    }
}

#[tokio::test]
async fn graph_read_only_parity_uses_the_process_isolated_rusqlite_helper() {
    let root = IsolatedTempRoot::new("graph");
    let source = root.path().join("source-graph.db");
    let checkpointed_source = root.path().join("checkpointed-source-graph.db");
    let runtime_baseline_copy = root.path().join("runtime-baseline-graph.db");
    let helper_snapshot_copy = root.path().join("helper-snapshot-graph.db");

    let writer = seed_latest_schema_graph(&source).await;
    assert_graph_api_evidence(&writer).await;
    writer
        .checkpoint()
        .await
        .expect("checkpoint seeded latest-schema graph database");
    writer
        .snapshot_to(&checkpointed_source)
        .await
        .expect("snapshot seeded latest-schema graph database");
    writer.close();

    let source_authority_before_probes = inventory_database_artifacts(&checkpointed_source);
    assert_checkpointed_snapshot(
        &source_authority_before_probes,
        "seeded graph source authority",
    );

    let runtime_baseline_before_reads = copy_checkpointed_graph_snapshot(
        &checkpointed_source,
        &source_authority_before_probes,
        &runtime_baseline_copy,
        "canonical runtime graph baseline",
    );
    let helper_snapshot_before_reads = copy_checkpointed_graph_snapshot(
        &checkpointed_source,
        &source_authority_before_probes,
        &helper_snapshot_copy,
        "rusqlite helper graph snapshot",
    );
    let helper_authority = HelperSnapshotAuthority::seal(
        &root,
        &helper_snapshot_copy,
        helper_snapshot_before_reads.clone(),
    );
    assert_artifacts_unchanged(
        &source_authority_before_probes,
        &inventory_database_artifacts(&checkpointed_source),
        "creating coherent graph probe copies must not mutate the source authority",
    );

    let runtime_reader = CanonicalGraphReader::open(&runtime_baseline_copy);
    let runtime = capture_runtime_evidence(&runtime_reader);
    drop(runtime_reader);

    let runtime_baseline_after_reads = inventory_database_artifacts(&runtime_baseline_copy);
    assert_runtime_read_only_baseline_sidecars(
        &runtime_baseline_before_reads,
        &runtime_baseline_after_reads,
    );
    assert_artifacts_unchanged(
        &source_authority_before_probes,
        &inventory_database_artifacts(&checkpointed_source),
        "canonical graph baseline must not mutate the source authority",
    );

    // Compare the normalized logical baseline from its own coherent copy with
    // the process-isolated helper reading only the pristine helper snapshot.
    assert_rusqlite_helper_parity(&helper_authority, &runtime);
    assert_artifacts_unchanged(
        &helper_snapshot_before_reads,
        &inventory_database_artifacts(&helper_snapshot_copy),
        "process-isolated rusqlite graph reads",
    );

    // Each typed command starts a fresh helper process. Repeat the complete
    // comparison so the helper reopen is itself covered by the immutable
    // DB/WAL/SHM inventory assertion.
    assert_rusqlite_helper_parity(&helper_authority, &runtime);
    assert_artifacts_unchanged(
        &helper_snapshot_before_reads,
        &inventory_database_artifacts(&helper_snapshot_copy),
        "process-isolated rusqlite graph reads and helper reopen",
    );
    assert_artifacts_unchanged(
        &source_authority_before_probes,
        &inventory_database_artifacts(&checkpointed_source),
        "all graph read-only parity probes must preserve the source authority",
    );
}

async fn seed_latest_schema_graph(path: &Path) -> Database {
    let (database, migrated) = common::initialize_test_database(path)
        .await
        .expect("initialize latest-schema graph fixture");
    assert!(
        !migrated,
        "a newly initialized latest-schema graph fixture must not report a migration"
    );

    let (primary, secondary) = graph_sample_nodes();
    database
        .insert_nodes(&[primary.clone(), secondary.clone()])
        .await
        .expect("seed graph sample nodes through Database API");
    database
        .insert_edge(&Edge {
            source: secondary.id.clone(),
            target: primary.id.clone(),
            kind: EdgeKind::Calls,
            line: Some(11),
        })
        .await
        .expect("seed graph sample edge through Database API");
    database
        .upsert_file(&FileRecord {
            path: GRAPH_FILE.to_string(),
            content_hash: "storage-runtime-graph-fixture-v1".to_string(),
            size: 777,
            modified_at: 1_700_000_000,
            indexed_at: 1_700_000_003,
            node_count: 2,
        })
        .await
        .expect("seed graph file record through Database API");
    for &(key, value) in METADATA_ROWS {
        database
            .set_metadata(key, value)
            .await
            .unwrap_or_else(|error| panic!("seed graph metadata {key:?}: {error}"));
    }

    database
}

fn graph_sample_nodes() -> (Node, Node) {
    let mut primary = common::sample_node(PRIMARY_NODE_ID, "paritytoken paritytoken", GRAPH_FILE);
    primary.qualified_name = "crate::storage_runtime::parity_primary".to_string();
    primary.start_line = 40;
    primary.attrs_start_line = 39;
    primary.end_line = 46;
    primary.docstring = Some("primary graph read-only parity node".to_string());
    primary.signature = Some("fn parity_primary()".to_string());

    let mut secondary = common::sample_node(SECONDARY_NODE_ID, "paritytoken", GRAPH_FILE);
    secondary.qualified_name = "crate::storage_runtime::parity_secondary".to_string();
    secondary.start_line = 11;
    secondary.attrs_start_line = 10;
    secondary.end_line = 16;
    secondary.docstring = Some("secondary graph read-only parity node".to_string());
    secondary.signature = Some("fn parity_secondary()".to_string());

    (primary, secondary)
}

async fn assert_graph_api_evidence(database: &Database) {
    let stats = database
        .get_stats()
        .await
        .expect("read graph stats from canonical runtime");
    assert_eq!(stats.node_count, 2, "fixture must contain two graph nodes");
    assert_eq!(stats.edge_count, 1, "fixture must contain one graph edge");
    assert_eq!(stats.file_count, 1, "fixture must contain one graph file");

    let node = database
        .get_node_by_id(PRIMARY_NODE_ID)
        .await
        .expect("read graph node by id from canonical runtime")
        .expect("seeded primary graph node must exist");
    assert_eq!(node.id, PRIMARY_NODE_ID, "node lookup must preserve its id");
    let ordered_nodes = database
        .get_nodes_by_file(GRAPH_FILE)
        .await
        .expect("read graph nodes ordered by file from canonical runtime");
    assert_eq!(
        ordered_nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        vec![SECONDARY_NODE_ID, PRIMARY_NODE_ID],
        "fixture must exercise Database::get_nodes_by_file start-line ordering"
    );
    let search = database
        .search_nodes("paritytoken", usize::from(FTS_LIMIT))
        .await
        .expect("perform canonical runtime FTS graph search");
    assert_eq!(search.len(), 2, "fixture FTS search must return both nodes");
}

fn capture_runtime_evidence(database: &CanonicalGraphReader) -> GraphRuntimeEvidence {
    GraphRuntimeEvidence {
        schema: SchemaEvidence {
            schema_version: scalar_i64(database, "PRAGMA schema_version"),
            user_version: scalar_i64(database, "PRAGMA user_version"),
            objects: schema_objects(database),
        },
        foreign_keys: scalar_i64(database, "PRAGMA foreign_keys") != 0,
        page_size: u32::try_from(scalar_i64(database, "PRAGMA page_size"))
            .expect("canonical SQLite page size must fit u32"),
        journal_mode: scalar_string(database, "PRAGMA journal_mode"),
        quick_check: string_rows(database, "PRAGMA quick_check(1000)"),
        integrity_check: string_rows(database, "PRAGMA integrity_check(1000)"),
        table_counts: table_counts(database),
        fts_matches: fts_matches(database),
    }
}

fn scalar_i64(database: &CanonicalGraphReader, sql: &str) -> i64 {
    let rows = database.query(sql, Vec::new());
    let row = rows
        .rows
        .first()
        .unwrap_or_else(|| panic!("fixed runtime scalar query returned no row: {sql:?}"));
    row_i64(row, 0, sql)
}

fn scalar_string(database: &CanonicalGraphReader, sql: &str) -> String {
    let rows = database.query(sql, Vec::new());
    let row = rows
        .rows
        .first()
        .unwrap_or_else(|| panic!("fixed runtime string query returned no row: {sql:?}"));
    row_string(row, 0, sql)
}

fn string_rows(database: &CanonicalGraphReader, sql: &str) -> Vec<String> {
    database
        .query(sql, Vec::new())
        .rows
        .iter()
        .map(|row| row_string(row, 0, sql))
        .collect()
}

fn schema_objects(database: &CanonicalGraphReader) -> Vec<SchemaObjectEvidence> {
    database
        .query(SCHEMA_SQL, Vec::new())
        .rows
        .iter()
        .map(|row| SchemaObjectEvidence {
            kind: row_string(row, 0, "schema object type"),
            name: row_string(row, 1, "schema object name"),
            table_name: row_string(row, 2, "schema object table name"),
            sql: row_optional_string(row, 3, "schema object SQL"),
        })
        .collect()
}

fn table_counts(database: &CanonicalGraphReader) -> BTreeMap<String, u64> {
    let tables = [
        ("nodes", "SELECT COUNT(*) FROM nodes"),
        ("edges", "SELECT COUNT(*) FROM edges"),
        ("files", "SELECT COUNT(*) FROM files"),
        ("unresolved_refs", "SELECT COUNT(*) FROM unresolved_refs"),
        ("vectors", "SELECT COUNT(*) FROM vectors"),
        ("metadata", "SELECT COUNT(*) FROM metadata"),
        ("nodes_fts", "SELECT COUNT(*) FROM nodes_fts"),
    ];
    let mut counts = BTreeMap::new();
    for (table, sql) in tables {
        let count = u64::try_from(scalar_i64(database, sql))
            .unwrap_or_else(|_| panic!("runtime row count for {table} must not be negative"));
        counts.insert(table.to_string(), count);
    }
    counts
}

fn fts_matches(database: &CanonicalGraphReader) -> Vec<FtsMatchEvidence> {
    let rows = database.query(
        FTS_SQL,
        vec![
            ExactSqlValue::Text(FTS_QUERY.to_string()),
            ExactSqlValue::Integer(i64::from(FTS_LIMIT)),
        ],
    );
    let matches = rows
        .rows
        .iter()
        .map(|row| FtsMatchEvidence {
            rowid: row_i64(row, 0, "FTS rowid"),
            rank: row_f64(row, 1, "FTS rank"),
            snippet: row_string(row, 2, "FTS snippet"),
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 2, "fixture FTS query must return both nodes");
    matches
}

fn row_i64(row: &ExactSqlRow, index: usize, context: &str) -> i64 {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) => *value,
        value => panic!("decode {context} as INTEGER, got {value:?}"),
    }
}

fn row_f64(row: &ExactSqlRow, index: usize, context: &str) -> f64 {
    match row.values.get(index) {
        Some(ExactSqlValue::Real(value)) => *value,
        value => panic!("decode {context} as REAL, got {value:?}"),
    }
}

fn row_string(row: &ExactSqlRow, index: usize, context: &str) -> String {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => value.clone(),
        value => panic!("decode {context} as TEXT, got {value:?}"),
    }
}

fn row_optional_string(row: &ExactSqlRow, index: usize, context: &str) -> Option<String> {
    match row.values.get(index) {
        Some(ExactSqlValue::Null) => None,
        Some(ExactSqlValue::Text(value)) => Some(value.clone()),
        value => panic!("decode {context} as optional TEXT, got {value:?}"),
    }
}

/// Test-owned authority over the sealed helper snapshot copy.
///
/// The shared parity protocol requires every request to carry the copied
/// snapshot's sealed provenance: a stable authority identity, the canonical
/// private staging root, the canonical copy path, and the byte length and
/// platform file identity captured after the copy was sealed. The authority
/// revalidates the copy and rebuilds that provenance from fresh file metadata
/// immediately before each helper request, so a mutated or replaced snapshot
/// fails before the helper process is even spawned.
struct HelperSnapshotAuthority {
    authority_identity: String,
    staging_root: PathBuf,
    canonical_path: PathBuf,
    sealed_inventory: DatabaseArtifactInventory,
}

impl HelperSnapshotAuthority {
    fn seal(
        root: &IsolatedTempRoot,
        snapshot_path: &Path,
        sealed_inventory: DatabaseArtifactInventory,
    ) -> Self {
        let canonical_path = snapshot_path
            .canonicalize()
            .expect("canonicalize the sealed helper graph snapshot copy");
        let staging_root = root.path().to_path_buf();
        assert!(
            canonical_path.starts_with(&staging_root),
            "the sealed helper graph snapshot copy must live inside the test-owned staging root"
        );
        assert_eq!(
            sealed_inventory.database_path, canonical_path,
            "the test-owned helper graph snapshot copy must already be its canonical path"
        );
        Self {
            authority_identity: HELPER_SNAPSHOT_AUTHORITY_IDENTITY.to_string(),
            staging_root,
            canonical_path,
            sealed_inventory,
        }
    }

    /// Revalidates the sealed copy and rebuilds the protocol-required
    /// provenance from fresh file metadata immediately before one request.
    fn revalidated_database(&self) -> CopiedDatabase {
        assert_artifacts_unchanged(
            &self.sealed_inventory,
            &inventory_database_artifacts(&self.canonical_path),
            "the sealed helper graph snapshot copy must remain untouched between parity requests",
        );
        let metadata = fs::metadata(&self.canonical_path)
            .expect("read the sealed helper graph snapshot metadata");
        CopiedDatabase {
            path: self.canonical_path.clone(),
            kind: DatabaseKind::CopiedSnapshot,
            provenance: CopiedSnapshotProvenance {
                authority_identity: self.authority_identity.clone(),
                staging_root: self.staging_root.clone(),
                canonical_path: self.canonical_path.clone(),
                byte_len: metadata.len(),
                content_digest: snapshot_content_digest(&self.canonical_path),
                file_identity: SnapshotFileIdentity::from_metadata(&metadata),
            },
        }
    }
}

fn assert_rusqlite_helper_parity(
    authority: &HelperSnapshotAuthority,
    runtime: &GraphRuntimeEvidence,
) {
    let metadata = parity_output(authority, "metadata", json!({ "type": "metadata" }));
    assert_eq!(
        PathBuf::from(
            metadata["canonical_path"]
                .as_str()
                .expect("helper metadata canonical_path must be a string"),
        ),
        authority.canonical_path,
        "helper must inspect the explicit copied graph snapshot"
    );
    assert_eq!(metadata["query_only"], json!(true));
    assert_eq!(metadata["immutable"], json!(true));
    assert!(
        metadata["sqlite_version"]
            .as_str()
            .is_some_and(|version| version.split('.').all(|part| part.parse::<u32>().is_ok())),
        "helper must report an SQLite version"
    );
    assert!(
        metadata["compile_options"]
            .as_array()
            .is_some_and(|options| options.iter().any(|option| option == "ENABLE_FTS5")),
        "helper must be built with FTS5 to inspect graph snapshots"
    );

    let schema = parity_output(authority, "schema", json!({ "type": "schema" }));
    assert_eq!(
        schema["schema_version"],
        json!(runtime.schema.schema_version)
    );
    assert_eq!(schema["user_version"], json!(runtime.schema.user_version));
    let expected_objects = runtime
        .schema
        .objects
        .iter()
        .map(|object| {
            json!({
                "kind": object.kind,
                "name": object.name,
                "table_name": object.table_name,
                "sql": object.sql,
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(schema["objects"], json!(expected_objects));

    let foreign_keys = parity_output(authority, "foreign-keys", json!({ "type": "foreign_keys" }));
    assert_eq!(
        foreign_keys["enabled"],
        json!(runtime.foreign_keys),
        "rusqlite helper foreign-key state must match the canonical read-only graph runtime"
    );

    let page_size = parity_output(authority, "page-size", json!({ "type": "page_size" }));
    assert_eq!(page_size["bytes"], json!(runtime.page_size));

    let journal_mode = parity_output(authority, "journal-mode", json!({ "type": "journal_mode" }));
    assert_eq!(
        runtime.journal_mode, "wal",
        "the canonical graph store must retain its configured WAL source mode"
    );
    assert_eq!(
        journal_mode["source_header"]["mode"],
        json!(runtime.journal_mode),
        "the copied source header journal mode must match the canonical WAL authority"
    );
    assert_eq!(journal_mode["source_header"]["read_version"], json!(2));
    assert_eq!(journal_mode["source_header"]["write_version"], json!(2));
    assert_eq!(
        journal_mode["immutable_effective_mode"],
        json!("delete"),
        "immutable SQLite cannot create or consume the WAL/SHM sidecars required for effective WAL"
    );
    assert_eq!(
        journal_mode["mode"], journal_mode["immutable_effective_mode"],
        "the protocol-v1 mode compatibility field must remain the explicitly named immutable effective mode"
    );
    assert_eq!(
        journal_mode["normalization"],
        json!("wal_source_immutable_delete"),
        "the protocol must explicitly normalize source-header WAL to immutable effective DELETE"
    );

    for (request_id, check, expected) in [
        ("integrity-quick", "quick", &runtime.quick_check),
        ("integrity-full", "full", &runtime.integrity_check),
    ] {
        let integrity = parity_output(
            authority,
            request_id,
            json!({ "type": "integrity", "check": check }),
        );
        assert_eq!(integrity["check"], json!(check));
        assert_eq!(integrity["findings"], json!(expected));
    }

    for (table, expected_count) in &runtime.table_counts {
        let counts = parity_output(
            authority,
            &format!("count-{table}"),
            json!({ "type": "row_parity", "table": table }),
        );
        assert_eq!(counts["table"], json!(table));
        assert_eq!(counts["row_count"], json!(expected_count));
    }

    let fts = parity_output(
        authority,
        "fts",
        json!({
            "type": "fts_parity",
            "table": "nodes",
            "query": FTS_QUERY,
            "limit": FTS_LIMIT,
        }),
    );
    assert_eq!(fts["table"], json!("nodes"));
    let matches = fts["matches"]
        .as_array()
        .expect("helper FTS output must contain a matches array");
    assert_eq!(matches.len(), runtime.fts_matches.len());
    for (index, (observed, expected)) in matches.iter().zip(&runtime.fts_matches).enumerate() {
        assert_eq!(observed["rowid"], json!(expected.rowid));
        assert_eq!(observed["snippet"], json!(expected.snippet));
        assert_float_eq(
            observed["rank"]
                .as_f64()
                .expect("helper FTS rank must be a number"),
            expected.rank,
            &format!("helper FTS rank at result {index}"),
        );
    }
}

fn parity_output(authority: &HelperSnapshotAuthority, request_id: &str, command: Value) -> Value {
    let expected_output_type = command["type"].clone();
    let database = serde_json::to_value(authority.revalidated_database())
        .expect("serialize the sealed helper graph snapshot database");
    let expected_verified_snapshot = json!({
        "authority_identity": database["provenance"]["authority_identity"].clone(),
        "canonical_path": database["provenance"]["canonical_path"].clone(),
        "byte_len": database["provenance"]["byte_len"].clone(),
        "content_digest": database["provenance"]["content_digest"].clone(),
        "file_identity": database["provenance"]["file_identity"].clone(),
    });
    let response = invoke_rusqlite_parity(&json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request_id,
        "database": database,
        "command": command,
    }));
    assert_eq!(response["protocol_version"], json!(PROTOCOL_VERSION));
    assert_eq!(response["request_id"], json!(request_id));
    assert_eq!(
        response["status"],
        json!("ok"),
        "typed helper command {request_id:?} failed: {response:?}"
    );
    assert_eq!(
        response["verified_snapshot"], expected_verified_snapshot,
        "typed helper command {request_id:?} must independently revalidate the sealed graph snapshot provenance"
    );
    let output = response.get("output").cloned().unwrap_or_else(|| {
        panic!("typed helper command {request_id:?} omitted output: {response:?}")
    });
    assert_eq!(
        output["type"], expected_output_type,
        "typed helper command {request_id:?} returned the wrong output variant: {output:?}"
    );
    output
}

fn copy_checkpointed_graph_snapshot(
    source_path: &Path,
    source_inventory: &DatabaseArtifactInventory,
    snapshot_path: &Path,
    label: &str,
) -> DatabaseArtifactInventory {
    fs::copy(source_path, snapshot_path)
        .unwrap_or_else(|error| panic!("copy checkpointed {label} database snapshot: {error}"));
    let snapshot_inventory = inventory_database_artifacts(snapshot_path);
    assert_checkpointed_snapshot(&snapshot_inventory, label);
    assert_copy_matches_checkpointed_source(source_inventory, &snapshot_inventory);
    snapshot_inventory
}

fn assert_checkpointed_snapshot(inventory: &DatabaseArtifactInventory, context: &str) {
    assert!(
        inventory
            .artifacts
            .get(&DatabaseArtifactKind::Database)
            .expect("inventory must include main database")
            .is_some(),
        "{context} must include a main database artifact"
    );
    for kind in [DatabaseArtifactKind::Wal, DatabaseArtifactKind::Shm] {
        assert!(
            inventory
                .artifacts
                .get(&kind)
                .expect("inventory must include SQLite sidecar")
                .is_none(),
            "{context} must be checkpointed before copying; found {kind:?} sidecar"
        );
    }
}

fn assert_runtime_read_only_baseline_sidecars(
    before: &DatabaseArtifactInventory,
    after: &DatabaseArtifactInventory,
) {
    assert_eq!(
        before.database_path, after.database_path,
        "canonical read-only baseline must retain its isolated database path"
    );
    assert_eq!(
        before
            .artifacts
            .get(&DatabaseArtifactKind::Database)
            .expect("runtime baseline inventory must include database"),
        after
            .artifacts
            .get(&DatabaseArtifactKind::Database)
            .expect("runtime baseline inventory must include database"),
        "canonical read-only baseline must not rewrite its main database"
    );
    for kind in [DatabaseArtifactKind::Wal, DatabaseArtifactKind::Shm] {
        assert!(
            before
                .artifacts
                .get(&kind)
                .expect("runtime baseline inventory must include SQLite sidecar")
                .is_none(),
            "runtime baseline must start checkpointed without a {kind:?} sidecar"
        );
        assert!(
            after
                .artifacts
                .get(&kind)
                .expect("runtime baseline inventory must include SQLite sidecar")
                .is_some(),
            "canonical live-runtime readers are expected to create a {kind:?} sidecar; this behavior is recorded separately from the immutable helper no-mutation proof\nafter: {after:#?}"
        );
    }
}

fn assert_copy_matches_checkpointed_source(
    source: &DatabaseArtifactInventory,
    copy: &DatabaseArtifactInventory,
) {
    assert_eq!(
        source
            .artifacts
            .get(&DatabaseArtifactKind::Database)
            .expect("source inventory must include database"),
        copy.artifacts
            .get(&DatabaseArtifactKind::Database)
            .expect("copy inventory must include database"),
        "copy must start as the checkpointed source database bytes"
    );
}

fn assert_float_eq(observed: f64, expected: f64, context: &str) {
    let tolerance = 1e-12_f64 * observed.abs().max(expected.abs()).max(1.0);
    assert!(
        (observed - expected).abs() <= tolerance,
        "{context}: expected {expected:?}, observed {observed:?}, tolerance {tolerance:?}"
    );
}
