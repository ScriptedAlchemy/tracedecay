use serde::{Deserialize, Serialize};

/// How an LSP server reports diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticMode {
    Push,
    Pull,
    PushAndPull,
}

/// Static description of an LSP adapter the dashboard broker can manage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspAdapterDefinition {
    pub language: String,
    pub language_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub install_options: Vec<LspInstallOption>,
    pub diagnostics: DiagnosticMode,
}

/// Operator-facing install hint for an LSP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspInstallOption {
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Built-in language-server adapters for the dashboard surface.
pub fn builtin_adapters() -> Vec<LspAdapterDefinition> {
    vec![
        adapter(AdapterSpec {
            language: "rust",
            language_id: "rust",
            command: "rust-analyzer",
            args: &[],
            extensions: &["rs"],
            root_markers: &["Cargo.toml"],
            install_options: &[install("rustup", "rustup component add rust-analyzer", None)],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "typescript",
            language_id: "typescript",
            command: "typescript-language-server",
            args: &["--stdio"],
            extensions: &["ts", "tsx"],
            root_markers: &["tsconfig.json", "jsconfig.json"],
            install_options: &[install(
                "npm",
                "npm install -g typescript typescript-language-server",
                None,
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "javascript",
            language_id: "javascript",
            command: "typescript-language-server",
            args: &["--stdio"],
            extensions: &["js", "jsx"],
            root_markers: &["jsconfig.json", "tsconfig.json"],
            install_options: &[install(
                "npm",
                "npm install -g typescript typescript-language-server",
                None,
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "python",
            language_id: "python",
            command: "pyright-langserver",
            args: &["--stdio"],
            extensions: &["py"],
            root_markers: &["pyrightconfig.json", "pyproject.toml"],
            install_options: &[install("npm", "npm install -g pyright", None)],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "go",
            language_id: "go",
            command: "gopls",
            args: &[],
            extensions: &["go"],
            root_markers: &["go.mod"],
            install_options: &[install(
                "go",
                "go install golang.org/x/tools/gopls@latest",
                None,
            )],
            diagnostics: DiagnosticMode::PushAndPull,
        }),
        adapter(AdapterSpec {
            language: "c",
            language_id: "c",
            command: "clangd",
            args: &[],
            extensions: &["c", "h"],
            root_markers: &["compile_commands.json"],
            install_options: &[install(
                "system package",
                "sudo apt install clangd",
                Some("Use your platform package manager on non-Debian systems."),
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "cpp",
            language_id: "cpp",
            command: "clangd",
            args: &[],
            extensions: &["cc", "cpp", "cxx", "hh", "hpp", "hxx"],
            root_markers: &["compile_commands.json"],
            install_options: &[install(
                "system package",
                "sudo apt install clangd",
                Some("Use your platform package manager on non-Debian systems."),
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "objc",
            language_id: "objective-c",
            command: "clangd",
            args: &[],
            extensions: &["m", "mm"],
            root_markers: &["compile_commands.json"],
            install_options: &[install(
                "system package",
                "sudo apt install clangd",
                Some("Use your platform package manager on non-Debian systems."),
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "zig",
            language_id: "zig",
            command: "zls",
            args: &[],
            extensions: &["zig"],
            root_markers: &["build.zig"],
            install_options: &[install(
                "package manager",
                "brew install zls",
                Some("Use your platform package manager or the zigtools/zls release for non-macOS systems."),
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "lua",
            language_id: "lua",
            command: "lua-language-server",
            args: &[],
            extensions: &["lua"],
            root_markers: &[".luarc.json"],
            install_options: &[install(
                "system package",
                "brew install lua-language-server",
                Some("Use your platform package manager on non-macOS systems."),
            )],
            diagnostics: DiagnosticMode::Push,
        }),
        adapter(AdapterSpec {
            language: "php",
            language_id: "php",
            command: "intelephense",
            args: &["--stdio"],
            extensions: &["php"],
            root_markers: &["composer.json"],
            install_options: &[install("npm", "npm install -g intelephense", None)],
            diagnostics: DiagnosticMode::Push,
        }),
    ]
}

#[derive(Clone, Copy)]
struct AdapterSpec<'a> {
    language: &'a str,
    language_id: &'a str,
    command: &'a str,
    args: &'a [&'a str],
    extensions: &'a [&'a str],
    root_markers: &'a [&'a str],
    install_options: &'a [LspInstallOption],
    diagnostics: DiagnosticMode,
}

fn adapter(spec: AdapterSpec<'_>) -> LspAdapterDefinition {
    LspAdapterDefinition {
        language: spec.language.to_string(),
        language_id: spec.language_id.to_string(),
        command: spec.command.to_string(),
        args: spec.args.iter().map(|arg| (*arg).to_string()).collect(),
        extensions: spec
            .extensions
            .iter()
            .map(|extension| (*extension).to_string())
            .collect(),
        root_markers: spec
            .root_markers
            .iter()
            .map(|marker| (*marker).to_string())
            .collect(),
        install_options: spec.install_options.to_vec(),
        diagnostics: spec.diagnostics,
    }
}

fn install(label: &str, command: &str, notes: Option<&str>) -> LspInstallOption {
    LspInstallOption {
        label: label.to_string(),
        command: command.to_string(),
        notes: notes.map(str::to_string),
    }
}
