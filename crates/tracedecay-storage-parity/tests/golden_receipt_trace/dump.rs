//! Engine-neutral logical dump of one owned store.
//!
//! The dump reads through `tracedecay_sql`, so the same code reads with
//! whichever engine the build selected. Every non-FTS table appears, empty or
//! not, ordered by its primary key, with each value tagged by storage class.

use std::path::Path;

use serde_json::{Value as Json, json};
use tracedecay_sql::{Connection, OpenFlags, types::ValueRef};

/// A column whose stored value is wall-clock or otherwise run-dependent.
pub struct ColumnNormalization {
    pub table: &'static str,
    pub column: &'static str,
    pub reason: &'static str,
}

/// Every column the dump replaces with `"<normalized>"`. Fixture inputs are
/// fixed, so only values the runtime derives from the clock land here.
pub const NORMALIZED_COLUMNS: &[ColumnNormalization] = &[
    ColumnNormalization {
        table: "td_runtime_writer_checkpoint_v1",
        column: "committed_at_micros",
        reason: "writer stamps the commit with the wall clock",
    },
    ColumnNormalization {
        table: "td_runtime_writer_idempotency_v2",
        column: "committed_at_micros",
        reason: "writer stamps the commit with the wall clock",
    },
    ColumnNormalization {
        table: "remote_spool_frames",
        column: "nonce",
        reason: "each spooled frame is sealed under a fresh random nonce",
    },
    ColumnNormalization {
        table: "remote_spool_frames",
        column: "ciphertext",
        reason: "ciphertext depends on the random nonce",
    },
];

/// JSON object keys replaced with `"<normalized>"` wherever they appear: in
/// submit outcomes and inside JSON text columns.
pub const NORMALIZED_JSON_KEYS: &[(&str, &str)] = &[(
    "committed_at",
    "commit receipts carry the writer's wall-clock commit time",
)];

pub const NORMALIZED: &str = "<normalized>";

/// Every normalization the goldens rely on, one per line.
pub fn normalization_listing() -> String {
    let mut out = String::new();
    for rule in NORMALIZED_COLUMNS {
        out.push_str(&format!("column {}.{}: {}\n", rule.table, rule.column, rule.reason));
    }
    for (key, reason) in NORMALIZED_JSON_KEYS {
        out.push_str(&format!("json key {key}: {reason}\n"));
    }
    out.push_str("path <store temp dir>: replaced with $STORE in errors and text values\n");
    out
}

/// Replaces the fixture's temp directory, which differs per run.
pub fn scrub_path(text: &str, store_dir: &Path) -> String {
    let mut text = text.to_owned();
    for dir in [store_dir.to_path_buf(), canonical(store_dir)] {
        let dir = dir.display().to_string();
        if !dir.is_empty() {
            text = text.replace(&dir, "$STORE");
        }
    }
    text
}

fn canonical(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Replaces every normalized key in `value`, returning whether any matched.
pub fn normalize_json(value: &mut Json) -> bool {
    match value {
        Json::Object(map) => {
            let mut changed = false;
            for (key, entry) in map.iter_mut() {
                if NORMALIZED_JSON_KEYS.iter().any(|(name, _)| name == key) {
                    *entry = Json::String(NORMALIZED.to_owned());
                    changed = true;
                } else {
                    changed |= normalize_json(entry);
                }
            }
            changed
        }
        Json::Array(items) => items
            .iter_mut()
            .fold(false, |changed, item| normalize_json(item) | changed),
        _ => false,
    }
}

fn text_value(text: &str, store_dir: &Path) -> Json {
    let text = scrub_path(text, store_dir);
    if text.starts_with(['{', '['])
        && let Ok(mut parsed) = serde_json::from_str::<Json>(&text)
        && normalize_json(&mut parsed)
    {
        return json!({ "t": parsed.to_string() });
    }
    json!({ "t": text })
}

fn typed(value: ValueRef<'_>, store_dir: &Path) -> Json {
    match value {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(value) => json!({ "i": value }),
        ValueRef::Real(value) => json!({ "r": value }),
        ValueRef::Text(bytes) => text_value(&String::from_utf8_lossy(bytes), store_dir),
        ValueRef::Blob(bytes) => json!({ "b": hex::encode(bytes) }),
    }
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn is_dumped_table(name: &str, sql: &str, virtual_tables: &[String]) -> bool {
    !name.starts_with("sqlite_")
        && !name.starts_with("__turso")
        && !sql
            .trim_start()
            .to_ascii_uppercase()
            .starts_with("CREATE VIRTUAL TABLE")
        && !virtual_tables
            .iter()
            .any(|table| name.starts_with(&format!("{table}_")))
}

/// Renders the logical contents of the store at `path`.
pub fn logical_dump(path: &Path, store_dir: &Path) -> String {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|error| panic!("open {} for the logical dump: {error}", path.display()));
    let mut statement = connection
        .prepare(
            "SELECT name, COALESCE(sql, '') FROM sqlite_schema WHERE type = 'table' ORDER BY name",
        )
        .expect("prepare table listing");
    let mut rows = statement.query([]).expect("list tables");
    let mut tables = Vec::new();
    while let Some(row) = rows.next().expect("read table listing") {
        tables.push((
            row.get::<_, String>(0).expect("table name"),
            row.get::<_, String>(1).expect("table sql"),
        ));
    }
    drop(rows);
    drop(statement);
    let virtual_tables = tables
        .iter()
        .filter(|(_, sql)| {
            sql.trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE VIRTUAL TABLE")
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let mut out = String::new();
    for (table, sql) in &tables {
        if !is_dumped_table(table, sql, &virtual_tables) {
            continue;
        }
        dump_table(&connection, table, store_dir, &mut out);
    }
    out
}

fn dump_table(connection: &Connection, table: &str, store_dir: &Path, out: &mut String) {
    let mut info = connection
        .prepare(&format!("PRAGMA table_info({})", quote(table)))
        .expect("prepare table_info");
    let mut rows = info.query([]).expect("read table_info");
    let mut columns = Vec::new();
    while let Some(row) = rows.next().expect("table_info row") {
        columns.push((
            row.get::<_, String>(1).expect("column name"),
            row.get::<_, i64>(5).expect("pk ordinal"),
        ));
    }
    drop(rows);
    drop(info);
    let mut key = columns.iter().filter(|(_, pk)| *pk > 0).collect::<Vec<_>>();
    key.sort_by_key(|(_, pk)| *pk);
    let order = if key.is_empty() {
        columns
            .iter()
            .map(|(name, _)| quote(name))
            .collect::<Vec<_>>()
    } else {
        key.iter().map(|(name, _)| quote(name)).collect()
    };
    let names = columns
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    let normalized = names
        .iter()
        .map(|name| {
            NORMALIZED_COLUMNS
                .iter()
                .any(|rule| rule.table == table && rule.column == *name)
        })
        .collect::<Vec<_>>();
    let select = format!(
        "SELECT {} FROM {} ORDER BY {}",
        names
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(", "),
        quote(table),
        order.join(", ")
    );
    let mut statement = connection.prepare(&select).expect("prepare table scan");
    let mut rows = statement.query([]).expect("scan table");
    let mut rendered = Vec::new();
    while let Some(row) = rows.next().expect("scan row") {
        let values = (0..names.len())
            .map(|index| {
                if normalized[index] {
                    Json::String(NORMALIZED.to_owned())
                } else {
                    typed(row.get_ref(index).expect("column value"), store_dir)
                }
            })
            .collect::<Vec<_>>();
        rendered.push(Json::Array(values).to_string());
    }
    out.push_str(&format!(
        "table {table} ({}) rows={}\n",
        names.join(", "),
        rendered.len()
    ));
    for row in rendered {
        out.push_str("  ");
        out.push_str(&row);
        out.push('\n');
    }
}
