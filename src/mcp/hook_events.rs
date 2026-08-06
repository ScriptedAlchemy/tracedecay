//! Normalizes daemon hook notifications into typed convergence hints.
//!
//! This module owns wire-level hook semantics. The retained scheduler owns all
//! code convergence; hook handling only admits bounded hints and wakes it.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::tools::handlers::hook_runtime::CursorQueuedEventV1;

/// Shared with hook emitters so the receiver accepts the same agent keys.
pub(crate) use crate::daemon::HookAgent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookEventKind {
    FileEdit,
    Shell,
    WorkspaceOpen,
    SessionStart,
    IncrementalSync,
    TerminalReceipt,
    TurnCompleted,
    TurnIngested,
}

impl HookEventKind {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "afterFileEdit" | "postToolUseEdit" => Some(Self::FileEdit),
            "afterShellExecution" | "postToolUseShell" => Some(Self::Shell),
            "workspaceOpen" => Some(Self::WorkspaceOpen),
            "sessionStart" => Some(Self::SessionStart),
            "postToolUse" => Some(Self::IncrementalSync),
            "terminalReceipt" => Some(Self::TerminalReceipt),
            "turnCompleted" => Some(Self::TurnCompleted),
            "turnIngested" => Some(Self::TurnIngested),
            _ => None,
        }
    }

    pub(crate) fn as_key(self) -> &'static str {
        match self {
            Self::FileEdit => "file_edit",
            Self::Shell => "shell",
            Self::WorkspaceOpen => "workspace_open",
            Self::SessionStart => "session_start",
            Self::IncrementalSync => "incremental_sync",
            Self::TerminalReceipt => "terminal_receipt",
            Self::TurnCompleted => "turn_completed",
            Self::TurnIngested => "turn_ingested",
        }
    }
}

pub(crate) struct HookEvent {
    pub(crate) agent: HookAgent,
    pub(crate) kind: HookEventKind,
    pub(crate) rel_paths: Vec<String>,
    pub(crate) had_command: bool,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) route: Option<crate::daemon::HookRouteMetadata>,
    pub(crate) receipt: Option<crate::daemon::HookTerminalReceipt>,
}

impl HookEvent {
    pub(crate) fn admission_source(&self) -> String {
        let mut identity = Vec::new();
        if let Some(route) = self.route.as_ref() {
            if let Some(session_id) = route
                .session_id
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                push_admission_identity_part(&mut identity, "session", session_id.as_bytes());
            } else if let Some(thread_id) =
                route.thread_id.as_deref().filter(|value| !value.is_empty())
            {
                push_admission_identity_part(&mut identity, "thread", thread_id.as_bytes());
            } else if let Some(worktree) = route.worktree.as_deref() {
                push_admission_identity_part(
                    &mut identity,
                    "worktree",
                    worktree.as_os_str().as_encoded_bytes(),
                );
            } else if let Some(cwd) = route.cwd.as_deref() {
                push_admission_identity_part(
                    &mut identity,
                    "route_cwd",
                    cwd.as_os_str().as_encoded_bytes(),
                );
            }
        }
        if identity.is_empty()
            && let Some(cwd) = self.cwd.as_deref()
        {
            push_admission_identity_part(&mut identity, "cwd", cwd.as_os_str().as_encoded_bytes());
        }
        if identity.is_empty()
            && let Some(receipt) = self.receipt.as_ref()
        {
            if let Some(turn_id) = receipt.turn_id.as_deref().filter(|value| !value.is_empty()) {
                push_admission_identity_part(&mut identity, "turn", turn_id.as_bytes());
            } else if let Some(tool_call_id) = receipt
                .tool_call_id
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                push_admission_identity_part(&mut identity, "tool_call", tool_call_id.as_bytes());
            } else if let Some(watermark) = receipt
                .transcript_watermark
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                push_admission_identity_part(&mut identity, "watermark", watermark.as_bytes());
            }
        }
        if identity.is_empty() {
            push_admission_identity_part(&mut identity, "event", self.kind.as_key().as_bytes());
            for path in &self.rel_paths {
                push_admission_identity_part(&mut identity, "path", path.as_bytes());
            }
        }
        format!(
            "{}:{}",
            self.agent.as_wire(),
            crate::context::read_cache::digest_bytes(&identity)
        )
    }
}

fn push_admission_identity_part(buffer: &mut Vec<u8>, label: &str, value: &[u8]) {
    buffer.extend_from_slice(label.len().to_string().as_bytes());
    buffer.push(b':');
    buffer.extend_from_slice(label.as_bytes());
    buffer.extend_from_slice(value.len().to_string().as_bytes());
    buffer.push(b':');
    buffer.extend_from_slice(value);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookEventPlan {
    SyncFiles(Vec<String>),
    DebouncedIncrementalSync(HookAgent),
    RecordTerminalReceipt {
        route: Option<crate::daemon::HookRouteMetadata>,
        receipt: crate::daemon::HookTerminalReceipt,
    },
    MarkTurnIngested {
        route: Option<crate::daemon::HookRouteMetadata>,
        transcript_watermark: String,
    },
    /// A privacy-filtered Cursor event accepted by the daemon. Its effects run
    /// only from durable admission replay, never on the host-hook deadline.
    CursorEvent(CursorQueuedEventV1),
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DurableHookEventPlan {
    SyncFiles {
        rel_paths: Vec<String>,
    },
    DebouncedIncrementalSync {
        agent: String,
    },
    RecordTerminalReceipt {
        route: Option<crate::daemon::HookRouteMetadata>,
        receipt: crate::daemon::HookTerminalReceipt,
    },
    MarkTurnIngested {
        route: Option<crate::daemon::HookRouteMetadata>,
        transcript_watermark: String,
    },
    CursorEvent {
        event: CursorQueuedEventV1,
    },
    Noop,
}

/// Durable spool envelope version. Cursor-native events extend the plan
/// inventory, so new records use V2 while V1 records remain replayable.
const DURABLE_HOOK_EVENT_ENVELOPE_VERSION: u16 = 2;
const LEGACY_DURABLE_HOOK_EVENT_ENVELOPE_VERSION: u16 = 1;

/// Lookup identifiers needed outside receipt-state equality (session and
/// watermark) stay bounded. Session ids are run through
/// [`crate::privacy::protect_sensitive_structural_id`] so credential-shaped
/// values become stable digests while public ids remain byte-for-byte.
/// Equality-only thread/tool/turn identifiers are hashed before persistence.
const DURABLE_MAX_IDENTIFIER_BYTES: usize = 256;
const DURABLE_MAX_STATUS_BYTES: usize = 64;
const DURABLE_MAX_PATH_BYTES: usize = 1024;
const DURABLE_MAX_REL_PATH_BYTES: usize = 512;
const DURABLE_MAX_REL_PATHS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableHookEventEnvelope {
    version: u16,
    plan: DurableHookEventPlan,
}

#[derive(Deserialize)]
struct DurableHookEventEnvelopeHeader {
    version: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DurableHookEventDecodeError {
    Malformed,
    UnsupportedVersion,
}

fn durable_bound_optional_str(value: Option<&str>, max_bytes: usize) -> Result<Option<String>, ()> {
    match value {
        None | Some("") => Ok(None),
        Some(value)
            if value.len() > max_bytes
                || value.as_bytes().contains(&0)
                || value.chars().any(char::is_control) =>
        {
            Err(())
        }
        Some(value) => Ok(Some(value.to_string())),
    }
}

fn durable_bound_required_str(value: &str, max_bytes: usize) -> Result<String, ()> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.as_bytes().contains(&0)
        || value.chars().any(char::is_control)
    {
        Err(())
    } else {
        Ok(value.to_string())
    }
}

fn protect_optional_hook_structural_id(value: Option<&str>) -> Result<Option<String>, ()> {
    crate::privacy::protect_optional_sensitive_structural_id(value).map_err(|_| ())
}

fn protect_hook_route_structural_ids(
    route: &mut crate::daemon::HookRouteMetadata,
) -> Result<(), ()> {
    route.session_id = protect_optional_hook_structural_id(route.session_id.as_deref())?;
    route.thread_id = protect_optional_hook_structural_id(route.thread_id.as_deref())?;
    Ok(())
}

fn protect_hook_receipt_structural_ids(
    receipt: &mut crate::daemon::HookTerminalReceipt,
) -> Result<(), ()> {
    receipt.tool_call_id = protect_optional_hook_structural_id(receipt.tool_call_id.as_deref())?;
    receipt.turn_id = protect_optional_hook_structural_id(receipt.turn_id.as_deref())?;
    receipt.transcript_watermark =
        protect_optional_hook_structural_id(receipt.transcript_watermark.as_deref())?;
    Ok(())
}

fn sanitize_durable_status(value: Option<&str>) -> Result<Option<String>, ()> {
    let Some(value) = durable_bound_optional_str(value, DURABLE_MAX_STATUS_BYTES)? else {
        return Ok(None);
    };
    let normalized = value.to_ascii_lowercase();
    Ok(Some(match normalized.as_str() {
        "success" | "succeeded" | "completed" | "failed" | "error" | "cancelled" | "canceled"
        | "timeout" | "timed_out" | "skipped" => normalized,
        _ => "unknown".to_string(),
    }))
}

/// Persist only route fields required for receipt lookup/replay. Absolute
/// `cwd`/`worktree` paths and branch labels are not effects for receipt plans,
/// so they are dropped. Structural ids use the same deterministic protection
/// as transcript/LCM storage so receipt, analytics, span, and reopen joins
/// preserve one identity.
fn sanitize_durable_route(
    route: Option<&crate::daemon::HookRouteMetadata>,
) -> Result<Option<crate::daemon::HookRouteMetadata>, ()> {
    let Some(route) = route else {
        return Ok(None);
    };
    let mut sanitized = crate::daemon::HookRouteMetadata {
        session_id: durable_bound_optional_str(
            route.session_id.as_deref(),
            DURABLE_MAX_IDENTIFIER_BYTES,
        )?,
        thread_id: durable_bound_optional_str(
            route.thread_id.as_deref(),
            DURABLE_MAX_IDENTIFIER_BYTES,
        )?,
        cwd: None,
        worktree: None,
        branch: None,
    };
    protect_hook_route_structural_ids(&mut sanitized)?;
    Ok(Some(sanitized))
}

fn sanitize_durable_receipt(
    receipt: &crate::daemon::HookTerminalReceipt,
) -> Result<crate::daemon::HookTerminalReceipt, ()> {
    let mut sanitized = crate::daemon::HookTerminalReceipt {
        tool_call_id: durable_bound_optional_str(
            receipt.tool_call_id.as_deref(),
            DURABLE_MAX_IDENTIFIER_BYTES,
        )?,
        turn_id: durable_bound_optional_str(
            receipt.turn_id.as_deref(),
            DURABLE_MAX_IDENTIFIER_BYTES,
        )?,
        status: sanitize_durable_status(receipt.status.as_deref())?,
        duration_ms: receipt.duration_ms,
        transcript_watermark: durable_bound_optional_str(
            receipt.transcript_watermark.as_deref(),
            DURABLE_MAX_IDENTIFIER_BYTES,
        )?,
    };
    protect_hook_receipt_structural_ids(&mut sanitized)?;
    Ok(sanitized)
}

fn sanitize_durable_rel_paths(rel_paths: &[String]) -> Result<Vec<String>, ()> {
    let sanitized = safe_hook_rel_paths(rel_paths);
    if sanitized.len() != rel_paths.len() || sanitized.len() > DURABLE_MAX_REL_PATHS {
        return Err(());
    }
    if sanitized
        .iter()
        .any(|path| path.len() > DURABLE_MAX_REL_PATH_BYTES)
    {
        return Err(());
    }
    Ok(sanitized)
}

pub(crate) fn authorize_observation_worktree_root(
    candidate: &Path,
    active_project_root: &Path,
) -> Option<PathBuf> {
    if !candidate.is_absolute() || !active_project_root.is_absolute() {
        return None;
    }
    let candidate = candidate.canonicalize().ok()?;
    let active_project_root = active_project_root.canonicalize().ok()?;
    let candidate_worktree = crate::worktree::git_worktree_root(&candidate)?;
    let candidate_worktree = candidate_worktree.canonicalize().ok()?;
    if candidate_worktree != candidate {
        return None;
    }
    let candidate_common = crate::worktree::git_common_dir(&candidate_worktree)?
        .canonicalize()
        .ok()?;
    let active_common = crate::worktree::git_common_dir(&active_project_root)?
        .canonicalize()
        .ok()?;
    (candidate_common == active_common).then_some(candidate_worktree)
}

fn durable_plan_from_runtime(plan: &HookEventPlan) -> Result<DurableHookEventPlan, ()> {
    Ok(match plan {
        HookEventPlan::SyncFiles(rel_paths) => DurableHookEventPlan::SyncFiles {
            rel_paths: sanitize_durable_rel_paths(rel_paths)?,
        },
        HookEventPlan::DebouncedIncrementalSync(agent) => {
            DurableHookEventPlan::DebouncedIncrementalSync {
                agent: agent.as_wire().to_string(),
            }
        }
        HookEventPlan::RecordTerminalReceipt { route, receipt } => {
            DurableHookEventPlan::RecordTerminalReceipt {
                route: sanitize_durable_route(route.as_ref())?,
                receipt: sanitize_durable_receipt(receipt)?,
            }
        }
        HookEventPlan::MarkTurnIngested {
            route,
            transcript_watermark,
        } => DurableHookEventPlan::MarkTurnIngested {
            route: sanitize_durable_route(route.as_ref())?,
            transcript_watermark: protect_optional_hook_structural_id(Some(
                &durable_bound_required_str(transcript_watermark, DURABLE_MAX_IDENTIFIER_BYTES)?,
            ))?
            .ok_or(())?,
        },
        HookEventPlan::CursorEvent(event) => DurableHookEventPlan::CursorEvent {
            event: {
                if !crate::mcp::tools::handlers::hook_runtime::cursor_event::validate_queued_cursor_event(event) {
                    return Err(());
                }
                event.clone()
            },
        },
        HookEventPlan::Noop => DurableHookEventPlan::Noop,
    })
}

fn runtime_plan_from_durable(
    durable: DurableHookEventPlan,
) -> Result<HookEventPlan, DurableHookEventDecodeError> {
    match durable {
        DurableHookEventPlan::SyncFiles { rel_paths } => Ok(HookEventPlan::SyncFiles(
            sanitize_durable_rel_paths(&rel_paths)
                .map_err(|()| DurableHookEventDecodeError::Malformed)?,
        )),
        DurableHookEventPlan::DebouncedIncrementalSync { agent } => {
            Ok(HookEventPlan::DebouncedIncrementalSync(
                HookAgent::from_wire(&agent).ok_or(DurableHookEventDecodeError::Malformed)?,
            ))
        }
        DurableHookEventPlan::RecordTerminalReceipt { route, receipt } => {
            Ok(HookEventPlan::RecordTerminalReceipt {
                route: sanitize_durable_route(route.as_ref())
                    .map_err(|()| DurableHookEventDecodeError::Malformed)?,
                receipt: sanitize_durable_receipt(&receipt)
                    .map_err(|()| DurableHookEventDecodeError::Malformed)?,
            })
        }
        DurableHookEventPlan::MarkTurnIngested {
            route,
            transcript_watermark,
        } => Ok(HookEventPlan::MarkTurnIngested {
            route: sanitize_durable_route(route.as_ref())
                .map_err(|()| DurableHookEventDecodeError::Malformed)?,
            transcript_watermark: protect_optional_hook_structural_id(Some(
                &durable_bound_required_str(&transcript_watermark, DURABLE_MAX_IDENTIFIER_BYTES)
                    .map_err(|()| DurableHookEventDecodeError::Malformed)?,
            ))
            .map_err(|()| DurableHookEventDecodeError::Malformed)?
            .ok_or(DurableHookEventDecodeError::Malformed)?,
        }),
        DurableHookEventPlan::CursorEvent { event } => {
            if crate::mcp::tools::handlers::hook_runtime::cursor_event::validate_queued_cursor_event(
                &event,
            ) {
                Ok(HookEventPlan::CursorEvent(event))
            } else {
                Err(DurableHookEventDecodeError::Malformed)
            }
        }
        DurableHookEventPlan::Noop => Ok(HookEventPlan::Noop),
    }
}

pub(crate) fn encode_durable_hook_event_plan(plan: &HookEventPlan) -> Result<Vec<u8>, ()> {
    let plan = durable_plan_from_runtime(plan)?;
    serde_json::to_vec(&DurableHookEventEnvelope {
        version: DURABLE_HOOK_EVENT_ENVELOPE_VERSION,
        plan,
    })
    .map_err(|_| ())
}

pub(crate) fn decode_durable_hook_event_plan(
    payload: &[u8],
) -> Result<HookEventPlan, DurableHookEventDecodeError> {
    let header = serde_json::from_slice::<DurableHookEventEnvelopeHeader>(payload)
        .map_err(|_| DurableHookEventDecodeError::Malformed)?;
    if !matches!(
        header.version,
        LEGACY_DURABLE_HOOK_EVENT_ENVELOPE_VERSION | DURABLE_HOOK_EVENT_ENVELOPE_VERSION
    ) {
        return Err(DurableHookEventDecodeError::UnsupportedVersion);
    }
    let durable = serde_json::from_slice::<DurableHookEventEnvelope>(payload)
        .map_err(|_| DurableHookEventDecodeError::Malformed)?
        .plan;
    runtime_plan_from_durable(durable)
}

pub(crate) fn parse_hook_event(params: Option<&Value>) -> Option<HookEvent> {
    let mut event =
        serde_json::from_value::<crate::daemon::DaemonHookEvent>(params?.clone()).ok()?;
    if let Some(route) = &mut event.route {
        protect_hook_route_structural_ids(route).ok()?;
    }
    if let Some(receipt) = &mut event.receipt {
        protect_hook_receipt_structural_ids(receipt).ok()?;
    }
    Some(HookEvent {
        agent: HookAgent::from_wire(&event.agent)?,
        kind: HookEventKind::from_wire(&event.event)?,
        rel_paths: safe_hook_rel_paths(&event.rel_paths),
        // Shell text is an untyped observation. Keep only a content-free
        // presence bit for telemetry; discard the text before admission or
        // planning.
        had_command: event
            .command
            .as_deref()
            .is_some_and(|command| !command.is_empty()),
        cwd: event.cwd,
        route: event.route,
        receipt: event.receipt,
    })
}

pub(crate) fn plan_hook_event(
    event: &HookEvent,
    _project_root: &Path,
    _current_branch: Option<&str>,
) -> HookEventPlan {
    match event.kind {
        HookEventKind::FileEdit => {
            if event.rel_paths.is_empty() {
                HookEventPlan::Noop
            } else {
                HookEventPlan::SyncFiles(event.rel_paths.clone())
            }
        }
        // Shell observations cannot mint branch/worktree/sync authority.
        // Native Git reconciliation and typed host records own those effects.
        HookEventKind::Shell => HookEventPlan::Noop,
        HookEventKind::WorkspaceOpen | HookEventKind::SessionStart => {
            HookEventPlan::DebouncedIncrementalSync(event.agent)
        }
        HookEventKind::IncrementalSync if !event.rel_paths.is_empty() => {
            HookEventPlan::SyncFiles(event.rel_paths.clone())
        }
        HookEventKind::IncrementalSync => HookEventPlan::DebouncedIncrementalSync(event.agent),
        HookEventKind::TerminalReceipt | HookEventKind::TurnCompleted => event
            .receipt
            .clone()
            .map_or(HookEventPlan::Noop, |receipt| {
                HookEventPlan::RecordTerminalReceipt {
                    route: event.route.clone(),
                    receipt,
                }
            }),
        HookEventKind::TurnIngested => event
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.transcript_watermark.clone())
            .map_or(HookEventPlan::Noop, |transcript_watermark| {
                HookEventPlan::MarkTurnIngested {
                    route: event.route.clone(),
                    transcript_watermark,
                }
            }),
    }
}

fn safe_hook_rel_paths(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| {
            let path_ref = Path::new(path.as_str());
            !path.is_empty()
                && !path_ref.is_absolute()
                && path_ref.components().all(|component| {
                    !matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::{
        DurableHookEventDecodeError, HookAgent, HookEvent, HookEventKind, HookEventPlan,
        decode_durable_hook_event_plan, encode_durable_hook_event_plan, parse_hook_event,
        plan_hook_event,
    };

    fn parse_or_panic(params: &serde_json::Value) -> HookEvent {
        match parse_hook_event(Some(params)) {
            Some(event) => event,
            None => panic!("hook event should parse"),
        }
    }

    #[test]
    fn parses_agent_and_event_kind_from_hook_notification() {
        let params = json!({
            "agent": "cursor",
            "event": "afterFileEdit",
            "rel_paths": ["src/lib.rs", "../outside.rs", "/tmp/outside.rs", ""]
        });

        let event = parse_or_panic(&params);

        assert_eq!(event.agent, HookAgent::Cursor);
        assert_eq!(event.kind, HookEventKind::FileEdit);
        assert_eq!(event.rel_paths, ["src/lib.rs"]);
    }

    #[test]
    fn maps_shell_and_workspace_events_to_typed_kinds() {
        let shell = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "git pull --rebase",
            "cwd": "/tmp/project"
        });
        let workspace = json!({
            "agent": "kiro",
            "event": "workspaceOpen"
        });

        let shell = parse_or_panic(&shell);
        let workspace = parse_or_panic(&workspace);

        assert_eq!(shell.agent, HookAgent::Codex);
        assert_eq!(shell.kind, HookEventKind::Shell);
        assert!(shell.had_command);
        assert_eq!(workspace.agent, HookAgent::Kiro);
        assert_eq!(workspace.kind, HookEventKind::WorkspaceOpen);
    }

    #[test]
    fn shell_emitters_do_not_put_command_text_on_the_wire() {
        for event in [
            crate::daemon::DaemonHookEvent::cursor_after_shell_execution(PathBuf::from("/project")),
            crate::daemon::DaemonHookEvent::post_tool_use_shell(
                HookAgent::Codex,
                PathBuf::from("/project"),
            ),
        ] {
            let wire = serde_json::to_value(event).unwrap();
            assert!(wire.get("command").is_none());
        }
    }

    #[test]
    fn preserves_route_metadata_from_hook_notification() {
        let params = json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "cargo test",
            "cwd": "/tmp/project",
            "route": {
                "session_id": "session-123",
                "thread_id": "thread-456",
                "cwd": "/tmp/project",
                "worktree": "/tmp/project-worktree",
                "branch": "feature/hook-route"
            }
        });

        let event = parse_or_panic(&params);

        let Some(route) = event.route.as_ref() else {
            panic!("route metadata should parse");
        };
        assert_eq!(route.session_id.as_deref(), Some("session-123"));
        assert_eq!(route.thread_id.as_deref(), Some("thread-456"));
        assert_eq!(route.cwd.as_deref(), Some(Path::new("/tmp/project")));
        assert_eq!(
            route.worktree.as_deref(),
            Some(Path::new("/tmp/project-worktree"))
        );
        assert_eq!(route.branch.as_deref(), Some("feature/hook-route"));
    }

    #[test]
    fn ignores_unknown_hook_event_names() {
        let params = json!({
            "agent": "cursor",
            "event": "futureEvent"
        });

        assert!(parse_hook_event(Some(&params)).is_none());
    }

    #[test]
    fn ignores_unknown_hook_agents() {
        let params = json!({
            "agent": "future-agent",
            "event": "postToolUse"
        });

        assert!(parse_hook_event(Some(&params)).is_none());
    }

    /// Regression: the receiver used to keep its own agent string match, so
    /// the claude-keyed events added for Claude `PostToolUse` were silently
    /// dropped. Every agent the send side can construct must parse here.
    #[test]
    fn accepts_every_constructible_hook_agent() {
        for agent in [
            HookAgent::Claude,
            HookAgent::Codex,
            HookAgent::Cursor,
            HookAgent::Kiro,
        ] {
            let params = json!({
                "agent": agent.as_wire(),
                "event": "postToolUseEdit",
                "rel_paths": ["src/lib.rs"],
                "cwd": "/tmp/project"
            });
            let event = parse_or_panic(&params);
            assert_eq!(event.agent, agent);
            assert_eq!(event.kind, HookEventKind::FileEdit);
        }
    }

    #[test]
    fn plans_file_edit_sync_with_sanitized_paths() {
        let params = json!({
            "agent": "cursor",
            "event": "afterFileEdit",
            "rel_paths": ["src/lib.rs", "../outside.rs"]
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::SyncFiles(vec!["src/lib.rs".to_string()])
        );
    }

    #[test]
    fn plans_incremental_sync_with_paths_as_targeted_sync() {
        let params = json!({
            "agent": "kiro",
            "event": "postToolUse",
            "rel_paths": ["src/lib.rs", "../outside.rs"]
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::SyncFiles(vec!["src/lib.rs".to_string()])
        );
    }

    #[test]
    fn shell_command_text_cannot_mint_git_or_sync_authority() {
        let mut admission_source = None;
        for command in [
            "git switch feature/daemon-hooks",
            "git worktree add ../wt feature/daemon-hooks",
            "git -C /foreign/repo reset --hard",
            "git pull --rebase",
        ] {
            let event = parse_or_panic(&json!({
                "agent": "codex",
                "event": "postToolUseShell",
                "command": command,
                "cwd": "/tmp/project"
            }));
            assert!(event.had_command);
            let source = event.admission_source();
            assert_eq!(admission_source.get_or_insert(source.clone()), &source);
            assert_eq!(
                plan_hook_event(&event, Path::new("/tmp/project"), Some("feature/claimed")),
                HookEventPlan::Noop,
                "{command}"
            );
        }
    }

    #[test]
    fn round_trips_session_start_wire_name_and_key() {
        assert_eq!(
            HookEventKind::from_wire("sessionStart"),
            Some(HookEventKind::SessionStart)
        );
        assert_eq!(HookEventKind::SessionStart.as_key(), "session_start");
    }

    #[test]
    fn parses_hermes_terminal_receipt_without_terminal_content() {
        let event = parse_or_panic(&json!({
            "agent": "hermes",
            "event": "terminalReceipt",
            "cwd": "/tmp/project",
            "route": {"session_id": "session-1", "cwd": "/tmp/project"},
            "receipt": {
                "tool_call_id": "call-1",
                "turn_id": "turn-1",
                "status": "success",
                "duration_ms": 12,
                "transcript_watermark": "turn-1"
            }
        }));
        assert_eq!(event.agent, HookAgent::Hermes);
        assert_eq!(event.kind, HookEventKind::TerminalReceipt);
        assert!(matches!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::RecordTerminalReceipt { receipt, .. }
                if receipt.tool_call_id.as_deref() == Some("call-1")
        ));
    }

    #[test]
    fn plans_projectless_hermes_turn_completion_as_a_review_receipt() {
        let event = parse_or_panic(&json!({
            "agent": "hermes",
            "event": "turnCompleted",
            "route": {"session_id": "session-1"},
            "receipt": {
                "status": "success",
                "transcript_watermark": "message-1"
            }
        }));
        assert_eq!(event.kind, HookEventKind::TurnCompleted);
        assert!(matches!(
            plan_hook_event(&event, Path::new("/tmp/project"), None),
            HookEventPlan::RecordTerminalReceipt { receipt, .. }
                if receipt.transcript_watermark.as_deref() == Some("message-1")
        ));
    }

    #[test]
    fn plans_session_start_as_debounced_convergence() {
        let params = json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": "/tmp/project",
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Claude)
        );
    }

    #[test]
    fn session_start_does_not_mint_branch_or_worktree_authority() {
        let params = json!({
            "agent": "codex",
            "event": "sessionStart",
            "cwd": "/tmp/linked-worktree",
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Codex)
        );
    }

    #[test]
    fn plans_session_start_with_empty_branch_as_debounced_incremental_sync() {
        let params = json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": "/tmp/project",
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("")),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Claude)
        );
    }

    #[test]
    fn plans_cursor_session_start_as_debounced_convergence() {
        let params = serde_json::to_value(crate::daemon::DaemonHookEvent::session_start(
            HookAgent::Cursor,
            PathBuf::from("/tmp/project"),
        ))
        .unwrap();
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Cursor)
        );
    }

    #[test]
    fn plans_workspace_open_as_debounced_convergence() {
        let params = json!({
            "agent": "kiro",
            "event": "workspaceOpen"
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Kiro)
        );
    }

    #[test]
    fn durable_plan_round_trip_preserves_supported_variants() {
        let route = Some(crate::daemon::HookRouteMetadata {
            session_id: Some("session-1".to_string()),
            thread_id: None,
            cwd: None,
            worktree: None,
            branch: None,
        });
        let receipt = crate::daemon::HookTerminalReceipt {
            tool_call_id: None,
            turn_id: None,
            status: Some("success".to_string()),
            duration_ms: Some(4),
            transcript_watermark: Some("message-1".to_string()),
        };
        for plan in [
            HookEventPlan::SyncFiles(vec!["src/lib.rs".to_string()]),
            HookEventPlan::DebouncedIncrementalSync(HookAgent::Cursor),
            HookEventPlan::RecordTerminalReceipt {
                route: route.clone(),
                receipt: receipt.clone(),
            },
            HookEventPlan::MarkTurnIngested {
                route: route.clone(),
                transcript_watermark: "message-1".to_string(),
            },
            HookEventPlan::Noop,
        ] {
            let encoded = encode_durable_hook_event_plan(&plan).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&encoded).unwrap()["version"],
                DURABLE_HOOK_EVENT_ENVELOPE_VERSION
            );
            assert_eq!(decode_durable_hook_event_plan(&encoded).unwrap(), plan);
        }
    }

    #[test]
    fn durable_plan_excludes_unclassified_shell_content() {
        let event = parse_or_panic(&json!({
            "agent": "codex",
            "event": "postToolUseShell",
            "command": "echo provider-secret-content",
            "cwd": "/tmp/project"
        }));
        let source = event.admission_source();
        assert!(source.starts_with("codex:"));
        assert!(!source.contains("provider-secret-content"));
        let plan = plan_hook_event(&event, Path::new("/tmp/project"), Some("main"));
        let encoded = encode_durable_hook_event_plan(&plan).unwrap();
        assert!(
            !String::from_utf8(encoded)
                .unwrap()
                .contains("provider-secret-content")
        );
    }

    #[test]
    fn durable_plan_rejects_malformed_paths_and_agents() {
        assert_eq!(
            decode_durable_hook_event_plan(
                br#"{"version":1,"plan":{"kind":"sync_files","rel_paths":["../private"]}}"#,
            ),
            Err(DurableHookEventDecodeError::Malformed)
        );
        assert_eq!(
            decode_durable_hook_event_plan(
                br#"{"version":1,"plan":{"kind":"debounced_incremental_sync","agent":"unknown"}}"#,
            ),
            Err(DurableHookEventDecodeError::Malformed)
        );
    }

    #[test]
    fn durable_plan_rejects_unsupported_version_before_plan_shape() {
        assert_eq!(
            decode_durable_hook_event_plan(
                br#"{"version":3,"plan":{"kind":"future_host_event","opaque":"ignored"}}"#,
            ),
            Err(DurableHookEventDecodeError::UnsupportedVersion)
        );
    }

    #[test]
    fn durable_plan_strips_route_paths_and_rejects_unbounded_identifiers() {
        let route = Some(crate::daemon::HookRouteMetadata {
            session_id: Some("session-1".to_string()),
            thread_id: Some("thread-1".to_string()),
            cwd: Some(PathBuf::from("/tmp/secret-home")),
            worktree: Some(PathBuf::from("/tmp/secret-worktree")),
            branch: Some("main".to_string()),
        });
        let receipt = crate::daemon::HookTerminalReceipt {
            tool_call_id: Some("call-1".to_string()),
            turn_id: Some("turn-1".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(4),
            transcript_watermark: Some("message-1".to_string()),
        };
        let plan = HookEventPlan::RecordTerminalReceipt {
            route: route.clone(),
            receipt: receipt.clone(),
        };
        let encoded = encode_durable_hook_event_plan(&plan).unwrap();
        let encoded_text = String::from_utf8(encoded.clone()).unwrap();
        assert!(!encoded_text.contains("secret-home"));
        assert!(!encoded_text.contains("secret-worktree"));
        assert!(encoded_text.contains("thread-1"));
        assert!(encoded_text.contains("call-1"));
        assert!(encoded_text.contains("turn-1"));
        assert!(!encoded_text.contains("\"branch\":\"main\""));
        let decoded = decode_durable_hook_event_plan(&encoded).unwrap();
        let HookEventPlan::RecordTerminalReceipt {
            route: decoded_route,
            receipt: decoded_receipt,
        } = decoded
        else {
            panic!("expected RecordTerminalReceipt");
        };
        let decoded_route = decoded_route.expect("route");
        assert_eq!(decoded_route.session_id.as_deref(), Some("session-1"));
        assert_eq!(decoded_route.thread_id.as_deref(), Some("thread-1"));
        assert!(decoded_route.branch.is_none());
        assert!(decoded_route.cwd.is_none());
        assert!(decoded_route.worktree.is_none());
        assert_eq!(decoded_receipt.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(decoded_receipt.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(decoded_receipt.status.as_deref(), Some("success"));
        assert_eq!(decoded_receipt.duration_ms, Some(4));
        assert_eq!(
            decoded_receipt.transcript_watermark.as_deref(),
            Some("message-1")
        );

        let mut private_status = receipt.clone();
        private_status.status = Some("private-status-payload".to_string());
        let encoded = encode_durable_hook_event_plan(&HookEventPlan::RecordTerminalReceipt {
            route: None,
            receipt: private_status,
        })
        .unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("private-status-payload"));
        let HookEventPlan::RecordTerminalReceipt { receipt, .. } =
            decode_durable_hook_event_plan(&encoded).unwrap()
        else {
            panic!("expected receipt");
        };
        assert_eq!(receipt.status.as_deref(), Some("unknown"));

        let oversized = "x".repeat(super::DURABLE_MAX_IDENTIFIER_BYTES + 1);
        assert!(
            encode_durable_hook_event_plan(&HookEventPlan::MarkTurnIngested {
                route: None,
                transcript_watermark: oversized,
            })
            .is_err()
        );
    }

    #[test]
    fn hook_boundary_protects_credential_ids_once_across_durable_receipt_joins() {
        let raw = ["AKIA", "SYNTHETIC", "CANARY", "6"].concat();
        let protected = crate::privacy::protect_sensitive_structural_id(&raw).unwrap();
        let params = serde_json::to_value(crate::daemon::DaemonHookEvent::hermes_terminal_receipt(
            PathBuf::from("/tmp/project"),
            crate::daemon::HookRouteMetadata {
                session_id: Some(raw.clone()),
                thread_id: Some(raw.clone()),
                cwd: Some(PathBuf::from("/tmp/project")),
                worktree: None,
                branch: Some("main".to_string()),
            },
            crate::daemon::HookTerminalReceipt {
                tool_call_id: Some(raw.clone()),
                turn_id: Some(raw.clone()),
                status: Some("success".to_string()),
                duration_ms: Some(1),
                transcript_watermark: Some(raw.clone()),
            },
        ))
        .unwrap();
        let event = parse_or_panic(&params);
        let plan = plan_hook_event(&event, Path::new("/tmp/project"), Some("main"));
        let encoded = encode_durable_hook_event_plan(&plan).unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains(&raw));

        let HookEventPlan::RecordTerminalReceipt { route, receipt } =
            decode_durable_hook_event_plan(&encoded).unwrap()
        else {
            panic!("expected terminal receipt");
        };
        let route = route.expect("protected route");
        for actual in [
            route.session_id.as_deref(),
            route.thread_id.as_deref(),
            receipt.tool_call_id.as_deref(),
            receipt.turn_id.as_deref(),
            receipt.transcript_watermark.as_deref(),
        ] {
            assert_eq!(actual, Some(protected.as_str()));
        }
        assert_eq!(
            crate::privacy::protect_sensitive_structural_id(&protected).unwrap(),
            protected
        );
    }

    #[test]
    fn durable_plan_rejects_legacy_unversioned_payloads_as_malformed() {
        assert_eq!(
            decode_durable_hook_event_plan(br#"{"kind":"noop"}"#),
            Err(DurableHookEventDecodeError::Malformed)
        );
    }

    #[test]
    fn admission_source_is_bounded_private_and_fair_per_host_session() {
        let first = parse_or_panic(&json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": "/tmp/project",
            "route": { "session_id": "private-session-alpha" }
        }));
        let same_session = parse_or_panic(&json!({
            "agent": "claude",
            "event": "postToolUseShell",
            "command": "echo different-event",
            "cwd": "/tmp/project",
            "route": { "session_id": "private-session-alpha" }
        }));
        let other_session = parse_or_panic(&json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": "/tmp/project",
            "route": { "session_id": "private-session-beta" }
        }));
        let first_source = first.admission_source();
        assert_eq!(first_source, same_session.admission_source());
        assert_ne!(first_source, other_session.admission_source());
        assert!(first_source.starts_with("claude:"));
        assert!(first_source.len() < 96);
        assert!(!first_source.contains("private-session-alpha"));

        let fallback = parse_or_panic(&json!({
            "agent": "cursor",
            "event": "afterFileEdit",
            "rel_paths": ["src/one.rs"]
        }));
        let other_fallback = parse_or_panic(&json!({
            "agent": "cursor",
            "event": "afterFileEdit",
            "rel_paths": ["src/two.rs"]
        }));
        assert_eq!(fallback.admission_source(), fallback.admission_source());
        assert_ne!(
            fallback.admission_source(),
            other_fallback.admission_source()
        );
    }
}
