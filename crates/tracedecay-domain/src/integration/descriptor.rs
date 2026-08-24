use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{HostCapabilityRecordV1, HostKindV1, canonical_stock_host_capabilities};

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
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "identity")]
pub enum HostHookMappingV1 {
    Native(NativeHostIdentityV1),
    Unavailable(NativeHostIdentityV1),
    NotApplicable,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAssetRenderPolicyV1 {
    ManagedEmbedded,
    StagedManualPlugin,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostActivationPolicyV1 {
    Managed,
    ManualHostInstall,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostProjectRegistrationPathV1 {
    ClaudeProjectDirectory,
    CursorProjectDirectory,
    CodexProjectDirectory,
    HermesProjectDirectory,
    KiroProjectDirectory,
    KimiProjectDirectory,
    OpenCodeProjectDirectory,
    Unavailable,
}

impl HostProjectRegistrationPathV1 {
    pub const fn relative_path(self) -> Option<&'static str> {
        match self {
            Self::ClaudeProjectDirectory => Some(".claude"),
            Self::CursorProjectDirectory => Some(".cursor"),
            Self::CodexProjectDirectory => Some(".codex"),
            Self::HermesProjectDirectory => Some(".hermes"),
            Self::KiroProjectDirectory => Some(".kiro"),
            Self::KimiProjectDirectory => Some(".kimi-code"),
            Self::OpenCodeProjectDirectory => Some(".config/opencode"),
            Self::Unavailable => None,
        }
    }
}

#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostDescriptorV1 {
    host: HostKindV1,
    cli_id: String,
    slug: String,
    hook: HostHookMappingV1,
    capabilities: Vec<HostCapabilityRecordV1>,
    components: Vec<HostComponentV1>,
    asset_render_policy: HostAssetRenderPolicyV1,
    activation_policy: HostActivationPolicyV1,
    project_registration_path: HostProjectRegistrationPathV1,
}

impl HostDescriptorV1 {
    pub const fn host(&self) -> HostKindV1 {
        self.host
    }

    pub fn cli_id(&self) -> &str {
        &self.cli_id
    }

    pub fn slug(&self) -> &str {
        &self.slug
    }

    pub const fn hook(&self) -> HostHookMappingV1 {
        self.hook
    }

    pub fn capabilities(&self) -> &[HostCapabilityRecordV1] {
        &self.capabilities
    }

    pub fn components(&self) -> &[HostComponentV1] {
        &self.components
    }

    pub const fn asset_render_policy(&self) -> HostAssetRenderPolicyV1 {
        self.asset_render_policy
    }

    pub const fn activation_policy(&self) -> HostActivationPolicyV1 {
        self.activation_policy
    }

    pub const fn project_registration_path(&self) -> HostProjectRegistrationPathV1 {
        self.project_registration_path
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
            // can ever reach.
            Self::ClineFamily | Self::Gemini | Self::Copilot => None,
            Self::Cline => Some(NativeHostIdentityV1::Cline),
            Self::RooCode => Some(NativeHostIdentityV1::RooCode),
            Self::Kilo => Some(NativeHostIdentityV1::Kilo),
            Self::KimiCode => Some(NativeHostIdentityV1::KimiCode),
            Self::OpenCode => Some(NativeHostIdentityV1::OpenCode),
        }
    }

    pub fn descriptor(self) -> HostDescriptorV1 {
        host_descriptor_v1(self)
    }
}

pub fn host_descriptor_v1(host: HostKindV1) -> HostDescriptorV1 {
    use HostActivationPolicyV1::{Managed, ManualHostInstall, Unsupported};
    use HostAssetRenderPolicyV1::{ManagedEmbedded, StagedManualPlugin, Unavailable};
    use HostComponentV1::{Agent, ContextMcp, Core, OperatorMcp};
    use HostHookMappingV1::{Native, NotApplicable};
    use HostProjectRegistrationPathV1::{
        ClaudeProjectDirectory, CodexProjectDirectory, CursorProjectDirectory,
        HermesProjectDirectory, KimiProjectDirectory, KiroProjectDirectory,
        OpenCodeProjectDirectory,
    };

    let (cli_id, slug, hook, components, asset_render_policy, activation_policy, path) = match host
    {
        HostKindV1::ClaudeCode => (
            "claude",
            "claude-code",
            Native(NativeHostIdentityV1::ClaudeCode),
            vec![Core, ContextMcp, OperatorMcp],
            ManagedEmbedded,
            Managed,
            ClaudeProjectDirectory,
        ),
        HostKindV1::CursorDesktop => (
            "cursor",
            "cursor-desktop",
            Native(NativeHostIdentityV1::CursorDesktop),
            vec![Core, Agent, ContextMcp, OperatorMcp],
            ManagedEmbedded,
            Managed,
            CursorProjectDirectory,
        ),
        HostKindV1::CursorCloud => (
            "cursor",
            "cursor-cloud",
            Native(NativeHostIdentityV1::CursorCloud),
            vec![],
            Unavailable,
            Unsupported,
            HostProjectRegistrationPathV1::Unavailable,
        ),
        HostKindV1::Codex => (
            "codex",
            "codex",
            Native(NativeHostIdentityV1::Codex),
            vec![Core, ContextMcp, OperatorMcp],
            ManagedEmbedded,
            Managed,
            CodexProjectDirectory,
        ),
        HostKindV1::Hermes => (
            "hermes",
            "hermes",
            Native(NativeHostIdentityV1::Hermes),
            vec![Core],
            ManagedEmbedded,
            Managed,
            HermesProjectDirectory,
        ),
        HostKindV1::Kiro => (
            "kiro",
            "kiro",
            Native(NativeHostIdentityV1::Kiro),
            vec![ContextMcp],
            ManagedEmbedded,
            Managed,
            KiroProjectDirectory,
        ),
        HostKindV1::ClineFamily => (
            "cline",
            "cline-family",
            NotApplicable,
            vec![],
            Unavailable,
            Unsupported,
            HostProjectRegistrationPathV1::Unavailable,
        ),
        HostKindV1::Cline => (
            "cline",
            "cline",
            HostHookMappingV1::Unavailable(NativeHostIdentityV1::Cline),
            vec![ContextMcp],
            ManagedEmbedded,
            Managed,
            HostProjectRegistrationPathV1::Unavailable,
        ),
        HostKindV1::RooCode => (
            "roo-code",
            "roo-code",
            HostHookMappingV1::Unavailable(NativeHostIdentityV1::RooCode),
            vec![ContextMcp],
            ManagedEmbedded,
            Managed,
            HostProjectRegistrationPathV1::Unavailable,
        ),
        HostKindV1::Kilo => (
            "kilo",
            "kilo",
            HostHookMappingV1::Unavailable(NativeHostIdentityV1::Kilo),
            vec![ContextMcp],
            ManagedEmbedded,
            Managed,
            HostProjectRegistrationPathV1::Unavailable,
        ),
        HostKindV1::KimiCode => (
            "kimi",
            "kimi-code",
            Native(NativeHostIdentityV1::KimiCode),
            vec![Core],
            StagedManualPlugin,
            ManualHostInstall,
            KimiProjectDirectory,
        ),
        HostKindV1::OpenCode => (
            "opencode",
            "opencode",
            Native(NativeHostIdentityV1::OpenCode),
            vec![Core, Agent, ContextMcp],
            ManagedEmbedded,
            Managed,
            OpenCodeProjectDirectory,
        ),
        // Gemini CLI owns extension registration through `gemini extensions
        // install|uninstall`; TraceDecay renders and stages the extension
        // source and drives those commands, so the assets are managed and the
        // activation is TraceDecay-driven. There is no project-local route:
        // a workspace-scoped extension is installed by running the host CLI
        // inside that workspace, and the lifecycle admits only the profile
        // home as the child working directory.
        HostKindV1::Gemini => (
            "gemini",
            "gemini",
            NotApplicable,
            vec![ContextMcp],
            ManagedEmbedded,
            Managed,
            HostProjectRegistrationPathV1::Unavailable,
        ),
        // GitHub Copilot owns its MCP registry through `copilot mcp
        // add|remove`; TraceDecay drives those commands and never merges
        // `~/.copilot/mcp-config.json` itself. The one managed artifact is the
        // receipt-owned component descriptor under `.copilot/tracedecay/`,
        // which is why the assets are `ManagedEmbedded` and the activation is
        // `Managed` — exactly Kiro's shape, for exactly Kiro's reason.
        //
        // The project registration path is `Unavailable`: Copilot exposes no
        // project-scoped registry command, and the workspace surface that does
        // exist (`.vscode/mcp.json`) is written by the operator and only ever
        // *read* by this integration. Naming a project directory here would
        // claim a registration route TraceDecay does not drive.
        HostKindV1::Copilot => (
            "copilot",
            "copilot",
            NotApplicable,
            vec![ContextMcp],
            ManagedEmbedded,
            Managed,
            HostProjectRegistrationPathV1::Unavailable,
        ),
    };
    HostDescriptorV1 {
        host,
        cli_id: cli_id.to_owned(),
        slug: slug.to_owned(),
        hook,
        capabilities: canonical_stock_host_capabilities(host).to_vec(),
        components,
        asset_render_policy,
        activation_policy,
        project_registration_path: path,
    }
}

pub fn host_descriptors_v1() -> Vec<HostDescriptorV1> {
    HostKindV1::ALL.map(host_descriptor_v1).to_vec()
}
