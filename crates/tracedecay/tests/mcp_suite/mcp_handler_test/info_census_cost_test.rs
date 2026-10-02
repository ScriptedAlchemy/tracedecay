//! Project-info tools read what their answer needs from the code graph, not
//! the whole symbol census: the catalog symbols a `tracedecay_files`,
//! `tracedecay_todos`, or `tracedecay_port_status` call is served stay the
//! same when the repository grows elsewhere.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture_with_sources,
    wait_for_current_graph,
};

/// Two functions; the TODO sits inside `alpha`.
const MARKED_RS: &str = "\
pub fn alpha() {
    // TODO: finish alpha
}

pub fn beta() {}
";

const SOURCE_RS: &str = "pub fn shared() {}\n\npub fn source_only() {}\n";
const PORTED_RS: &str = "pub fn shared() {}\n";

const BULK_FILES: usize = 20;
const BULK_FUNCTIONS_PER_FILE: usize = 50;

fn write_core(project: &Path) {
    for dir in ["src", "source", "ported"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(project.join("src/marked.rs"), MARKED_RS).unwrap();
    fs::write(project.join("source/lib.rs"), SOURCE_RS).unwrap();
    fs::write(project.join("ported/lib.rs"), PORTED_RS).unwrap();
}

/// The core corpus plus a thousand marker-free functions outside every
/// directory the calls below name.
fn write_core_and_bulk(project: &Path) {
    write_core(project);
    fs::create_dir_all(project.join("bulk")).unwrap();
    for file in 0..BULK_FILES {
        let body = (0..BULK_FUNCTIONS_PER_FILE)
            .map(|function| format!("pub fn bulk_{file}_{function}() {{}}\n"))
            .collect::<String>();
        fs::write(project.join(format!("bulk/file_{file}.rs")), body).unwrap();
    }
}

struct Observed {
    payload: Value,
    catalog_symbols: u64,
}

async fn call(
    fixture: &ProductionCompositionFixture,
    tool: &str,
    mut arguments: Value,
) -> Observed {
    arguments
        .as_object_mut()
        .expect("arguments are an object")
        .insert("format".to_owned(), json!("json"));
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool, arguments)
        .await
        .expect("production MCP tools/call");
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool} failed: {:?}", response.error));
    let payload = serde_json::from_str(extract_text(&result))
        .unwrap_or_else(|error| panic!("{tool} JSON: {error}\n{result}"));
    let trailer = result["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .filter_map(|block| block["text"].as_str())
        .find_map(|text| text.strip_prefix("\ntracedecay_cost: "))
        .unwrap_or_else(|| panic!("{tool} has no cost trailer: {result}"));
    let catalog_symbols = trailer
        .split(' ')
        .find_map(|pair| pair.strip_prefix("catalog_symbols="))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("catalog_symbols missing from {trailer:?}"));
    Observed {
        payload,
        catalog_symbols,
    }
}

/// `(payload, catalog symbols)` of every probed call, in a fixed order.
async fn observe(write: fn(&Path)) -> Vec<(Value, u64)> {
    let fixture = production_composition_fixture_with_sources(write).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;
    let mut observed = Vec::new();
    for (tool, arguments) in [
        ("tracedecay_files", json!({"path": "src"})),
        ("tracedecay_files", json!({})),
        ("tracedecay_todos", json!({})),
        (
            "tracedecay_port_status",
            json!({"source_dir": "source", "target_dir": "ported"}),
        ),
    ] {
        let Observed {
            payload,
            catalog_symbols,
        } = call(&fixture, tool, arguments).await;
        observed.push((payload, catalog_symbols));
    }
    fixture.harness.shutdown().await;
    observed
}

#[tokio::test]
async fn info_tools_read_the_same_symbols_however_large_the_repository_grows() {
    let core = observe(write_core).await;
    let grown = observe(write_core_and_bulk).await;

    let files_in_src = json!({
        "count": 1,
        "layout": "grouped",
        "files": [{"path": "src/marked.rs", "symbols": 2, "bytes": MARKED_RS.len()}],
    });
    let todos = json!({
        "match_count": 1,
        "by_kind": {"TODO": 1},
        "markers": [{
            "kind": "TODO",
            "file": "src/marked.rs",
            "line": 2,
            "text": "// TODO: finish alpha",
            "enclosing": "src/marked.rs::alpha",
        }],
    });
    // Files come from the census aggregates (no symbol read); todos reads
    // only the two symbols of the one file carrying a marker; port status
    // reads the three symbols under `source` and `ported`.
    for (label, observed) in [("core", &core), ("grown", &grown)] {
        assert_eq!(observed[0], (files_in_src.clone(), 0), "{label} files");
        assert_eq!(observed[1].1, 0, "{label} unscoped files");
        assert_eq!(observed[2], (todos.clone(), 2), "{label} todos");
        assert_eq!(observed[3].1, 3, "{label} port status");
    }
    assert_eq!(
        [
            "source_count",
            "target_count",
            "matched",
            "unmatched",
            "target_only"
        ]
        .map(|field| core[3].0[field].clone()),
        [json!(2), json!(1), json!(1), json!(1), json!(0)],
        "{}",
        core[3].0
    );
    assert_eq!(
        core[3].0, grown[3].0,
        "port status ignores symbols outside its directories"
    );
    assert_eq!(
        grown[1].0["count"],
        json!(3 + BULK_FILES),
        "the unscoped listing still names every indexed file: {}",
        grown[1].0
    );
}
