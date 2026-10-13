use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::HostKindV1;

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeHostIdentityV1 {
    ClaudeCode,
    CursorDesktop,
    CursorCloud,
    Codex,
    Hermes,
    Kiro,
    Cline,
    RooCode,
    Kilo,
    KimiCode,
    OpenCode,
    Pi,
    FactoryDroid,
}

impl NativeHostIdentityV1 {
    pub const fn host_kind(self) -> HostKindV1 {
        match self {
            Self::ClaudeCode => HostKindV1::ClaudeCode,
            Self::CursorDesktop => HostKindV1::CursorDesktop,
            Self::CursorCloud => HostKindV1::CursorCloud,
            Self::Codex => HostKindV1::Codex,
            Self::Hermes => HostKindV1::Hermes,
            Self::Kiro => HostKindV1::Kiro,
            Self::Cline => HostKindV1::Cline,
            Self::RooCode => HostKindV1::RooCode,
            Self::Kilo => HostKindV1::Kilo,
            Self::KimiCode => HostKindV1::KimiCode,
            Self::OpenCode => HostKindV1::OpenCode,
            Self::Pi => HostKindV1::Pi,
            Self::FactoryDroid => HostKindV1::FactoryDroid,
        }
    }

    /// Stable key used by native hook configuration and spool storage.
    ///
    /// Hosts sharing one CLI selector retain distinct keys so their native
    /// identities cannot alias in persisted hook state.
    pub const fn hook_key(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::CursorDesktop => "cursor-desktop",
            Self::CursorCloud => "cursor-cloud",
            Self::Codex => "codex",
            Self::Hermes => "hermes",
            Self::Kiro => "kiro",
            Self::Cline => "cline",
            Self::RooCode => "roo-code",
            Self::Kilo => "kilo",
            Self::KimiCode => "kimi",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
            Self::FactoryDroid => "droid",
        }
    }
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HostComponentV1 {
    Core,
    Agent,
    ContextMcp,
    OperatorMcp,
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostDescriptorV1 {
    cli_id: String,
    slug: String,
    components: Vec<HostComponentV1>,
}

impl HostDescriptorV1 {
    pub fn cli_id(&self) -> &str {
        &self.cli_id
    }

    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub fn components(&self) -> &[HostComponentV1] {
        &self.components
    }
}

impl HostKindV1 {
    pub const fn native_identity(self) -> Option<NativeHostIdentityV1> {
        match self {
            Self::ClaudeCode => Some(NativeHostIdentityV1::ClaudeCode),
            Self::CursorDesktop => Some(NativeHostIdentityV1::CursorDesktop),
            Self::CursorCloud => Some(NativeHostIdentityV1::CursorCloud),
            Self::Codex => Some(NativeHostIdentityV1::Codex),
            Self::Hermes => Some(NativeHostIdentityV1::Hermes),
            Self::Kiro => Some(NativeHostIdentityV1::Kiro),
            // None of these hosts owns a native hook identity: the Cline family
            // is an alias surface, the Gemini extension declares no hook route,
            // and Copilot publishes no third-party hook surface at all, so
            // persisting a hook key for any of them would name a spool no event
            // can ever reach. ChatGPT shares the gap: its portable plugin
            // bundle declares no hooks and no checked-in native fixture proves
            // a ChatGPT event route exists.
            Self::Devin
            | Self::Zed
            | Self::Antigravity
            | Self::Vibe
            | Self::ClineFamily
            | Self::Gemini
            | Self::Copilot
            | Self::ChatGpt => None,
            Self::Cline => Some(NativeHostIdentityV1::Cline),
            Self::RooCode => Some(NativeHostIdentityV1::RooCode),
            Self::Kilo => Some(NativeHostIdentityV1::Kilo),
            Self::KimiCode => Some(NativeHostIdentityV1::KimiCode),
            Self::OpenCode => Some(NativeHostIdentityV1::OpenCode),
            Self::Pi => Some(NativeHostIdentityV1::Pi),
            Self::FactoryDroid => Some(NativeHostIdentityV1::FactoryDroid),
        }
    }

    pub fn descriptor(self) -> HostDescriptorV1 {
        use HostComponentV1::{Agent, ContextMcp, Core, OperatorMcp};

        let (cli_id, slug, components) = match self {
            HostKindV1::ClaudeCode => {
                ("claude", "claude-code", vec![Core, ContextMcp, OperatorMcp])
            }
            HostKindV1::CursorDesktop => (
                "cursor",
                "cursor-desktop",
                vec![Core, Agent, ContextMcp, OperatorMcp],
            ),
            HostKindV1::CursorCloud => ("cursor", "cursor-cloud", vec![]),
            HostKindV1::Codex => ("codex", "codex", vec![Core, ContextMcp, OperatorMcp]),
            HostKindV1::Devin => ("devin", "devin", vec![ContextMcp]),
            HostKindV1::Zed => ("zed", "zed", vec![ContextMcp]),
            HostKindV1::Antigravity => ("antigravity", "antigravity", vec![ContextMcp]),
            HostKindV1::Vibe => ("vibe", "vibe", vec![Core, ContextMcp]),
            HostKindV1::Hermes => ("hermes", "hermes", vec![Core]),
            HostKindV1::Kiro => ("kiro", "kiro", vec![ContextMcp]),
            HostKindV1::ClineFamily => ("cline", "cline-family", vec![]),
            HostKindV1::Cline => ("cline", "cline", vec![ContextMcp]),
            HostKindV1::RooCode => ("roo-code", "roo-code", vec![ContextMcp]),
            HostKindV1::Kilo => ("kilo", "kilo", vec![ContextMcp]),
            HostKindV1::KimiCode => ("kimi", "kimi-code", vec![Core]),
            // ChatGPT registers a connector or plugin only through its own
            // interactive surfaces (developer-mode connector setup, the desktop
            // app's plugin flow); there is no host CLI or local registry file
            // TraceDecay may write. The lifecycle therefore stages the portable
            // Agent Plugins bundle as receipt-owned source and defers activation
            // to the operator, exactly Kimi's shape. There is no project-local
            // route either.
            HostKindV1::ChatGpt => ("chatgpt", "chatgpt", vec![Core]),
            HostKindV1::OpenCode => ("opencode", "opencode", vec![Core, Agent, ContextMcp]),
            // Gemini CLI owns extension registration through `gemini extensions
            // install|uninstall`; TraceDecay renders and stages the extension
            // source and drives those commands. There is no project-local route:
            // a workspace-scoped extension is installed by running the host CLI
            // inside that workspace, and the lifecycle admits only the profile
            // home as the child working directory.
            HostKindV1::Gemini => ("gemini", "gemini", vec![ContextMcp]),
            // GitHub Copilot owns its MCP registry through `copilot mcp
            // add|remove`; TraceDecay drives those commands and never merges
            // `~/.copilot/mcp-config.json` itself. The one managed artifact is the
            // receipt-owned component descriptor under `.copilot/tracedecay/`.
            // Copilot exposes no project-scoped registry command; the workspace
            // surface that does exist (`.vscode/mcp.json`) is written by the
            // operator and only ever *read* by this integration.
            HostKindV1::Copilot => ("copilot", "copilot", vec![ContextMcp]),
            // Pi registers tools, hooks, and skills through its extension API in
            // `~/.pi/agent/extensions`; there is no MCP route, so the component
            // set is Core + Agent only. The extension forwards its lifecycle
            // events to `hook-pi-event`. There is no project-local route.
            HostKindV1::Pi => ("pi", "pi", vec![Core, Agent]),
            // Factory Droid's `droid mcp add|remove` registry owns the MCP
            // document; TraceDecay drives the commands and owns only the
            // receipt-backed component descriptor, Copilot's shape exactly.
            HostKindV1::FactoryDroid => ("droid", "factory-droid", vec![Core, ContextMcp]),
        };
        HostDescriptorV1 {
            cli_id: cli_id.to_owned(),
            slug: slug.to_owned(),
            components,
        }
    }
}
