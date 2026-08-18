//! Normalizes daemon hook notifications into typed sync plans.
//!
//! This module owns wire-level hook semantics. The MCP server owns graph side
//! effects such as branch tracking, sync execution, and token-map refreshes.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    AddBranch(String),
    AddBranchAt {
        root: PathBuf,
        branch: String,
        agent: HookAgent,
    },
    SyncCurrentBranch {
        branch: String,
        agent: HookAgent,
    },
    DebouncedIncrementalSync(HookAgent),
    RecordTerminalReceipt {
        route: Option<crate::daemon::HookRouteMetadata>,
        receipt: crate::daemon::HookTerminalReceipt,
    },
    MarkTurnIngested {
        route: Option<crate::daemon::HookRouteMetadata>,
        transcript_watermark: String,
    },
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DurableHookEventPlan {
    SyncFiles {
        rel_paths: Vec<String>,
    },
    AddBranch {
        branch: String,
    },
    AddBranchAt {
        root: PathBuf,
        branch: String,
        agent: String,
    },
    SyncCurrentBranch {
        branch: String,
        agent: String,
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
    Noop,
}

/// Durable spool envelope version. Bump when the plan inventory or field policy
/// changes in a non-compatible way; keep decode arms for prior versions when
/// retained spool records must still replay.
const DURABLE_HOOK_EVENT_ENVELOPE_VERSION: u16 = 1;

/// Lookup identifiers needed outside receipt-state equality (session and
/// watermark) stay bounded. Session ids are run through
/// [`crate::privacy::protect_sensitive_structural_id`] so credential-shaped
/// values become stable digests while public ids remain byte-for-byte.
/// Equality-only thread/tool/turn identifiers are hashed before persistence.
const DURABLE_MAX_IDENTIFIER_BYTES: usize = 256;
const DURABLE_MAX_BRANCH_BYTES: usize = 256;
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

/// Normalize an effect root for durable storage. Canonicalization and live
/// project reauthorization happen at replay via [`authorize_add_branch_at_root`].
fn normalize_durable_effect_root(root: &Path) -> Result<PathBuf, ()> {
    bound_absolute_add_branch_at_root(root).map_err(|_| ())
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

fn durable_plan_from_runtime(plan: &HookEventPlan) -> Result<DurableHookEventPlan, ()> {
    Ok(match plan {
        HookEventPlan::SyncFiles(rel_paths) => DurableHookEventPlan::SyncFiles {
            rel_paths: sanitize_durable_rel_paths(rel_paths)?,
        },
        HookEventPlan::AddBranch(branch) => DurableHookEventPlan::AddBranch {
            branch: durable_bound_required_str(branch, DURABLE_MAX_BRANCH_BYTES)?,
        },
        HookEventPlan::AddBranchAt {
            root,
            branch,
            agent,
        } => DurableHookEventPlan::AddBranchAt {
            root: normalize_durable_effect_root(root)?,
            branch: durable_bound_required_str(branch, DURABLE_MAX_BRANCH_BYTES)?,
            agent: agent.as_wire().to_string(),
        },
        HookEventPlan::SyncCurrentBranch { branch, agent } => {
            DurableHookEventPlan::SyncCurrentBranch {
                branch: durable_bound_required_str(branch, DURABLE_MAX_BRANCH_BYTES)?,
                agent: agent.as_wire().to_string(),
            }
        }
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
        DurableHookEventPlan::AddBranch { branch } => Ok(HookEventPlan::AddBranch(
            durable_bound_required_str(&branch, DURABLE_MAX_BRANCH_BYTES)
                .map_err(|()| DurableHookEventDecodeError::Malformed)?,
        )),
        DurableHookEventPlan::AddBranchAt {
            root,
            branch,
            agent,
        } => Ok(HookEventPlan::AddBranchAt {
            root: normalize_durable_effect_root(&root)
                .map_err(|()| DurableHookEventDecodeError::Malformed)?,
            branch: durable_bound_required_str(&branch, DURABLE_MAX_BRANCH_BYTES)
                .map_err(|()| DurableHookEventDecodeError::Malformed)?,
            agent: HookAgent::from_wire(&agent).ok_or(DurableHookEventDecodeError::Malformed)?,
        }),
        DurableHookEventPlan::SyncCurrentBranch { branch, agent } => {
            Ok(HookEventPlan::SyncCurrentBranch {
                branch: durable_bound_required_str(&branch, DURABLE_MAX_BRANCH_BYTES)
                    .map_err(|()| DurableHookEventDecodeError::Malformed)?,
                agent: HookAgent::from_wire(&agent)
                    .ok_or(DurableHookEventDecodeError::Malformed)?,
            })
        }
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
    if header.version != DURABLE_HOOK_EVENT_ENVELOPE_VERSION {
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
    project_root: &Path,
    current_branch: Option<&str>,
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
        HookEventKind::WorkspaceOpen => current_branch
            .filter(|branch| !branch.is_empty())
            .map(|branch| HookEventPlan::SyncCurrentBranch {
                branch: branch.to_string(),
                agent: event.agent,
            })
            .unwrap_or(HookEventPlan::DebouncedIncrementalSync(event.agent)),
        HookEventKind::SessionStart => {
            plan_session_start_hook_event(event, project_root, current_branch)
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

pub(crate) fn sync_marker_path(data_root: &Path, agent: HookAgent) -> PathBuf {
    data_root.join(agent.sync_marker_file())
}

pub(crate) fn should_run_sync(marker: &Path, now_secs: i64, debounce_secs: i64) -> bool {
    crate::hooks::cursor_should_run_sync(now_secs, read_marker_secs(marker), debounce_secs)
}

pub(crate) fn write_sync_marker(marker: &Path, now_secs: i64) {
    let _ = std::fs::write(marker, now_secs.to_string());
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

/// Plans the sync for a `sessionStart` hook.
///
/// In the main checkout this mirrors `WorkspaceOpen`: sync the current branch,
/// or fall back to a debounced incremental sync when the branch is unknown.
///
/// When the event `cwd` is a *linked* git worktree (a harness-created
/// `.claude/worktrees/*` session tree whose `.git` is a gitdir pointer rather
/// than a real directory), we additionally plan `AddBranchAt` against the
/// resolved worktree root so the session gets its own writable branch store
/// instead of the read-only fallback-ancestor DB. The downstream
/// `add_hook_branch_tracking` returns `AlreadyTracked` cheaply and
/// idempotently, so re-planning `AddBranchAt` for an already-tracked worktree
/// branch is a no-op — we do not need branch-meta visibility here.
fn plan_session_start_hook_event(
    event: &HookEvent,
    project_root: &Path,
    current_branch: Option<&str>,
) -> HookEventPlan {
    let cwd = event.cwd.as_deref().unwrap_or(project_root);
    if let Some(plan) = plan_linked_worktree_branch_add(event, cwd, project_root) {
        return plan;
    }
    current_branch
        .filter(|branch| !branch.is_empty())
        .map(|branch| HookEventPlan::SyncCurrentBranch {
            branch: branch.to_string(),
            agent: event.agent,
        })
        .unwrap_or(HookEventPlan::DebouncedIncrementalSync(event.agent))
}

/// When `cwd` resolves to a linked git worktree that belongs to `project_root`,
/// returns an `AddBranchAt` plan for the worktree root and its current branch.
/// Returns `None` for the main checkout, a non-git cwd, or an unrelated repo.
fn plan_linked_worktree_branch_add(
    event: &HookEvent,
    cwd: &Path,
    project_root: &Path,
) -> Option<HookEventPlan> {
    let worktree_root = crate::worktree::git_worktree_root(cwd)?;
    // A linked worktree's git common dir lives outside its own working tree
    // (it points back at the main checkout's `.git`). In the main checkout the
    // common dir is `<root>/.git`, so the two paths match and we bail out.
    let common_dir = crate::worktree::git_common_dir(&worktree_root)?;
    if path_is_inside(&common_dir, &worktree_root) {
        return None;
    }
    if !git_roots_share_common_dir(&worktree_root, project_root) {
        return None;
    }
    let branch = crate::branch::current_branch(&worktree_root)?;
    if branch.is_empty() {
        return None;
    }
    Some(HookEventPlan::AddBranchAt {
        root: worktree_root,
        branch,
        agent: event.agent,
    })
}

/// Effect-time authorization failure for durable branch-write plans
/// ([`HookEventPlan::AddBranch`], [`HookEventPlan::AddBranchAt`],
/// [`HookEventPlan::SyncCurrentBranch`]).
///
/// Queued plans keep their encoded root/branch; this error means the effect
/// must not run until a later replay reauthorizes against live git state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddBranchAtRootAuthError {
    Empty,
    NotAbsolute,
    Unbounded,
    Unresolvable,
    Unauthorized,
}

impl AddBranchAtRootAuthError {
    pub(crate) const fn reason_code(self) -> &'static str {
        match self {
            Self::Empty
            | Self::NotAbsolute
            | Self::Unbounded
            | Self::Unresolvable
            | Self::Unauthorized => "stale_branch_authorization",
        }
    }
}

const MAX_ADD_BRANCH_AT_ROOT_BYTES: usize = DURABLE_MAX_PATH_BYTES;
const MAX_ADD_BRANCH_AT_ROOT_COMPONENTS: usize = 64;

/// Normalize and bound a durable effect root, then freshly reauthorize it
/// against the live project root via canonical path + git common-dir identity.
///
/// Admit-time membership is never reused: removal, replacement, symlink/path
/// swap, or common-dir drift fail closed instead of applying a stale write.
pub(crate) fn authorize_add_branch_at_root(
    planned_root: &Path,
    project_root: &Path,
) -> Result<PathBuf, AddBranchAtRootAuthError> {
    let bounded = bound_absolute_add_branch_at_root(planned_root)?;
    let canonical = bounded
        .canonicalize()
        .map_err(|_| AddBranchAtRootAuthError::Unresolvable)?;
    let live_worktree_root = crate::worktree::git_worktree_root(&canonical)
        .and_then(|root| root.canonicalize().ok())
        .ok_or(AddBranchAtRootAuthError::Unauthorized)?;
    if live_worktree_root != canonical {
        return Err(AddBranchAtRootAuthError::Unauthorized);
    }
    let project_canonical = project_root
        .canonicalize()
        .map_err(|_| AddBranchAtRootAuthError::Unresolvable)?;
    if !root_belongs_to_project(&canonical, &project_canonical) {
        return Err(AddBranchAtRootAuthError::Unauthorized);
    }
    Ok(canonical)
}

/// Revalidate live root identity and current branch immediately before a
/// durable branch-write effect. Admit-time root/branch are never reused.
pub(crate) fn authorize_planned_branch_effect(
    planned_root: &Path,
    project_root: &Path,
    planned_branch: &str,
) -> Result<PathBuf, AddBranchAtRootAuthError> {
    let root = authorize_add_branch_at_root(planned_root, project_root)?;
    if crate::branch::current_branch(&root).as_deref() != Some(planned_branch) {
        return Err(AddBranchAtRootAuthError::Unauthorized);
    }
    Ok(root)
}

fn bound_absolute_add_branch_at_root(path: &Path) -> Result<PathBuf, AddBranchAtRootAuthError> {
    let raw = path.as_os_str().as_encoded_bytes();
    if raw.is_empty() {
        return Err(AddBranchAtRootAuthError::Empty);
    }
    if !path.is_absolute() {
        return Err(AddBranchAtRootAuthError::NotAbsolute);
    }
    if raw.len() > MAX_ADD_BRANCH_AT_ROOT_BYTES || raw.contains(&0) {
        return Err(AddBranchAtRootAuthError::Unbounded);
    }

    let mut components = 0usize;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(AddBranchAtRootAuthError::Unbounded);
            }
            Component::Normal(part) => {
                components = components.saturating_add(1);
                if components > MAX_ADD_BRANCH_AT_ROOT_COMPONENTS {
                    return Err(AddBranchAtRootAuthError::Unbounded);
                }
                normalized.push(part);
            }
        }
    }
    Ok(normalized)
}

fn root_belongs_to_project(root: &Path, project_root: &Path) -> bool {
    paths_same(root, project_root) || git_roots_share_common_dir(root, project_root)
}

fn path_is_inside(path: &Path, root: &Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.starts_with(root)
}

fn git_roots_share_common_dir(a: &Path, b: &Path) -> bool {
    let a_common = crate::worktree::git_common_dir(a);
    let b_common = crate::worktree::git_common_dir(b);
    a_common
        .as_ref()
        .zip(b_common.as_ref())
        .is_some_and(|(a_common, b_common)| paths_same(a_common, b_common))
}

fn paths_same(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn read_marker_secs(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::json;

    use super::{
        AddBranchAtRootAuthError, DurableHookEventDecodeError, HookAgent, HookEvent, HookEventKind,
        HookEventPlan, authorize_add_branch_at_root, decode_durable_hook_event_plan,
        encode_durable_hook_event_plan, parse_hook_event, plan_hook_event,
    };

    fn parse_or_panic(params: &serde_json::Value) -> HookEvent {
        match parse_hook_event(Some(params)) {
            Some(event) => event,
            None => panic!("hook event should parse"),
        }
    }

    /// Resolves the `git` executable to an absolute path exactly once per
    /// process. Under heavy parallel test load (nextest spawns one process per
    /// test, each spawning several `git` subprocesses), a bare
    /// `Command::new("git")` PATH lookup can transiently fail the spawn with
    /// `ENOENT` ("No such file or directory") even though git is installed.
    /// Resolving to an absolute path up front, plus a `GIT` env override,
    /// removes the per-spawn PATH walk and makes the lookup deterministic.
    fn git_program() -> std::ffi::OsString {
        use std::sync::OnceLock;
        static GIT: OnceLock<std::ffi::OsString> = OnceLock::new();
        GIT.get_or_init(|| {
            if let Some(explicit) = std::env::var_os("GIT") {
                return explicit;
            }
            let exe_name = if cfg!(windows) { "git.exe" } else { "git" };
            if let Some(paths) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&paths) {
                    let candidate = dir.join(exe_name);
                    if candidate.is_file() {
                        return candidate.into_os_string();
                    }
                }
            }
            // Fall back to a bare name and let the OS resolve it.
            std::ffi::OsString::from("git")
        })
        .clone()
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        // A cwd that does not yet exist makes the spawn itself fail with
        // ENOENT, which is indistinguishable from git-not-found; guard it so
        // any real failure is attributable.
        assert!(
            cwd.is_dir(),
            "git cwd {cwd:?} should exist before running git {args:?}"
        );
        let git = git_program();
        // Retry a transient spawn ENOENT a few times: under load the initial
        // fork/exec can spuriously fail even with a valid absolute program.
        let mut last_err: Option<std::io::Error> = None;
        let mut output = None;
        for attempt in 0..5 {
            match Command::new(&git).args(args).current_dir(cwd).output() {
                Ok(out) => {
                    output = Some(out);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt < 4 => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
                }
                Err(e) => {
                    panic!("git {args:?} should run (program {git:?}): {e}");
                }
            }
        }
        let output = output.unwrap_or_else(|| {
            panic!("git {args:?} should run (program {git:?}) after retries: {last_err:?}")
        });
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    fn git_test_root(path: &Path) -> std::path::PathBuf {
        path.to_path_buf()
    }

    #[cfg(not(windows))]
    fn git_test_root(path: &Path) -> std::path::PathBuf {
        path.canonicalize()
            .unwrap_or_else(|e| panic!("tempdir should canonicalize: {e}"))
    }

    fn setup_linked_session_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir should create: {e}"));
        let base_root = git_test_root(base.path());
        let project_root = base_root.join("project");
        let worktree_root = base_root.join("session-worktree");
        std::fs::create_dir_all(project_root.join("src"))
            .unwrap_or_else(|e| panic!("project dirs should create: {e}"));
        std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n")
            .unwrap_or_else(|e| panic!("source should write: {e}"));
        run_git(&project_root, &["init", "-b", "main"]);
        run_git(&project_root, &["config", "user.email", "test@test.com"]);
        run_git(&project_root, &["config", "user.name", "Test"]);
        run_git(&project_root, &["add", "."]);
        run_git(&project_root, &["commit", "-m", "initial"]);
        let worktree_arg = worktree_root.to_string_lossy();
        run_git(
            &project_root,
            &[
                "worktree",
                "add",
                worktree_arg.as_ref(),
                "-b",
                "feature/session",
            ],
        );
        (base, project_root, worktree_root)
    }

    fn assert_add_branch_at(plan: HookEventPlan, expected_root: &Path, expected_branch: &str) {
        let HookEventPlan::AddBranchAt {
            root,
            branch,
            agent,
        } = plan
        else {
            panic!("expected AddBranchAt plan, got {plan:?}");
        };
        assert!(
            super::paths_same(&root, expected_root),
            "planned root {root:?} should match expected root {expected_root:?}"
        );
        assert_eq!(branch, expected_branch);
        assert_eq!(agent, HookAgent::Codex);
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
    fn plans_session_start_from_main_checkout_as_current_branch_sync() {
        let (_base, project_root, _worktree_root) = setup_linked_session_worktree();

        let params = json!({
            "agent": "claude",
            "event": "sessionStart",
            "cwd": project_root,
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, &project_root, Some("main")),
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Claude,
            }
        );
    }

    #[test]
    fn plans_session_start_from_linked_worktree_as_branch_add() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();

        let params = json!({
            "agent": "codex",
            "event": "sessionStart",
            "cwd": worktree_root,
        });
        let event = parse_or_panic(&params);

        // The session cwd is the linked worktree, so even though the main
        // checkout reports `main`, the plan tracks the worktree's own branch
        // at the worktree root.
        assert_add_branch_at(
            plan_hook_event(&event, &project_root, Some("main")),
            &worktree_root,
            "feature/session",
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
    fn plans_cursor_session_start_as_current_branch_sync() {
        let params = serde_json::to_value(crate::daemon::DaemonHookEvent::session_start(
            HookAgent::Cursor,
            PathBuf::from("/tmp/project"),
        ))
        .unwrap();
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Cursor,
            }
        );
    }

    #[test]
    fn plans_workspace_open_as_current_branch_sync() {
        let params = json!({
            "agent": "kiro",
            "event": "workspaceOpen"
        });
        let event = parse_or_panic(&params);

        assert_eq!(
            plan_hook_event(&event, Path::new("/tmp/project"), Some("main")),
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Kiro,
            }
        );
    }

    #[test]
    fn durable_plan_round_trip_preserves_supported_variants() {
        let worktree_root = std::env::temp_dir().join("worktree");
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
            HookEventPlan::AddBranch("feature/test".to_string()),
            HookEventPlan::AddBranchAt {
                root: worktree_root,
                branch: "feature/test".to_string(),
                agent: HookAgent::Codex,
            },
            HookEventPlan::SyncCurrentBranch {
                branch: "main".to_string(),
                agent: HookAgent::Claude,
            },
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
                1
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
                br#"{"version":2,"plan":{"kind":"future_host_event","opaque":"ignored"}}"#,
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
        assert!(
            encode_durable_hook_event_plan(&HookEventPlan::AddBranchAt {
                root: PathBuf::from("/tmp/worktree/../escape"),
                branch: "feature".to_string(),
                agent: HookAgent::Codex,
            })
            .is_err()
        );
    }

    #[test]
    fn hook_boundary_protects_credential_ids_once_across_durable_receipt_joins() {
        let raw = ["AKIA", "SYNTHETIC", "CANARY", "6"].concat();
        let protected = crate::privacy::protect_sensitive_structural_id(&raw).unwrap();
        // The sender-side wire shape the Hermes plugin emits; production only
        // deserializes these events.
        let params = serde_json::to_value(crate::daemon::DaemonHookEvent {
            agent: HookAgent::Hermes.as_wire().to_string(),
            event: "terminalReceipt".to_string(),
            rel_paths: Vec::new(),
            command: None,
            cwd: Some(PathBuf::from("/tmp/project")),
            route: Some(crate::daemon::HookRouteMetadata {
                session_id: Some(raw.clone()),
                thread_id: Some(raw.clone()),
                cwd: Some(PathBuf::from("/tmp/project")),
                worktree: None,
                branch: Some("main".to_string()),
            }),
            receipt: Some(crate::daemon::HookTerminalReceipt {
                tool_call_id: Some(raw.clone()),
                turn_id: Some(raw.clone()),
                status: Some("success".to_string()),
                duration_ms: Some(1),
                transcript_watermark: Some(raw.clone()),
            }),
        })
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

    #[test]
    fn add_branch_at_effect_auth_accepts_linked_worktree_canonical_root() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();
        let authorized = authorize_add_branch_at_root(&worktree_root, &project_root)
            .expect("linked worktree should authorize");
        assert_eq!(
            authorized,
            worktree_root
                .canonicalize()
                .expect("worktree should canonicalize")
        );
    }

    #[test]
    fn add_branch_at_effect_auth_rejects_relative_and_parent_escape() {
        let (_base, project_root, _worktree_root) = setup_linked_session_worktree();
        assert_eq!(
            authorize_add_branch_at_root(Path::new("relative-root"), &project_root),
            Err(AddBranchAtRootAuthError::NotAbsolute)
        );
        let escaped = project_root.join("..").join("outside");
        assert_eq!(
            authorize_add_branch_at_root(&escaped, &project_root),
            Err(AddBranchAtRootAuthError::Unbounded)
        );
        assert_eq!(
            authorize_add_branch_at_root(&project_root.join("src"), &project_root),
            Err(AddBranchAtRootAuthError::Unauthorized)
        );
    }

    #[test]
    fn add_branch_at_effect_auth_rejects_removed_or_replaced_root() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();
        let planned = worktree_root.clone();
        authorize_add_branch_at_root(&planned, &project_root).expect("precondition");

        std::fs::remove_dir_all(&worktree_root).expect("remove worktree");
        assert_eq!(
            authorize_add_branch_at_root(&planned, &project_root),
            Err(AddBranchAtRootAuthError::Unresolvable)
        );

        // Replacement at the same path with an unrelated repository must not
        // inherit the queued plan's prior admission.
        std::fs::create_dir_all(worktree_root.join("src")).expect("recreate");
        std::fs::write(worktree_root.join("src/lib.rs"), "pub fn other() {}\n").expect("write");
        run_git(&worktree_root, &["init", "-b", "main"]);
        run_git(&worktree_root, &["config", "user.email", "test@test.com"]);
        run_git(&worktree_root, &["config", "user.name", "Test"]);
        run_git(&worktree_root, &["add", "."]);
        run_git(&worktree_root, &["commit", "-m", "replacement"]);
        assert_eq!(
            authorize_add_branch_at_root(&planned, &project_root),
            Err(AddBranchAtRootAuthError::Unauthorized)
        );
    }

    #[test]
    fn add_branch_at_effect_auth_rejects_common_dir_drift() {
        let (_base, project_root, worktree_root) = setup_linked_session_worktree();
        let planned = worktree_root.clone();
        authorize_add_branch_at_root(&planned, &project_root).expect("precondition");

        let stranger = project_root.parent().expect("base").join("unrelated-repo");
        std::fs::create_dir_all(stranger.join("src")).expect("stranger dirs");
        std::fs::write(stranger.join("src/lib.rs"), "pub fn stranger() {}\n").expect("write");
        run_git(&stranger, &["init", "-b", "main"]);
        run_git(&stranger, &["config", "user.email", "test@test.com"]);
        run_git(&stranger, &["config", "user.name", "Test"]);
        run_git(&stranger, &["add", "."]);
        run_git(&stranger, &["commit", "-m", "stranger"]);
        let stranger_git = stranger
            .join(".git")
            .canonicalize()
            .expect("stranger gitdir");

        // Linked worktrees store a gitdir pointer; rewriting it changes the
        // common-dir identity without changing the planned path string.
        let git_pointer = worktree_root.join(".git");
        assert!(git_pointer.is_file(), "linked worktree uses gitfile");
        std::fs::write(
            &git_pointer,
            format!("gitdir: {}\n", stranger_git.display()),
        )
        .expect("rewrite gitdir pointer");

        assert_eq!(
            authorize_add_branch_at_root(&planned, &project_root),
            Err(AddBranchAtRootAuthError::Unauthorized)
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_branch_at_effect_auth_rejects_symlink_path_swap() {
        let base = tempfile::tempdir().expect("tempdir");
        let base_root = git_test_root(base.path());
        let project_root = base_root.join("project");
        let worktree_root = base_root.join("session-worktree");
        let alias = base_root.join("alias-root");
        std::fs::create_dir_all(project_root.join("src")).expect("project");
        std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n").expect("write");
        run_git(&project_root, &["init", "-b", "main"]);
        run_git(&project_root, &["config", "user.email", "test@test.com"]);
        run_git(&project_root, &["config", "user.name", "Test"]);
        run_git(&project_root, &["add", "."]);
        run_git(&project_root, &["commit", "-m", "initial"]);
        let worktree_arg = worktree_root.to_string_lossy();
        run_git(
            &project_root,
            &[
                "worktree",
                "add",
                worktree_arg.as_ref(),
                "-b",
                "feature/session",
            ],
        );
        std::os::unix::fs::symlink(&worktree_root, &alias).expect("alias symlink");

        let authorized = authorize_add_branch_at_root(&alias, &project_root)
            .expect("alias into linked worktree should authorize");
        assert_eq!(
            authorized,
            worktree_root.canonicalize().expect("canonicalize worktree")
        );

        let stranger = base_root.join("stranger");
        std::fs::create_dir_all(stranger.join("src")).expect("stranger");
        std::fs::write(stranger.join("src/lib.rs"), "pub fn stranger() {}\n").expect("write");
        run_git(&stranger, &["init", "-b", "main"]);
        run_git(&stranger, &["config", "user.email", "test@test.com"]);
        run_git(&stranger, &["config", "user.name", "Test"]);
        run_git(&stranger, &["add", "."]);
        run_git(&stranger, &["commit", "-m", "stranger"]);

        std::fs::remove_file(&alias).expect("remove alias");
        std::os::unix::fs::symlink(&stranger, &alias).expect("swap alias");

        assert_eq!(
            authorize_add_branch_at_root(&alias, &project_root),
            Err(AddBranchAtRootAuthError::Unauthorized)
        );
    }
}
