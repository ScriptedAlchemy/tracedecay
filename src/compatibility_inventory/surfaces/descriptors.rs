pub(super) const LIB_SOURCE: &str = include_str!("../../lib.rs");
pub(super) const CLI_SOURCE: &str = include_str!("../../cli.rs");
pub(super) const CLI_AUTOMATION_SOURCE: &str = include_str!("../../cli/automation.rs");
pub(super) const DASHBOARD_ROUTES_SOURCE: &str = include_str!("../../dashboard/mod.rs");
pub(super) const CONFIG_SOURCE: &str = include_str!("../../config.rs");
pub(super) const USER_CONFIG_SOURCE: &str = include_str!("../../user_config.rs");
pub(super) const DOCTOR_SOURCE: &str = include_str!("../../doctor.rs");
pub(super) const REPAIR_SOURCE: &str = include_str!("../../doctor/heal.rs");
pub(super) const CARGO_SOURCE: &str = include_str!("../../../Cargo.toml");
pub(super) const RELEASE_SOURCE: &str = include_str!("../../../.github/workflows/release.yml");
pub(super) const RELEASE_BETA_SOURCE: &str =
    include_str!("../../../.github/workflows/release-beta.yml");

pub(super) const ENV_OWNER_SOURCES: &[(&str, &str)] = &[
    ("src/agents/kiro.rs", include_str!("../../agents/kiro.rs")),
    ("src/agents/mod.rs", include_str!("../../agents/mod.rs")),
    (
        "src/agents/opencode.rs",
        include_str!("../../agents/opencode.rs"),
    ),
    ("src/agents/vibe.rs", include_str!("../../agents/vibe.rs")),
    ("src/config.rs", CONFIG_SOURCE),
    ("src/daemon.rs", include_str!("../../daemon.rs")),
    (
        "src/daemon/service.rs",
        include_str!("../../daemon/service.rs"),
    ),
    (
        "src/dashboard/savings_pricing.rs",
        include_str!("../../dashboard/savings_pricing.rs"),
    ),
    (
        "src/dashboard/settings_api.rs",
        include_str!("../../dashboard/settings_api.rs"),
    ),
    (
        "src/db/connection.rs",
        include_str!("../../db/connection.rs"),
    ),
    (
        "src/external_tools.rs",
        include_str!("../../external_tools.rs"),
    ),
    (
        "src/extraction_worker.rs",
        include_str!("../../extraction_worker.rs"),
    ),
    ("src/global_db.rs", include_str!("../../global_db.rs")),
    ("src/hooks/claude.rs", include_str!("../../hooks/claude.rs")),
    (
        "src/sessions/codex_app_server.rs",
        include_str!("../../sessions/codex_app_server.rs"),
    ),
    (
        "src/sessions/cursor_agent.rs",
        include_str!("../../sessions/cursor_agent.rs"),
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SurfaceFamilyOwner {
    pub(super) kind_key: &'static str,
    pub(super) v2_owner: &'static str,
}

pub(super) const SURFACE_FAMILY_OWNERS: &[SurfaceFamilyOwner] = &[
    SurfaceFamilyOwner {
        kind_key: "cli_command",
        v2_owner: "tracedecay-tool-catalog",
    },
    SurfaceFamilyOwner {
        kind_key: "cli_flag",
        v2_owner: "tracedecay-tool-catalog",
    },
    SurfaceFamilyOwner {
        kind_key: "config",
        v2_owner: "tracedecay-application",
    },
    SurfaceFamilyOwner {
        kind_key: "dashboard_action",
        v2_owner: "dashboard",
    },
    SurfaceFamilyOwner {
        kind_key: "dashboard_panel",
        v2_owner: "dashboard",
    },
    SurfaceFamilyOwner {
        kind_key: "default",
        v2_owner: "tracedecay-application",
    },
    SurfaceFamilyOwner {
        kind_key: "doctor_action",
        v2_owner: "tracedecay-application",
    },
    SurfaceFamilyOwner {
        kind_key: "env",
        v2_owner: "tracedecay-application",
    },
    SurfaceFamilyOwner {
        kind_key: "http_action",
        v2_owner: "api",
    },
    SurfaceFamilyOwner {
        kind_key: "http_route",
        v2_owner: "api",
    },
    SurfaceFamilyOwner {
        kind_key: "installer_mutation",
        v2_owner: "host-deploy",
    },
    SurfaceFamilyOwner {
        kind_key: "mcp_schema",
        v2_owner: "tracedecay-tool-catalog",
    },
    SurfaceFamilyOwner {
        kind_key: "mcp_tool",
        v2_owner: "tracedecay-tool-catalog",
    },
    SurfaceFamilyOwner {
        kind_key: "migrate_action",
        v2_owner: "tracedecay-application",
    },
    SurfaceFamilyOwner {
        kind_key: "provider_hook",
        v2_owner: "hooks",
    },
    SurfaceFamilyOwner {
        kind_key: "release_asset",
        v2_owner: "root",
    },
    SurfaceFamilyOwner {
        kind_key: "repair_action",
        v2_owner: "tracedecay-application",
    },
];

pub(super) const LIBRARY_MODULE_OWNERS: &[(&str, &str)] = &[
    ("accounting", "tracedecay-projectors"),
    ("agents", "root"),
    ("analytics_bridge", "tracedecay-projectors"),
    ("ast_grep_search", "tracedecay-query"),
    ("automation", "tracedecay-application"),
    ("bench", "root"),
    ("branch", "tracedecay-code-index"),
    ("branch_meta", "tracedecay-store"),
    ("client_identity", "tracedecay-domain"),
    ("cloud", "root"),
    ("compatibility_inventory", "root"),
    ("config", "tracedecay-application"),
    ("context", "tracedecay-query"),
    ("daemon", "root"),
    ("dashboard", "dashboard"),
    ("db", "tracedecay-store"),
    ("derive_table", "tracedecay-capture"),
    ("diagnose", "tracedecay-query"),
    ("diagnostics", "tracedecay-query"),
    ("display", "presentation"),
    ("doctor", "tracedecay-application"),
    ("errors", "tracedecay-domain"),
    ("external_tools", "root"),
    ("extraction", "tracedecay-capture"),
    ("extraction_worker", "tracedecay-capture"),
    ("git", "root"),
    ("global_db", "tracedecay-store"),
    ("graph", "tracedecay-code-index"),
    ("hooks", "hooks"),
    ("lifecycle_lease", "root"),
    ("mcp", "root"),
    ("memory", "tracedecay-projectors"),
    ("migrate", "root"),
    ("monitor", "tracedecay-application"),
    ("project_registry", "tracedecay-store"),
    ("redundancy", "tracedecay-query"),
    ("resolution", "tracedecay-code-index"),
    ("retention", "tracedecay-application"),
    ("runtime_identity", "tracedecay-domain"),
    ("runtime_telemetry", "tracedecay-projectors"),
    ("serde_util", "tracedecay-domain"),
    ("serve", "root"),
    ("sessions", "tracedecay-capture"),
    ("storage", "tracedecay-store"),
    ("sync", "tracedecay-capture"),
    ("text", "root"),
    ("timeutil", "tracedecay-domain"),
    ("tracedecay", "tracedecay-application"),
    ("types", "tracedecay-domain"),
    ("upgrade", "root"),
    ("user_config", "tracedecay-application"),
    ("worktree", "root"),
];

pub(super) const EXPECTED_SURFACE_FAMILY_CARDINALITIES: &[(&str, usize)] = &[
    ("cli_command", 128),
    ("cli_flag", 113),
    ("config", 48),
    ("dashboard_action", 36),
    ("dashboard_panel", 6),
    ("default", 48),
    ("doctor_action", 12),
    ("env", 37),
    ("http_action", 92),
    ("http_route", 81),
    ("installer_mutation", 62),
    ("library_module", 52),
    ("mcp_schema", 104),
    ("mcp_tool", 104),
    ("migrate_action", 9),
    ("provider_hook", 42),
    ("release_asset", 35),
    ("repair_action", 10),
];

pub(super) fn checked_v2_owner(kind: &str, name: &str) -> Result<&'static str, String> {
    if kind == "library_module" {
        return LIBRARY_MODULE_OWNERS
            .iter()
            .find_map(|(module, owner)| (*module == name).then_some(*owner))
            .ok_or_else(|| format!("unowned library module discovered: {name}"));
    }

    SURFACE_FAMILY_OWNERS
        .iter()
        .find_map(|descriptor| (descriptor.kind_key == kind).then_some(descriptor.v2_owner))
        .ok_or_else(|| format!("unowned surface family discovered: {kind}:{name}"))
}
