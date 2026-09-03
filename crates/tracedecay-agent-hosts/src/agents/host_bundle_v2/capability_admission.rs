//! Host-component admission gates derived from canonical host capabilities.

use super::{
    HostBundleComponentV1, HostBundleError, HostCapabilityStateV1, HostCapabilityV1, HostKindV1,
    stock_host_native_fixture_evidence,
};
use tracedecay_host_integration::stock_host_capabilities;

/// Refuse silent emulation of unsupported/degraded host capabilities.
pub fn require_capability(
    host: HostKindV1,
    capability: HostCapabilityV1,
) -> Result<(), HostBundleError> {
    let record = stock_host_capabilities(host)
        .into_iter()
        .find(|record| record.capability == capability)
        .ok_or(HostBundleError::UnsupportedCapability)?;
    if record.state == HostCapabilityStateV1::Supported {
        Ok(())
    } else {
        Err(HostBundleError::UnsupportedCapability)
    }
}

/// Refuse a component whose host capabilities or binding fixture evidence do
/// not justify the native surfaces that component would install.
pub fn require_component_capabilities(
    host: HostKindV1,
    component: HostBundleComponentV1,
) -> Result<(), HostBundleError> {
    use HostBundleComponentV1::{Agent, ContextMcp, Core, OperatorMcp};
    use HostCapabilityV1::{Cli, Hooks, Lsp, Mcp, NativeDiagnostics};

    let required: &[HostCapabilityV1] = match (host, component) {
        (HostKindV1::ClaudeCode, Core) => &[Lsp, Hooks],
        (HostKindV1::CursorDesktop | HostKindV1::Codex, Core) => &[Hooks],
        (
            HostKindV1::Hermes | HostKindV1::Kiro | HostKindV1::KimiCode | HostKindV1::OpenCode,
            Core,
        ) => &[Hooks, Mcp],
        (HostKindV1::CursorCloud | HostKindV1::ClineFamily, Core) => &[Mcp],
        (HostKindV1::Cline | HostKindV1::RooCode | HostKindV1::Kilo, Core) => &[Hooks, Mcp],
        // Gemini's extension is its MCP registration and nothing else. A Core
        // component would install the hook surface, which this host neither
        // reports nor drives, so requiring `Hooks` refuses it against the same
        // capability matrix every other host is judged by.
        (HostKindV1::Gemini, Core) => &[Hooks, Mcp],
        // Copilot's adopted lifecycle is its MCP registration and nothing else.
        // A Core component would install the hook surface, which this host
        // neither reports nor drives, so requiring `Hooks` refuses it against
        // the same capability matrix every other host is judged by instead of
        // through a missing match arm that would silently fall to `&[Mcp]`.
        (HostKindV1::Copilot, Core) => &[Hooks, Mcp],
        (_, ContextMcp | OperatorMcp) => &[Mcp],
        (HostKindV1::CursorDesktop, Agent) => &[NativeDiagnostics],
        (HostKindV1::OpenCode, Agent) => &[Cli],
        (_, Agent) => return Err(HostBundleError::UnsupportedCapability),
    };
    for capability in required {
        require_capability(host, *capability)?;
    }
    if required
        .iter()
        .any(|capability| matches!(capability, Lsp | NativeDiagnostics | Hooks))
        && stock_host_native_fixture_evidence(host).is_none()
    {
        return Err(HostBundleError::UnsupportedCapability);
    }

    Ok(())
}
