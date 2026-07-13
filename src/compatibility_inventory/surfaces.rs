//! Deterministic discovery of the shipped V1 product surface.
//!
//! Runtime registries are preferred. The few surfaces without a registry are
//! extracted from a fixed set of owner files embedded at compile time; this
//! module never walks the repository or parses planning documents.

use std::collections::{BTreeMap, BTreeSet};

use super::model::{
    CompatibilityEntryV1, EntityDispositionV1, InventoryGatesV1, InventoryOwnersV1,
    PlatformDispositionV1, RouteStatusV1,
};

mod descriptors;

use descriptors::{
    CARGO_SOURCE, CLI_AUTOMATION_SOURCE, CLI_SOURCE, CONFIG_SOURCE, DASHBOARD_ROUTES_SOURCE,
    DOCTOR_SOURCE, ENV_OWNER_SOURCES, EXPECTED_SURFACE_FAMILY_CARDINALITIES, LIB_SOURCE,
    RELEASE_BETA_SOURCE, RELEASE_SOURCE, REPAIR_SOURCE, USER_CONFIG_SOURCE, checked_v2_owner,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct SurfaceSpec {
    kind_key: &'static str,
    name: String,
    owner: &'static str,
    test: &'static str,
}

impl SurfaceSpec {
    fn new(
        kind_key: &'static str,
        name: impl Into<String>,
        owner: &'static str,
        test: &'static str,
    ) -> Self {
        Self {
            kind_key,
            name: name.into(),
            owner,
            test,
        }
    }

    fn key(&self) -> String {
        format!("{}:{}", self.kind_key, self.name)
    }
}

/// Discover every bounded V1 product surface and return canonical sorted rows.
pub fn discover_surfaces() -> Vec<CompatibilityEntryV1> {
    let mut specs = Vec::new();
    collect_library_modules(&mut specs);
    collect_cli(&mut specs);
    collect_mcp(&mut specs);
    collect_integrations(&mut specs);
    collect_dashboard(&mut specs);
    collect_config(&mut specs);
    collect_operational_actions(&mut specs);
    collect_release_assets(&mut specs);

    let mut canonical_keys = BTreeSet::new();
    assert!(
        specs.iter().all(|spec| canonical_keys.insert(spec.key())),
        "surface discovery produced duplicate canonical keys"
    );
    specs.sort_by_key(|spec| stable_id(spec.kind_key, &spec.name));
    let entries = specs.into_iter().map(entry_from_spec).collect::<Vec<_>>();
    validate_surface_family_cardinalities(&entries).unwrap_or_else(|error| panic!("{error}"));
    entries
}

fn validate_surface_family_cardinalities(entries: &[CompatibilityEntryV1]) -> Result<(), String> {
    let mut actual = BTreeMap::<&str, usize>::new();
    for entry in entries {
        *actual.entry(entry.kind.as_str()).or_default() += 1;
    }
    let expected = EXPECTED_SURFACE_FAMILY_CARDINALITIES
        .iter()
        .copied()
        .collect::<BTreeMap<_, _>>();
    if actual != expected {
        return Err(format!(
            "surface-family cardinality drift: expected {expected:?}, discovered {actual:?}"
        ));
    }
    Ok(())
}

fn entry_from_spec(spec: SurfaceSpec) -> CompatibilityEntryV1 {
    let key = spec.key();
    let v2_owner =
        checked_v2_owner(spec.kind_key, &spec.name).unwrap_or_else(|error| panic!("{error}"));
    let writes = matches!(
        spec.kind_key,
        "dashboard_action"
            | "http_action"
            | "installer_mutation"
            | "migrate_action"
            | "repair_action"
    );
    CompatibilityEntryV1 {
        stable_id: stable_id(spec.kind_key, &spec.name),
        kind: spec.kind_key.to_owned(),
        canonical_name: spec.name,
        source_refs: vec![spec.owner.to_owned()],
        platform: "all".to_owned(),
        route_status: RouteStatusV1::V1Only,
        entity_disposition: EntityDispositionV1::Retained,
        platform_disposition: Some(PlatformDispositionV1::Supported),
        owners: InventoryOwnersV1 {
            v1_owner: spec.owner.to_owned(),
            v2_owner: v2_owner.to_owned(),
        },
        readers: (!writes)
            .then(|| spec.owner.to_owned())
            .into_iter()
            .collect(),
        writers: writes.then(|| spec.owner.to_owned()).into_iter().collect(),
        tests: vec![spec.test.to_owned()],
        gates: InventoryGatesV1 {
            parity_gate: format!("PR3-PARITY:{key}"),
            cutover_gate: format!("PR37-CUTOVER:{key}"),
        },
        recovery: "retain the V1 implementation until parity is proven".to_owned(),
        delete_by_pr: "PR 37".to_owned(),
    }
}

fn stable_id(kind: &str, name: &str) -> String {
    let mut slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug = slug.trim_matches('-').to_owned();
    slug.truncate(110_usize.saturating_sub(kind.len()));
    format!("{kind}:{slug}:{:016x}", fnv64(name.as_bytes()))
}

fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn collect_library_modules(rows: &mut Vec<SurfaceSpec>) {
    for line in LIB_SOURCE.lines().map(str::trim) {
        let Some(name) = line
            .strip_prefix("pub mod ")
            .and_then(|rest| rest.strip_suffix(';'))
        else {
            continue;
        };
        rows.push(SurfaceSpec::new(
            "library_module",
            name,
            "src/lib.rs",
            "cargo test --lib",
        ));
    }
}

fn collect_cli(rows: &mut Vec<SurfaceSpec>) {
    let mut seen_flags = BTreeSet::new();
    for (owner, source) in [
        ("src/cli.rs", CLI_SOURCE),
        ("src/cli/automation.rs", CLI_AUTOMATION_SOURCE),
    ] {
        for (enum_name, variant) in subcommand_variants(source) {
            let command = format!("{}.{}", kebab_case(&enum_name), kebab_case(&variant));
            rows.push(SurfaceSpec::new(
                "cli_command",
                command.clone(),
                owner,
                "src/cli/parse_tests.rs::visible_subcommands_accept_clap_help",
            ));
            if variant.starts_with("Hook") {
                rows.push(SurfaceSpec::new(
                    "provider_hook",
                    kebab_case(variant.trim_start_matches("Hook")),
                    owner,
                    "tests/hooks_lsp_suite/hooks_test.rs",
                ));
            }
        }

        for flag in clap_flags(source) {
            if !seen_flags.insert(flag.clone()) {
                continue;
            }
            rows.push(SurfaceSpec::new(
                "cli_flag",
                flag,
                owner,
                "src/cli/parse_tests.rs::visible_subcommands_accept_clap_help",
            ));
        }
    }
}

fn collect_mcp(rows: &mut Vec<SurfaceSpec>) {
    for definition in crate::mcp::tools::get_tool_definitions() {
        let name = definition.name;
        rows.push(SurfaceSpec::new(
            "mcp_tool",
            name.clone(),
            "src/mcp/tools/definitions.rs",
            "src/mcp/tools/handlers/mod.rs::tool_definitions_and_dispatch_handlers_stay_in_lockstep",
        ));
        rows.push(SurfaceSpec::new(
            "mcp_schema",
            format!("{name}.input"),
            "src/mcp/tools/definitions.rs",
            "src/mcp/tools/handlers/mod.rs::test_tool_definitions_have_schemas",
        ));
    }
}

fn collect_integrations(rows: &mut Vec<SurfaceSpec>) {
    for integration in crate::agents::all_integrations() {
        let id = integration.id();
        rows.push(SurfaceSpec::new(
            "provider_hook",
            format!("{id}.integration"),
            "src/agents/mod.rs::all_integrations",
            "tests/agent_suite/agent_test.rs",
        ));
        for operation in ["install", "reinstall", "uninstall", "update-plugin"] {
            rows.push(SurfaceSpec::new(
                "installer_mutation",
                format!("{id}.{operation}"),
                "src/agents/mod.rs::AgentIntegration",
                "tests/agent_suite/agent_test.rs",
            ));
        }
    }
}

fn collect_dashboard(rows: &mut Vec<SurfaceSpec>) {
    for plugin in crate::dashboard::assets::DASHBOARD_PLUGINS {
        rows.push(SurfaceSpec::new(
            "dashboard_panel",
            plugin.name,
            "src/dashboard/assets.rs::DASHBOARD_PLUGINS",
            "tests/dashboard_api_test",
        ));
    }

    for route in route_calls(DASHBOARD_ROUTES_SOURCE) {
        rows.push(SurfaceSpec::new(
            "http_route",
            route.path.clone(),
            "src/dashboard/mod.rs::router/project_api_router",
            "tests/dashboard_api_test",
        ));
        for method in route.methods {
            let action = format!("{method} {}", route.path);
            rows.push(SurfaceSpec::new(
                "http_action",
                action.clone(),
                "src/dashboard/mod.rs::router/project_api_router",
                "tests/dashboard_api_test",
            ));
            if method != "GET" {
                rows.push(SurfaceSpec::new(
                    "dashboard_action",
                    action,
                    "src/dashboard/mod.rs::project_api_router",
                    "tests/dashboard_api_test",
                ));
            }
        }
    }
}

fn collect_config(rows: &mut Vec<SurfaceSpec>) {
    for (owner, source, structs) in [
        (
            "src/config.rs",
            CONFIG_SOURCE,
            &["TraceDecayConfig", "SyncConfig", "TelemetryConfig"][..],
        ),
        (
            "src/user_config.rs",
            USER_CONFIG_SOURCE,
            &["UserConfig"][..],
        ),
    ] {
        for struct_name in structs {
            for field in public_struct_fields(source, struct_name) {
                let canonical_name = format!("{}.{}", kebab_case(struct_name), kebab_case(&field));
                rows.push(SurfaceSpec::new(
                    "config",
                    canonical_name.clone(),
                    owner,
                    "tests/core_cli_suite/config_test.rs",
                ));
                rows.push(SurfaceSpec::new(
                    "default",
                    canonical_name,
                    owner,
                    "tests/core_cli_suite/config_test.rs",
                ));
            }
        }
    }

    let mut env_names = BTreeSet::new();
    for (owner, source) in ENV_OWNER_SOURCES {
        for name in environment_names(source) {
            if env_names.insert(name.clone()) {
                rows.push(SurfaceSpec::new(
                    "env",
                    name,
                    owner,
                    "src/dashboard/settings_api.rs::environment_payload",
                ));
            }
        }
    }
}

fn collect_operational_actions(rows: &mut Vec<SurfaceSpec>) {
    rows.push(SurfaceSpec::new(
        "doctor_action",
        "doctor",
        "src/doctor.rs::run_doctor",
        "src/doctor/tests.rs",
    ));
    for check in function_names_with_prefix(DOCTOR_SOURCE, "check_") {
        rows.push(SurfaceSpec::new(
            "doctor_action",
            check,
            "src/doctor.rs::run_doctor",
            "src/doctor/tests.rs",
        ));
    }
    for repair in function_names_with_prefix(REPAIR_SOURCE, "run_post_update_")
        .into_iter()
        .chain(function_names_with_prefix(REPAIR_SOURCE, "quarantine_"))
        .chain(function_names_with_prefix(REPAIR_SOURCE, "gc_"))
        .chain(function_names_with_prefix(REPAIR_SOURCE, "retire_"))
    {
        rows.push(SurfaceSpec::new(
            "repair_action",
            repair,
            "src/doctor/heal.rs",
            "src/doctor/heal.rs::tests",
        ));
    }
    for (enum_name, variant) in subcommand_variants(CLI_SOURCE) {
        if enum_name == "MigrateAction" {
            rows.push(SurfaceSpec::new(
                "migrate_action",
                kebab_case(&variant),
                "src/cli.rs::MigrateAction",
                "src/cli/parse_tests.rs::migrate_commands_parse_manifest_scaffolding_flags",
            ));
        }
    }
    for action in ["daemon-service.install", "daemon-service.uninstall"] {
        rows.push(SurfaceSpec::new(
            "installer_mutation",
            action,
            "src/daemon/service.rs",
            "src/cli/parse_tests.rs::daemon_install_service_command_parses_socket_and_no_start",
        ));
    }
}

fn collect_release_assets(rows: &mut Vec<SurfaceSpec>) {
    for package_path in cargo_package_assets(CARGO_SOURCE) {
        rows.push(SurfaceSpec::new(
            "release_asset",
            format!("crate:{package_path}"),
            "Cargo.toml::package.include",
            "cargo package --list",
        ));
    }
    for (channel, source) in [("stable", RELEASE_SOURCE), ("beta", RELEASE_BETA_SOURCE)] {
        for (platform, archive) in release_matrix_assets(source) {
            rows.push(SurfaceSpec::new(
                "release_asset",
                format!("{channel}:{platform}.{archive}"),
                if channel == "stable" {
                    ".github/workflows/release.yml"
                } else {
                    ".github/workflows/release-beta.yml"
                },
                "tests/release_workflow_contract_test.sh",
            ));
        }
    }
    for asset in [
        "crates.io",
        "homebrew-bottle",
        "scoop-manifest",
        "winget-manifest",
    ] {
        rows.push(SurfaceSpec::new(
            "release_asset",
            asset,
            ".github/workflows/release.yml",
            "tests/release_workflow_contract_test.sh",
        ));
    }
}

fn subcommand_variants(source: &str) -> Vec<(String, String)> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].contains("derive(Subcommand") {
            index += 1;
            continue;
        }
        index += 1;
        while index < lines.len() && !lines[index].trim_start().starts_with("pub enum ") {
            index += 1;
        }
        let Some(enum_line) = lines.get(index) else {
            break;
        };
        let Some(enum_name) = enum_line
            .trim()
            .strip_prefix("pub enum ")
            .and_then(|rest| rest.split_whitespace().next())
        else {
            index += 1;
            continue;
        };
        let enum_name = enum_name.trim_end_matches('{').to_owned();
        let mut depth = brace_delta(enum_line);
        index += 1;
        while index < lines.len() && depth > 0 {
            let line = lines[index].trim();
            if depth == 1 && !line.is_empty() && !line.starts_with('#') && !line.starts_with("//") {
                let candidate = line
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .next()
                    .unwrap_or_default();
                if candidate
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_uppercase())
                {
                    rows.push((enum_name.clone(), candidate.to_owned()));
                }
            }
            depth += brace_delta(lines[index]);
            index += 1;
        }
    }
    rows
}

fn clap_flags(source: &str) -> BTreeSet<String> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut flags = BTreeSet::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if !line.starts_with("#[arg(") {
            index += 1;
            continue;
        }
        let mut attribute = line.to_owned();
        while !attribute.contains(")]") && index + 1 < lines.len() {
            index += 1;
            attribute.push_str(lines[index].trim());
        }
        index += 1;
        while index < lines.len() && lines[index].trim().starts_with('#') {
            index += 1;
        }
        let field = lines
            .get(index)
            .map(|line| line.trim())
            .and_then(|line| line.strip_prefix("pub ").or(Some(line)))
            .and_then(|line| line.split(':').next())
            .map(str::trim)
            .filter(|field| {
                field
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            });
        let Some(field) = field else {
            continue;
        };
        if attribute_word(&attribute, "long") {
            let long = assigned_quoted(&attribute, "long").unwrap_or_else(|| kebab_case(field));
            flags.insert(format!("--{long}"));
        }
        if attribute_word(&attribute, "short") {
            let short = assigned_char(&attribute, "short")
                .or_else(|| field.chars().next())
                .unwrap_or_default();
            flags.insert(format!("-{short}"));
        }
    }
    flags
}

fn public_struct_fields(source: &str, struct_name: &str) -> Vec<String> {
    let Some(start) = source.find(&format!("pub struct {struct_name}")) else {
        return Vec::new();
    };
    let Some(open) = source[start..].find('{').map(|offset| start + offset) else {
        return Vec::new();
    };
    let Some(body) = balanced_fragment(source, open, '{', '}') else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("pub ")
                .and_then(|line| line.split(':').next())
                .map(str::trim)
                .filter(|field| !field.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

fn function_names_with_prefix(source: &str, prefix: &str) -> Vec<String> {
    let mut names = source
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let declaration = [
                "fn ",
                "async fn ",
                "pub fn ",
                "pub async fn ",
                "pub(crate) fn ",
                "pub(crate) async fn ",
            ]
            .into_iter()
            .find_map(|prefix| line.strip_prefix(prefix))?;
            let name = declaration.split('(').next()?.trim();
            name.starts_with(prefix).then(|| name.to_owned())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn environment_names(source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let lines = source.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.contains("_ENV") && trimmed.contains("const ") {
            let declaration = lines[index..lines.len().min(index + 4)].join(" ");
            if let Some(value) = first_quoted(&declaration) {
                if looks_like_env_name(&value) {
                    names.insert(value);
                }
            }
        }
        for needle in ["env::var(\"", "env::var_os(\"", "option_env!(\""] {
            let mut rest = trimmed;
            while let Some(offset) = rest.find(needle) {
                rest = &rest[offset + needle.len()..];
                let Some(end) = rest.find('"') else {
                    break;
                };
                let value = &rest[..end];
                if looks_like_env_name(value) {
                    names.insert(value.to_owned());
                }
                rest = &rest[end + 1..];
            }
        }
        for needle in ["brand_env(\"", "env_bool(\"", "env_parse(\""] {
            let mut rest = trimmed;
            while let Some(offset) = rest.find(needle) {
                rest = &rest[offset + needle.len()..];
                let Some(end) = rest.find('"') else {
                    break;
                };
                let suffix = &rest[..end];
                if looks_like_env_name(suffix) {
                    names.insert(format!("TRACEDECAY_{suffix}"));
                }
                rest = &rest[end + 1..];
            }
        }
    }
    names
}

fn looks_like_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

#[derive(Debug, Eq, PartialEq)]
struct RouteCall {
    path: String,
    methods: BTreeSet<&'static str>,
}

fn route_calls(source: &str) -> Vec<RouteCall> {
    let mut routes = std::collections::BTreeMap::<String, BTreeSet<&'static str>>::new();
    let mut rest = source;
    while let Some(offset) = rest.find(".route(") {
        rest = &rest[offset + ".route".len()..];
        let Some(fragment) = balanced_fragment(rest, 0, '(', ')') else {
            break;
        };
        if let Some(path) = first_quoted(fragment) {
            let mut methods = BTreeSet::new();
            for (needle, method) in [
                ("any(", "ANY"),
                ("delete(", "DELETE"),
                ("get(", "GET"),
                ("patch(", "PATCH"),
                ("post(", "POST"),
                ("put(", "PUT"),
            ] {
                if fragment.contains(needle) {
                    methods.insert(method);
                }
            }
            routes.entry(path).or_default().append(&mut methods);
        }
        rest = &rest[fragment.len()..];
    }
    routes
        .into_iter()
        .map(|(path, methods)| RouteCall { path, methods })
        .collect()
}

fn cargo_package_assets(source: &str) -> Vec<String> {
    let Some(start) = source.find("include = [") else {
        return Vec::new();
    };
    let Some(end) = source[start..].find("]\n").map(|offset| start + offset) else {
        return Vec::new();
    };
    let mut assets = source[start..end]
        .lines()
        .filter_map(first_quoted)
        .collect::<Vec<_>>();
    assets.sort();
    assets.dedup();
    assets
}

fn release_matrix_assets(source: &str) -> Vec<(String, String)> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut assets = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(name) = line.trim().strip_prefix("- name: ") else {
            continue;
        };
        let window = &lines[index..lines.len().min(index + 7)];
        if !window
            .iter()
            .any(|line| line.trim().starts_with("target: "))
        {
            continue;
        }
        let Some(archive) = window
            .iter()
            .find_map(|line| line.trim().strip_prefix("archive: "))
        else {
            continue;
        };
        assets.push((
            name.trim_matches('"').to_owned(),
            archive.trim_matches('"').to_owned(),
        ));
    }
    assets.sort();
    assets.dedup();
    assets
}

fn balanced_fragment(source: &str, open: usize, opening: char, closing: char) -> Option<&str> {
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in source[open..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character == '"' {
            in_string = true;
        } else if character == opening {
            depth += 1;
        } else if character == closing {
            depth -= 1;
            if depth == 0 {
                return Some(&source[open..=open + offset]);
            }
        }
    }
    None
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0, |depth, character| match character {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

fn first_quoted(source: &str) -> Option<String> {
    let start = source.find('"')? + 1;
    let end = source[start..].find('"')? + start;
    Some(source[start..end].to_owned())
}

fn assigned_quoted(attribute: &str, key: &str) -> Option<String> {
    let rest = attribute.split_once(key)?.1.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    first_quoted(rest)
}

fn assigned_char(attribute: &str, key: &str) -> Option<char> {
    let rest = attribute.split_once(key)?.1.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start().strip_prefix('\'')?;
    rest.chars().next()
}

fn attribute_word(attribute: &str, word: &str) -> bool {
    attribute
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|part| part == word)
}

fn kebab_case(value: &str) -> String {
    let mut output = String::new();
    let mut previous_lowercase = false;
    for character in value.chars() {
        if character == '_' {
            output.push('-');
            previous_lowercase = false;
        } else if character.is_ascii_uppercase() {
            if previous_lowercase {
                output.push('-');
            }
            output.push(character.to_ascii_lowercase());
            previous_lowercase = false;
        } else {
            output.push(character);
            previous_lowercase = character.is_ascii_lowercase() || character.is_ascii_digit();
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_stably_sorted() {
        let first = discover_surfaces();
        let second = discover_surfaces();
        let first_ids = first
            .iter()
            .map(|entry| entry.stable_id.as_str())
            .collect::<Vec<_>>();
        let second_ids = second
            .iter()
            .map(|entry| entry.stable_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(first_ids, second_ids);
        assert!(first_ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn discovery_has_no_duplicate_ids() {
        let rows = discover_surfaces();
        let ids = rows
            .iter()
            .map(|entry| entry.stable_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), rows.len());
    }

    #[test]
    fn bounded_collectors_cover_each_surface_family() {
        let rows = discover_surfaces();
        for kind in [
            "library_module",
            "cli_command",
            "cli_flag",
            "mcp_tool",
            "mcp_schema",
            "http_route",
            "http_action",
            "dashboard_panel",
            "dashboard_action",
            "provider_hook",
            "config",
            "env",
            "default",
            "installer_mutation",
            "doctor_action",
            "repair_action",
            "migrate_action",
            "release_asset",
        ] {
            assert!(
                rows.iter().any(|entry| entry.kind == kind),
                "missing {kind}"
            );
        }
    }

    #[test]
    fn checked_owners_follow_surface_and_module_boundaries() {
        assert_eq!(
            checked_v2_owner("cli_command", "command.run").unwrap(),
            "tracedecay-tool-catalog"
        );
        assert_eq!(
            checked_v2_owner("mcp_tool", "context").unwrap(),
            "tracedecay-tool-catalog"
        );
        assert_eq!(
            checked_v2_owner("release_asset", "crates.io").unwrap(),
            "root"
        );
        assert_eq!(
            checked_v2_owner("library_module", "sessions").unwrap(),
            "tracedecay-capture"
        );
        assert_eq!(
            checked_v2_owner("library_module", "storage").unwrap(),
            "tracedecay-store"
        );
    }

    #[test]
    fn unknown_surface_and_module_owners_fail_closed() {
        assert!(checked_v2_owner("unknown", "surface").is_err());
        assert!(checked_v2_owner("library_module", "new_unowned_module").is_err());
    }

    #[test]
    fn partial_surface_discovery_fails_closed() {
        let mut rows = discover_surfaces();
        rows.pop();
        assert!(validate_surface_family_cardinalities(&rows).is_err());

        let mut rows = discover_surfaces();
        rows[0].kind = "unknown_surface_family".to_owned();
        assert!(validate_surface_family_cardinalities(&rows).is_err());
    }
}
