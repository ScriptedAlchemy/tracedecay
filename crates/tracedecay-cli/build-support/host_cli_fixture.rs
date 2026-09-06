// Deterministic host-CLI fixture used by TraceDecay tests.
//
// Compiled by `build.rs` into a real native executable so Windows runners do
// not have to rename a shell script to `.exe` (os error 216) or depend on an
// ambient Kiro/Codex install. The child environment is cleared by
// `run_host_cli`; only `HOME` / `USERPROFILE` are admitted. Dispatch follows
// argv[0] (`kiro-cli` / `codex`) and implements install, list, and conflict
// outputs while recording arguments under the isolated home.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const FIXTURE_DIR: &str = ".tracedecay-host-cli-fixture";
const INVOCATIONS_LOG: &str = "invocations.log";
const MALFORMED_MARKER: &str = "malformed";
const CONFLICT_MARKER: &str = "conflict";

fn main() -> ExitCode {
    let raw_args: Vec<String> = env::args().collect();
    let (program, args) = raw_args
        .split_first()
        .map(|(program, args)| (program.as_str(), args.to_vec()))
        .unwrap_or(("", Vec::new()));

    let Some(home) = admitted_home() else {
        eprintln!("host-cli fixture requires HOME or USERPROFILE");
        return ExitCode::from(64);
    };

    if let Err(error) = record_invocation(&home, &args) {
        eprintln!("failed to record host-cli fixture arguments: {error}");
        return ExitCode::from(1);
    }

    if fixture_marker(&home, CONFLICT_MARKER) {
        eprintln!(r#"{{"error":"conflict","message":"ownership conflict"}}"#);
        return ExitCode::from(1);
    }

    match host_kind(program, &args) {
        HostKind::Kiro => match run_kiro(&home, &args) {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(1)
            }
        },
        HostKind::Codex => match run_codex(&home, &args) {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(1)
            }
        },
        HostKind::Unknown => {
            eprintln!("unexpected host-cli fixture invocation: {}", args.join(" "));
            ExitCode::from(64)
        }
    }
}

#[derive(Clone, Copy)]
enum HostKind {
    Kiro,
    Codex,
    Unknown,
}

fn host_kind(program: &str, args: &[String]) -> HostKind {
    let stem = Path::new(program)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(program);
    if stem == "kiro-cli" {
        return HostKind::Kiro;
    }
    if stem == "codex" {
        return HostKind::Codex;
    }
    match args.first().map(String::as_str) {
        Some("mcp") => HostKind::Kiro,
        Some("plugin") => HostKind::Codex,
        _ => HostKind::Unknown,
    }
}

fn admitted_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn fixture_root(home: &Path) -> PathBuf {
    home.join(FIXTURE_DIR)
}

fn fixture_marker(home: &Path, name: &str) -> bool {
    fixture_root(home).join(name).is_file()
}

fn record_invocation(home: &Path, args: &[String]) -> io::Result<()> {
    let root = fixture_root(home);
    fs::create_dir_all(&root)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(root.join(INVOCATIONS_LOG))?;
    writeln!(file, "{}", args.join(" "))
}

fn run_kiro(home: &Path, args: &[String]) -> Result<u8, String> {
    match args.first().map(String::as_str) {
        Some("mcp") => {}
        _ => {
            eprintln!("unexpected kiro-cli invocation: {}", args.join(" "));
            return Ok(64);
        }
    }
    match args.get(1).map(String::as_str) {
        Some("add") => kiro_mcp_add(home, args),
        Some("remove") => kiro_mcp_remove(home, args),
        Some("list") => kiro_mcp_list(home),
        _ => {
            eprintln!("unexpected kiro-cli invocation: {}", args.join(" "));
            Ok(64)
        }
    }
}

fn kiro_mcp_add(home: &Path, args: &[String]) -> Result<u8, String> {
    if args.get(6).map(String::as_str) != Some("--args")
        || args.get(7).map(String::as_str) != Some("serve")
        || args.get(8).map(String::as_str) != Some("--scope")
        || args.get(9).map(String::as_str) != Some("global")
        || args.get(10).map(String::as_str) != Some("--force")
    {
        eprintln!("unexpected kiro-cli mcp add arguments: {}", args.join(" "));
        return Ok(64);
    }
    let command = args.get(5).cloned().unwrap_or_default();
    let config = kiro_config_path(home);
    if fixture_marker(home, MALFORMED_MARKER) {
        write_parent(&config)?;
        fs::write(&config, "{not-json\n").map_err(io_err)?;
        println!("not-a-json-document");
        return Ok(0);
    }
    let preserve_other = existing_text(&config).is_some_and(|text| text.contains("\"other\""));
    let body = if preserve_other {
        format!(
            r#"{{"mcpServers":{{"other":{{"command":"other","args":[]}},"tracedecay":{{"command":"{}","args":["serve"]}}}}}}"#,
            json_escape(&command)
        )
    } else {
        format!(
            r#"{{"mcpServers":{{"tracedecay":{{"command":"{}","args":["serve"]}}}}}}"#,
            json_escape(&command)
        )
    };
    write_parent(&config)?;
    fs::write(&config, format!("{body}\n")).map_err(io_err)?;
    println!("{body}");
    Ok(0)
}

fn kiro_mcp_remove(home: &Path, args: &[String]) -> Result<u8, String> {
    if args.get(2).map(String::as_str) != Some("--name")
        || args.get(3).map(String::as_str) != Some("tracedecay")
        || args.get(4).map(String::as_str) != Some("--scope")
        || args.get(5).map(String::as_str) != Some("global")
    {
        eprintln!(
            "unexpected kiro-cli mcp remove arguments: {}",
            args.join(" ")
        );
        return Ok(64);
    }
    let config = kiro_config_path(home);
    match existing_text(&config) {
        Some(text) if text.contains("\"other\"") => {
            fs::write(
                &config,
                "{\"mcpServers\":{\"other\":{\"command\":\"other\",\"args\":[]}}}\n",
            )
            .map_err(io_err)?;
        }
        Some(_) | None => {
            let _ = fs::remove_file(&config);
        }
    }
    Ok(0)
}

fn kiro_mcp_list(home: &Path) -> Result<u8, String> {
    if fixture_marker(home, MALFORMED_MARKER) {
        println!("not-a-json-document");
        return Ok(0);
    }
    match existing_text(&kiro_config_path(home)) {
        Some(text) => print!("{text}"),
        None => println!(r#"{{"mcpServers":{{}}}}"#),
    }
    Ok(0)
}

fn run_codex(home: &Path, args: &[String]) -> Result<u8, String> {
    match args.first().map(String::as_str) {
        Some("plugin") => {}
        _ => {
            eprintln!("unexpected codex invocation: {}", args.join(" "));
            return Ok(64);
        }
    }
    match args.get(1).map(String::as_str) {
        Some("add") => codex_plugin_add(home, args),
        Some("remove") => codex_plugin_remove(home, args),
        Some("list") => codex_plugin_list(home),
        _ => {
            eprintln!("unexpected codex invocation: {}", args.join(" "));
            Ok(64)
        }
    }
}

fn codex_plugin_add(home: &Path, args: &[String]) -> Result<u8, String> {
    let Some(selector) = plugin_selector(args) else {
        eprintln!("unexpected codex plugin selector: {}", args.join(" "));
        return Ok(64);
    };
    let marketplace = marketplace_from_selector(&selector);
    let config = codex_config_path(home);
    if fixture_marker(home, MALFORMED_MARKER) {
        write_parent(&config)?;
        fs::write(&config, "this is not toml [[[\n").map_err(io_err)?;
        println!("not-a-json-document");
        return Ok(0);
    }
    let header = format!("[plugins.\"{selector}\"]");
    let mut text = existing_text(&config).unwrap_or_default();
    if !text.contains(&header) {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n{header}\nenabled = true\n"));
    }
    write_parent(&config)?;
    fs::write(&config, text).map_err(io_err)?;
    let version = plugin_version(home);
    let source = home.join(".codex/plugins/tracedecay");
    let cache = home
        .join(".codex/plugins/cache")
        .join(&marketplace)
        .join("tracedecay")
        .join(&version);
    if source.is_dir() {
        let _ = fs::remove_dir_all(&cache);
        copy_dir(&source, &cache).map_err(io_err)?;
    } else {
        fs::create_dir_all(&cache).map_err(io_err)?;
    }
    println!(r#"{{"pluginId":"{selector}","enabled":true}}"#);
    Ok(0)
}

fn codex_plugin_remove(home: &Path, args: &[String]) -> Result<u8, String> {
    let Some(selector) = plugin_selector(args) else {
        eprintln!("unexpected codex plugin selector: {}", args.join(" "));
        return Ok(64);
    };
    let marketplace = marketplace_from_selector(&selector);
    let config = codex_config_path(home);
    if let Some(text) = existing_text(&config) {
        let header = format!("[plugins.\"{selector}\"]");
        fs::write(&config, remove_toml_section(&text, &header)).map_err(io_err)?;
    }
    let cache = home
        .join(".codex/plugins/cache")
        .join(marketplace)
        .join("tracedecay");
    let _ = fs::remove_dir_all(cache);
    Ok(0)
}

fn codex_plugin_list(home: &Path) -> Result<u8, String> {
    if fixture_marker(home, MALFORMED_MARKER) {
        println!("not-a-json-document");
        return Ok(0);
    }
    let plugins = match existing_text(&codex_config_path(home)) {
        Some(text) => listed_plugin_ids(&text),
        None => Vec::new(),
    };
    let entries: Vec<String> = plugins
        .into_iter()
        .map(|id| format!(r#"{{"id":"{id}","enabled":true}}"#))
        .collect();
    println!(r#"{{"plugins":[{}]}}"#, entries.join(","));
    Ok(0)
}

fn plugin_selector(args: &[String]) -> Option<String> {
    args.iter()
        .find(|arg| arg.starts_with("tracedecay@") && arg.len() > "tracedecay@".len())
        .cloned()
}

fn marketplace_from_selector(selector: &str) -> String {
    selector
        .strip_prefix("tracedecay@")
        .unwrap_or("personal")
        .to_string()
}

fn plugin_version(home: &Path) -> String {
    let manifest = home.join(".codex/plugins/tracedecay/.codex-plugin/plugin.json");
    existing_text(&manifest)
        .and_then(|text| json_string_field(&text, "version"))
        .unwrap_or_else(|| PRODUCT_VERSION.to_string())
}

fn json_string_field(text: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let after_key = text.split_once(&needle)?.1;
    let after_colon = after_key.split_once(':')?.1;
    let after_quote = after_colon.split_once('"')?.1;
    let value = after_quote.split_once('"')?.0;
    Some(value.to_string())
}

fn listed_plugin_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(quoted) = trimmed
            .strip_prefix("[plugins.\"")
            .and_then(|rest| rest.strip_suffix("\"]"))
        {
            ids.push(quoted.to_string());
            continue;
        }
        if let Some(plain) = trimmed
            .strip_prefix("[plugins.")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            ids.push(plain.to_string());
        }
    }
    ids
}

fn remove_toml_section(text: &str, header: &str) -> String {
    let mut out = String::new();
    let mut dropping = false;
    for line in text.lines() {
        if line.trim() == header {
            dropping = true;
            continue;
        }
        if dropping && line.starts_with('[') {
            dropping = false;
        }
        if !dropping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn kiro_config_path(home: &Path) -> PathBuf {
    home.join(".kiro/settings/mcp.json")
}

fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex/config.toml")
}

fn existing_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn write_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

fn json_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            ch => out.push(ch),
        }
    }
    out
}

fn io_err(error: io::Error) -> String {
    error.to_string()
}
