#![cfg(feature = "test-transport")]

//! `tracedecay_config` as an MCP client sees it.
//!
//! Each case sends `tools/call` through the production server. The payload is
//! the compact JSON (or markdown) the client receives, compared to a literal.
//!
//! The handler's line number is the first line whose text starts with the leaf
//! key, not the line of the dotted path. `match_count` drops `found: false`
//! rows and keeps parse errors. Extensions other than `.toml` and `.json` are
//! omitted, and the scan does not consult gitignore.

use crate::support::{
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;

const CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool config ...` \
(`tracedecay tool config --help` for parameters). If MCP calls keep failing or timing out, fall \
back to that CLI instead of querying .tracedecay databases directly.";

const APP_TOML: &str = r#"name = "top"
ratio = 1.5
enabled = false
ports = [8080, 9090]
offset = -4
released = 2024-05-06T07:08:09Z

[package]
name = "widget"
version = "9.9.9"

[tool.widget]
version = "0.1.0"

[dependencies]
tokio = { version = "1.40.0", features = ["rt"] }
"#;

const TSCONFIG_JSON: &str = r#"{
  "compilerOptions": {
    "strict": true,
    "jsx": "preserve"
  },
  "include": ["src", "tests"]
}
"#;

const JSON_HIT: &str = r#"{
  "name": "json-hit"
}
"#;

const INLINE_JSON: &str = "{\"name\":\"inline\"}\n";
const APP_CASE_TOML: &str = "title = \"Upper\"\n";
const LEFT_TOML: &str = "name = \"left\"\n";
const RIGHT_TOML: &str = "other = \"right\"\n";
const TOML_HIT: &str = "name = \"toml-hit\"\n";
const SECRET_TOML: &str = "token = \"SECRET_TOKEN\"\n";
const BROKEN_TOML: &str = "version = [\n";
const BROKEN_JSON: &str = "{ \"ok\":";
const YAML_TEXT: &str = "name: yaml-hit\n";
const TXT_TEXT: &str = "name = \"txt-hit\"\n";

const BROKEN_TOML_ERROR: &str = "toml parse error: TOML parse error at line 1, column 12\n  |\n1 | version = [\n  |            ^\nunclosed array, expected `]`\n";
const BROKEN_JSON_ERROR: &str = "json parse error: EOF while parsing a value at line 1 column 7";

#[cfg(windows)]
const MISSING_OS_ERROR: &str = "The system cannot find the file specified. (os error 2)";
#[cfg(not(windows))]
const MISSING_OS_ERROR: &str = "No such file or directory (os error 2)";

const VERSION_MARKDOWN: &str = "\
**match_count:** 1

## matches
- **app.toml**
  **line:** 10
  **key:** package.version
  **value:** 9.9.9
";

const EMPTY_MARKDOWN: &str = "\
**match_count:** 0
matches: none
";

fn write_config_project(project: &Path) {
    fs::create_dir_all(project.join("mixed")).unwrap();
    fs::create_dir_all(project.join("nested")).unwrap();
    fs::create_dir_all(project.join("secret")).unwrap();
    fs::create_dir_all(project.join("emptydir")).unwrap();
    fs::write(project.join(".gitignore"), "secret/\n").unwrap();
    fs::write(project.join("app.toml"), APP_TOML).unwrap();
    fs::write(project.join("tsconfig.json"), TSCONFIG_JSON).unwrap();
    fs::write(project.join("inline.json"), INLINE_JSON).unwrap();
    fs::write(project.join("App.TOML"), APP_CASE_TOML).unwrap();
    fs::write(project.join("nested/left.toml"), LEFT_TOML).unwrap();
    fs::write(project.join("nested/right.toml"), RIGHT_TOML).unwrap();
    fs::write(project.join("mixed/a.toml"), TOML_HIT).unwrap();
    fs::write(project.join("mixed/b.json"), JSON_HIT).unwrap();
    fs::write(project.join("mixed/broken.toml"), BROKEN_TOML).unwrap();
    fs::write(project.join("mixed/c.yml"), YAML_TEXT).unwrap();
    fs::write(project.join("mixed/d.txt"), TXT_TEXT).unwrap();
    fs::write(project.join("secret/hidden.toml"), SECRET_TOML).unwrap();
    fs::write(project.join("broken.json"), BROKEN_JSON).unwrap();
}

async fn config(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_config", arguments).await
}

fn tokens(bytes: usize) -> u64 {
    u64::try_from(bytes).expect("file size fits") / 4
}

fn assert_json(response: &Value, expected: &Value, touched: &[usize]) {
    assert!(
        response["error"].is_null(),
        "config JSON call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("config content: {response}"));
    assert_eq!(content[0]["type"], "text", "{response}");
    let text = content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("config text: {response}"));
    assert_eq!(text, expected.to_string(), "{response}");
    let before: u64 = touched.iter().copied().map(tokens).sum();
    if before == 0 {
        assert_eq!(content.len(), 1, "{response}");
        return;
    }
    let footer = format!(
        "\ntracedecay_metrics: before={before} after={}",
        text.len() / 4
    );
    assert_eq!(content[1]["type"], "text", "{response}");
    assert_eq!(
        content.get(1).and_then(|item| item["text"].as_str()),
        Some(footer.as_str()),
        "{response}"
    );
    assert_eq!(content.len(), 2, "{response}");
}

fn assert_markdown(response: &Value, text: &str, touched: &[usize]) {
    assert!(
        response["error"].is_null(),
        "config markdown call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("config content: {response}"));
    assert_eq!(content[0]["type"], "text", "{response}");
    assert_eq!(content[0]["text"], text, "{response}");
    let before: u64 = touched.iter().copied().map(tokens).sum();
    if before == 0 {
        assert_eq!(content.len(), 1, "{response}");
        return;
    }
    let footer = format!(
        "\ntracedecay_metrics: before={before} after={}",
        text.len() / 4
    );
    assert_eq!(content[1]["type"], "text", "{response}");
    assert_eq!(
        content.get(1).and_then(|item| item["text"].as_str()),
        Some(footer.as_str()),
        "{response}"
    );
    assert_eq!(content.len(), 2, "{response}");
}

fn invalid_params(message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": -32602,
            "message": message,
            "data": {
                "tool": "tracedecay_config",
                "reason_code": "missing_required_parameter",
                "retryable": false,
                "detail": message,
            }
        }
    })
}

fn execution_failed(message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": -32603,
            "message": message,
            "data": {
                "tool": "tracedecay_config",
                "cli_fallback": CLI_FALLBACK,
            }
        }
    })
}

fn hit(file: &str, key: &str, value: Value, line: Option<u32>) -> Value {
    json!({
        "file": file,
        "key": key,
        "value": value,
        "line": line,
    })
}

fn miss(file: &str, key: &str) -> Value {
    json!({
        "file": file,
        "key": key,
        "value": Value::Null,
        "found": false,
    })
}

fn payload(match_count: u64, matches: Value) -> Value {
    json!({
        "match_count": match_count,
        "matches": matches,
    })
}

fn empty_payload() -> Value {
    payload(0, json!([]))
}

#[tokio::test]
async fn tracedecay_config_reports_literal_values_and_typed_failures() {
    let fixture = production_composition_fixture_with_sources(write_config_project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production config server");
    let project_root = server.cg().await.project_root().to_path_buf();

    let missing_key = config(&server, json!({"path": "app.toml", "format": "json"})).await;
    assert_eq!(
        missing_key,
        invalid_params("missing required parameter: key")
    );

    let non_string_key = config(
        &server,
        json!({"key": 1, "path": "app.toml", "format": "json"}),
    )
    .await;
    assert_eq!(
        non_string_key,
        invalid_params("missing required parameter: key")
    );

    let missing_locator = config(&server, json!({"key": "package.name", "format": "json"})).await;
    assert_eq!(
        missing_locator,
        invalid_params("missing required parameter: 'path' or 'glob'")
    );

    let non_string_path = config(
        &server,
        json!({"key": "package.name", "path": true, "format": "json"}),
    )
    .await;
    assert_eq!(
        non_string_path,
        invalid_params("missing required parameter: 'path' or 'glob'")
    );

    let both_locators = config(
        &server,
        json!({
            "key": "package.name",
            "path": "app.toml",
            "glob": "nested/*.toml",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        both_locators,
        execution_failed(
            "tool execution failed: config error: tracedecay_config: 'path' and 'glob' are mutually exclusive"
        )
    );

    let dotted_path = config(
        &server,
        json!({"key": "package.name", "path": "./app.toml", "format": "json"}),
    )
    .await;
    assert_eq!(
        dotted_path,
        execution_failed(
            "tool execution failed: config error: path './app.toml' is not normalized"
        )
    );

    let parent_path = config(
        &server,
        json!({"key": "token", "path": "../outside.toml", "format": "json"}),
    )
    .await;
    assert_eq!(
        parent_path,
        execution_failed(
            "tool execution failed: config error: path '../outside.toml' is not normalized"
        )
    );

    let bad_glob = config(
        &server,
        json!({"key": "package.name", "glob": "[", "format": "json"}),
    )
    .await;
    assert_eq!(
        bad_glob,
        execution_failed(&invalid_glob_message(&project_root))
    );

    let missing_file = project_root.join("no-such.toml");
    let absent = config(
        &server,
        json!({"key": "package.name", "path": "no-such.toml", "format": "json"}),
    )
    .await;
    assert_eq!(
        absent,
        execution_failed(&format!(
            "tool execution failed: config error: failed to canonicalize project path '{}': {MISSING_OS_ERROR}",
            missing_file.display()
        ))
    );

    let outside = project_root
        .parent()
        .expect("project has a parent")
        .join("outside-abs.toml");
    fs::write(&outside, "token = \"ABS_SECRET\"\n").unwrap();
    let outside_arg = outside.display().to_string();
    let absolute_escape = config(
        &server,
        json!({"key": "token", "path": outside_arg, "format": "json"}),
    )
    .await;
    assert_eq!(
        absolute_escape,
        execution_failed(&format!(
            "tool execution failed: config error: path '{outside_arg}' escapes project root '{}'",
            project_root.display()
        ))
    );

    #[cfg(unix)]
    {
        let linked = project_root
            .parent()
            .expect("project has a parent")
            .join("linked-outside");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join("secret.toml"), "token = \"SYMLINK_SECRET\"\n").unwrap();
        symlink(&linked, project_root.join("escape")).unwrap();
        let symlink_escape = config(
            &server,
            json!({"key": "token", "path": "escape/secret.toml", "format": "json"}),
        )
        .await;
        assert_eq!(
            symlink_escape,
            execution_failed(&format!(
                "tool execution failed: config error: path 'escape/secret.toml' escapes project root '{}'",
                project_root.display()
            ))
        );
    }

    let version = payload(
        1,
        json!([hit("app.toml", "package.version", json!("9.9.9"), Some(10))]),
    );
    assert_json(
        &config(
            &server,
            json!({"key": "package.version", "path": "app.toml", "format": "json"}),
        )
        .await,
        &version,
        &[APP_TOML.len()],
    );
    assert_markdown(
        &config(
            &server,
            json!({"key": "package.version", "path": "app.toml", "format": "markdown"}),
        )
        .await,
        VERSION_MARKDOWN,
        &[APP_TOML.len()],
    );

    // `package.name` walks to "widget". The line is the first `name =`, which
    // is the root key on line 1, not `[package]`'s name on line 9.
    assert_json(
        &config(
            &server,
            json!({"key": "package.name", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit("app.toml", "package.name", json!("widget"), Some(1))]),
        ),
        &[APP_TOML.len()],
    );
    // Value is the `[tool.widget]` version. The line is still the first
    // `version =`, which belongs to `[package]`.
    assert_json(
        &config(
            &server,
            json!({"key": "tool.widget.version", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit(
                "app.toml",
                "tool.widget.version",
                json!("0.1.0"),
                Some(10)
            )]),
        ),
        &[APP_TOML.len()],
    );

    assert_json(
        &config(
            &server,
            json!({"key": "ratio", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(1, json!([hit("app.toml", "ratio", json!(1.5), Some(2))])),
        &[APP_TOML.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "enabled", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit("app.toml", "enabled", json!(false), Some(3))]),
        ),
        &[APP_TOML.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "offset", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(1, json!([hit("app.toml", "offset", json!(-4), Some(5))])),
        &[APP_TOML.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "released", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit(
                "app.toml",
                "released",
                json!("2024-05-06T07:08:09Z"),
                Some(6)
            )]),
        ),
        &[APP_TOML.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "ports", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit("app.toml", "ports", json!([8080, 9090]), Some(4))]),
        ),
        &[APP_TOML.len()],
    );
    // Index segments have no source line of their own.
    assert_json(
        &config(
            &server,
            json!({"key": "ports.1", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(1, json!([hit("app.toml", "ports.1", json!(9090), None)])),
        &[APP_TOML.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "dependencies.tokio", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit(
                "app.toml",
                "dependencies.tokio",
                json!({"features": ["rt"], "version": "1.40.0"}),
                Some(16)
            )]),
        ),
        &[APP_TOML.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "dependencies.tokio.features", "path": "app.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit(
                "app.toml",
                "dependencies.tokio.features",
                json!(["rt"]),
                None
            )]),
        ),
        &[APP_TOML.len()],
    );

    let missing_key_in_file = payload(0, json!([miss("app.toml", "no.such")]));
    assert_json(
        &config(
            &server,
            json!({"key": "no.such", "path": "app.toml", "format": "json"}),
        )
        .await,
        &missing_key_in_file,
        &[APP_TOML.len()],
    );

    let absolute = project_root.join("app.toml").display().to_string();
    assert_json(
        &config(
            &server,
            json!({"key": "package.version", "path": absolute, "format": "json"}),
        )
        .await,
        &version,
        &[APP_TOML.len()],
    );

    assert_json(
        &config(
            &server,
            json!({"key": "compilerOptions.strict", "path": "tsconfig.json", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit(
                "tsconfig.json",
                "compilerOptions.strict",
                json!(true),
                Some(3)
            )]),
        ),
        &[TSCONFIG_JSON.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "include.1", "path": "tsconfig.json", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit("tsconfig.json", "include.1", json!("tests"), None)]),
        ),
        &[TSCONFIG_JSON.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "include.9", "path": "tsconfig.json", "format": "json"}),
        )
        .await,
        &payload(0, json!([miss("tsconfig.json", "include.9")])),
        &[TSCONFIG_JSON.len()],
    );
    // A single-line document still returns the value; the line stays null
    // because the leaf is not at the start of a line.
    assert_json(
        &config(
            &server,
            json!({"key": "name", "path": "inline.json", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit("inline.json", "name", json!("inline"), None)]),
        ),
        &[INLINE_JSON.len()],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "title", "path": "App.TOML", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit("App.TOML", "title", json!("Upper"), Some(1))]),
        ),
        &[APP_CASE_TOML.len()],
    );

    assert_json(
        &config(
            &server,
            json!({"key": "name", "glob": "nested/*.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([
                hit("nested/left.toml", "name", json!("left"), Some(1)),
                miss("nested/right.toml", "name"),
            ]),
        ),
        &[LEFT_TOML.len(), RIGHT_TOML.len()],
    );

    assert_json(
        &config(
            &server,
            json!({"key": "name", "glob": "mixed/*", "format": "json"}),
        )
        .await,
        &payload(
            3,
            json!([
                hit("mixed/a.toml", "name", json!("toml-hit"), Some(1)),
                hit("mixed/b.json", "name", json!("json-hit"), Some(2)),
                {"error": BROKEN_TOML_ERROR, "file": "mixed/broken.toml"},
            ]),
        ),
        &[TOML_HIT.len(), JSON_HIT.len()],
    );

    assert_json(
        &config(
            &server,
            json!({"key": "version", "path": "mixed/broken.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([{"error": BROKEN_TOML_ERROR, "file": "mixed/broken.toml"}]),
        ),
        &[],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "ok", "path": "broken.json", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([{"error": BROKEN_JSON_ERROR, "file": "broken.json"}]),
        ),
        &[],
    );

    // `.yml` is not parsed. A directory cannot be read. Both are an empty
    // success, not a typed error, and neither contributes a metrics footer.
    let skipped = empty_payload();
    assert_json(
        &config(
            &server,
            json!({"key": "name", "path": "mixed/c.yml", "format": "json"}),
        )
        .await,
        &skipped,
        &[],
    );
    assert_markdown(
        &config(
            &server,
            json!({"key": "name", "path": "mixed/c.yml", "format": "markdown"}),
        )
        .await,
        EMPTY_MARKDOWN,
        &[],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "name", "path": "emptydir", "format": "json"}),
        )
        .await,
        &skipped,
        &[],
    );
    assert_json(
        &config(
            &server,
            json!({"key": "name", "glob": "no-such-dir/*.toml", "format": "json"}),
        )
        .await,
        &skipped,
        &[],
    );

    // `secret/` is gitignored. The scan is a filesystem glob, so the token
    // is still returned.
    assert_json(
        &config(
            &server,
            json!({"key": "token", "glob": "secret/*.toml", "format": "json"}),
        )
        .await,
        &payload(
            1,
            json!([hit(
                "secret/hidden.toml",
                "token",
                json!("SECRET_TOKEN"),
                Some(1)
            )]),
        ),
        &[SECRET_TOML.len()],
    );

    assert_eq!(
        fs::read_to_string(project_root.join("app.toml")).expect("app.toml still readable"),
        APP_TOML
    );
    assert_eq!(
        fs::read_to_string(project_root.join("secret/hidden.toml")).expect("secret still readable"),
        SECRET_TOML
    );
}

fn invalid_glob_message(project_root: &PathBuf) -> String {
    let joined = project_root.join("[");
    let pattern = joined.to_string_lossy();
    let position = pattern.len() - 1;
    format!(
        "tool execution failed: config error: invalid glob '[': Pattern syntax error near position {position}: invalid range pattern"
    )
}
