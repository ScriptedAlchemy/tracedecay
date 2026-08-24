use serde_json::Value;
use tracedecay::diagnostics::lsp::{adapters as lsp_adapters, broker as lsp_broker};

use crate::cli::LspAction;

pub(crate) fn handle_lsp_action(action: LspAction) -> tracedecay::errors::Result<()> {
    match action {
        LspAction::Servers { json } => print_lsp_servers(json)?,
    }
    Ok(())
}

fn print_lsp_servers(json: bool) -> tracedecay::errors::Result<()> {
    let adapters = lsp_adapters::builtin_adapters();
    if json {
        let rows: Vec<_> = adapters.iter().map(lsp_server_row).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print_lsp_servers_table(&adapters);
    }
    Ok(())
}

fn lsp_server_row(adapter: &lsp_adapters::LspAdapterDefinition) -> Value {
    serde_json::json!({
        "language": adapter.language,
        "language_id": adapter.language_id,
        "command": adapter.command,
        "args": adapter.args,
        "available": lsp_broker::command_available(&adapter.command),
        "extensions": adapter.extensions,
        "root_markers": adapter.root_markers,
        "install_options": adapter.install_options,
    })
}

fn print_lsp_servers_table(adapters: &[lsp_adapters::LspAdapterDefinition]) {
    println!(
        "{:<14} {:<12} {:<28} install",
        "language", "available", "command"
    );
    for adapter in adapters {
        let install = adapter
            .install_options
            .first()
            .map(|option| option.command.as_str())
            .unwrap_or("");
        println!(
            "{:<14} {:<12} {:<28} {}",
            adapter.language,
            if lsp_broker::command_available(&adapter.command) {
                "yes"
            } else {
                "no"
            },
            adapter.command,
            install
        );
    }
}
