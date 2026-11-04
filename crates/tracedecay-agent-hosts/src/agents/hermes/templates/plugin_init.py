"""tracedecay Hermes plugin registration."""
import copy
import atexit
import json
import hashlib
import logging
import os
import re
import shlex
import shutil
import subprocess
import threading
import time
from collections import deque
from functools import partial
from pathlib import Path

from . import schemas, tools

logger = logging.getLogger(__name__)

# Canonical profile-home resolver (hermes_constants); guarded so the plugin
# still imports outside a Hermes install.
try:
    from hermes_constants import get_hermes_home as _hermes_get_hermes_home
except Exception:
    _hermes_get_hermes_home = None

# Canonical config read/write path for provider save_config(); guarded for
# use outside Hermes (raw-YAML fallback below).
try:
    from hermes_cli import config as _hermes_cli_config
except Exception:
    _hermes_cli_config = None

try:
    from agent.memory_provider import MemoryProvider
except Exception:
    class MemoryProvider:
        pass

# Hermes' centralized auxiliary LLM facade is the MODULE-LEVEL
# agent.auxiliary_client.call_llm(task=..., messages=..., ...). AIAgent
# instances carry no ``auxiliary_client`` attribute and no host call site
# hands the plugin an agent object. Guarded so the plugin still degrades
# gracefully (deterministic fallback summaries) outside a hermes install.
try:
    from agent import auxiliary_client as _hermes_auxiliary_client
except Exception:
    _hermes_auxiliary_client = None

# Stock Hermes' single source of truth for the logical session workspace.
# Multi-session gateways keep this in a ContextVar, so os.getcwd() alone would
# incorrectly route every session through the gateway process directory.
try:
    from agent.runtime_cwd import resolve_agent_cwd as _hermes_resolve_agent_cwd  # type: ignore[import-not-found]
except Exception:
    _hermes_resolve_agent_cwd = None

def _resolve_auxiliary_client(agent=None):
    """Best auxiliary LLM client: an agent-attached one, else hermes' module-level facade."""
    client = getattr(agent, "auxiliary_client", None)
    if client is not None and callable(getattr(client, "call_llm", None)):
        return client
    if _hermes_auxiliary_client is not None and callable(
        getattr(_hermes_auxiliary_client, "call_llm", None)
    ):
        return _hermes_auxiliary_client
    return None

# schemas.json is generated from the MCP tool catalog by the same binary that
# generated this module, so it is the one authority for exact tool names,
# argument schemas, and the `readOnlyHint` annotation. Index it once; every
# exact route below is derived from it rather than spelled again.
TOOL_SCHEMAS_BY_NAME = {schema["name"]: schema for schema in schemas.TOOL_SCHEMAS}
READ_ONLY_TOOL_NAMES = frozenset(
    name
    for name, schema in TOOL_SCHEMAS_BY_NAME.items()
    if schema.get("read_only") is True
)

_FACT_STORE_ROUTE_PREFIX = "tracedecay_fact_store_"
# action -> exact TraceDecay route, for every fact-store operation this
# install's catalog advertises.
FACT_STORE_EXACT_ROUTES = {
    name[len(_FACT_STORE_ROUTE_PREFIX):]: name
    for name in TOOL_SCHEMAS_BY_NAME
    if name.startswith(_FACT_STORE_ROUTE_PREFIX)
}

MEMORY_TOOL_MAP = {
    "fact_store": {"resolve_action": True},
    "fact_feedback": {"tracedecay_name": "tracedecay_fact_feedback"},
    "memory_status": {"tracedecay_name": "tracedecay_memory_status"},
}

# Canonical tools the memory provider must be able to describe. Reported at
# register() time when this install's catalog does not advertise one, instead
# of registering a schema-less stand-in.
MEMORY_PROVIDER_REQUIRED_TOOLS = ("tracedecay_fact_feedback", "tracedecay_memory_status")
MISSING_MEMORY_PROVIDER_TOOLS = tuple(
    name for name in MEMORY_PROVIDER_REQUIRED_TOOLS if name not in TOOL_SCHEMAS_BY_NAME
)

LCM_NATIVE_SCHEMAS = [
    {
        "name": "lcm_grep",
        "description": (
            "Search the plugin-local LCM database for past conversation content. "
            "Default scope is the active session and returns raw messages and summary nodes."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Search query."},
                "limit": {"type": "integer", "description": "Max results to return.", "default": 10},
                "sort": {
                    "type": "string",
                    "enum": ["recency", "relevance", "hybrid"],
                    "description": "How to order matches.",
                    "default": "relevance",
                },
                "session_scope": {
                    "type": "string",
                    "enum": ["current", "all", "session"],
                    "description": "Search scope across the local LCM database.",
                    "default": "current",
                },
                "session_id": {"type": "string", "description": "Session id when session_scope='session'."},
                "source": {"type": "string", "description": "Optional source/platform filter."},
                "role": {
                    "type": "string",
                    "enum": ["system", "user", "assistant", "tool", "unknown"],
                    "description": "Optional raw-message role filter.",
                },
                "time_from": {
                    "anyOf": [{"type": "number"}, {"type": "string"}],
                    "description": "Optional inclusive minimum raw-message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like 'last hour'.",
                },
                "time_to": {
                    "anyOf": [{"type": "number"}, {"type": "string"}],
                    "description": "Optional inclusive maximum raw-message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like 'last hour'.",
                },
            },
            "required": ["query"],
        },
    },
    {
        "name": "lcm_load_session",
        "description": "Load an ordered raw-message transcript page for one explicit session_id.",
        "parameters": {
            "type": "object",
            "properties": {
                "session_id": {"type": "string", "description": "Explicit LCM session id to load."},
                "limit": {"type": "integer", "description": "Maximum raw messages to return.", "default": 100},
                "max_content_chars": {
                    "type": "integer",
                    "description": "Maximum content characters to include per message.",
                    "default": 4000,
                },
                "cursor": {
                    "type": "string",
                    "description": "Authenticated opaque continuation cursor returned as next_cursor.",
                },
                "roles": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional role filter.",
                },
                "time_from": {
                    "type": "number",
                    "description": "Optional inclusive minimum message timestamp.",
                },
                "time_to": {
                    "type": "number",
                    "description": "Optional inclusive maximum message timestamp.",
                },
            },
            "required": ["session_id"],
        },
    },
    {
        "name": "lcm_describe",
        "description": "Inspect a current-session summary node, externalized payload, or top-level DAG overview.",
        "parameters": {
            "type": "object",
            "additionalProperties": False,
            "properties": {
                "node_id": {"type": "string", "description": "Opaque summary node ID to inspect."},
                "externalized_ref": {
                    "type": "string",
                    "description": "Externalized payload ref filename to inspect.",
                },
            },
            "required": [],
            "oneOf": [
                {
                    "not": {
                        "anyOf": [
                            {"required": ["node_id"]},
                            {"required": ["externalized_ref"]},
                        ],
                    },
                },
                {"required": ["node_id"], "not": {"required": ["externalized_ref"]}},
                {"required": ["externalized_ref"], "not": {"required": ["node_id"]}},
            ],
        },
    },
    {
        "name": "lcm_expand",
        "description": "Recover detail behind a summary node, externalized payload, or raw message.",
        "parameters": {
            "type": "object",
            "additionalProperties": False,
            "properties": {
                "node_id": {"type": "string", "description": "Opaque summary node ID to expand."},
                "externalized_ref": {
                    "type": "string",
                    "description": "Externalized payload ref filename to expand.",
                },
                "store_id": {"type": "integer", "description": "Raw message store_id to fetch."},
                "session_id": {
                    "type": "string",
                    "description": "Optional session id override (for example, expand a cross-session grep hit in its owning session).",
                },
                "max_tokens": {"type": "integer", "description": "Token budget for returned content.",
                    "default": 4000},
                "source_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 50,
                    "description": "Maximum immediate sources to return. Continue summary pages only with the authenticated cursor. If a returned source marks content_truncated=true, continue from its own store_id + content_offset.",
                },
                "content_offset": {
                    "type": "integer",
                    "description": "Character offset used to continue oversized content.",
                    "default": 0,
                },
                "cursor": {
                    "type": "string",
                    "description": "Authenticated opaque summary-source continuation cursor.",
                },
            },
            "required": [],
            "oneOf": [
                {
                    "required": ["node_id"],
                    "not": {
                        "anyOf": [
                            {"required": ["store_id"]},
                            {"required": ["externalized_ref"]},
                        ],
                    },
                },
                {
                    "required": ["store_id"],
                    "not": {
                        "anyOf": [
                            {"required": ["node_id"]},
                            {"required": ["externalized_ref"]},
                        ],
                    },
                },
                {
                    "required": ["externalized_ref"],
                    "not": {
                        "anyOf": [
                            {"required": ["node_id"]},
                            {"required": ["store_id"]},
                        ],
                    },
                },
            ],
        },
    },
    {
        "name": "lcm_expand_query",
        "description": "Answer a natural-language question using expanded LCM context from the current session.",
        "parameters": {
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "The question or task to answer from expanded LCM context."},
                "query": {"type": "string", "description": "Optional search query used to find candidate summaries."},
                "node_ids": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional explicit summary node IDs.",
                },
                "max_results": {"type": "integer", "description": "Max candidate summaries.", "default": 5},
                "max_tokens": {"type": "integer", "description": "Max answer tokens.", "default": 2000},
                "context_max_tokens": {
                    "type": "integer",
                    "description": "Expanded context budget for the auxiliary LLM.",
                    "default": 32000,
                },
            },
            "required": ["prompt"],
        },
    },
    {
        "name": "lcm_status",
        "description": "Get a quick health overview of the LCM engine for the current session.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "lcm_doctor",
        "description": "Read LCM database and configuration health without mutation.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
]

# Every Hermes-native LCM tool above fronts the TraceDecay tool of the same
# name; only the argument shape differs (`_translate_lcm_args`).
LCM_TOOL_ALIASES = {
    schema["name"]: "tracedecay_" + schema["name"] for schema in LCM_NATIVE_SCHEMAS
}
LCM_DIRECT_TO_NATIVE = {tracedecay_name: native_name for native_name, tracedecay_name in LCM_TOOL_ALIASES.items()}

STANDARD_HERMES_LCM_PROVIDER = "hermes"

# Direct duplicates of the memory provider's own tool surface
# (fact_store / fact_feedback / memory_status). Skipped at register() time
# when tracedecay is the active memory.provider so the same store is not
# exposed twice per API call. tracedecay_message_search stays registered,
# the provider does not expose transcript search.
MEMORY_PROVIDER_TOOLS = frozenset((
    *FACT_STORE_EXACT_ROUTES.values(),
    *MEMORY_PROVIDER_REQUIRED_TOOLS,
))

# Tool names successfully registered with this host. Consulted by the
# first-turn guidance nudge so it never advertises tools that are not
# actually registered.
_REGISTERED_TOOL_NAMES = set()

def _active_memory_provider(ctx=None):
    """The memory.provider configured for this profile, if any."""
    config = getattr(ctx, "config", None) if ctx is not None else None
    if isinstance(config, dict):
        memory = config.get("memory")
        if isinstance(memory, dict) and memory.get("provider"):
            return str(memory.get("provider"))
    try:
        import yaml
        config_path = os.path.join(tools.hermes_home_dir(), "config.yaml")
        with open(config_path, encoding="utf-8-sig") as config_file:
            raw = yaml.safe_load(config_file) or {}
        memory = raw.get("memory")
        if isinstance(memory, dict) and memory.get("provider"):
            return str(memory.get("provider"))
    except Exception:
        pass
    return None

def _make_wrapped_lcm_handler(tool_name: str, config=None, hermes_home=None):
    return _make_project_safe_handler(
        LCM_TOOL_ALIASES[tool_name],
        partial(_handle_lcm_tool_call, tool_name, config=config),
        hermes_home,
    )

_NUDGE_CODE_REQUEST_RE = re.compile(
    r"\b("
    r"codebase|repo(?:sitory)?|project|workspace|symbol|function|method|class|"
    r"call\s*graph|caller|callee|impact|architecture|module|file|path|diff|"
    r"git|branch|commit|pr|pull\s*request|ci|test|build|compile|lint|"
    r"bug|fix|debug|implement|refactor|review|traceback|stack\s*trace|error"
    r")\b",
    re.IGNORECASE,
)

_NUDGE_PATH_OR_CODE_RE = re.compile(
    r"(```|`[^`]+`|(?:^|\s)[\w./-]+\.(?:py|rs|ts|tsx|js|jsx|go|java|kt|rb|php|c|cc|cpp|h|hpp|cs|swift|toml|ya?ml|json|md)\b|(?:^|\s)[\w.-]+/[\w./-]+)",
    re.IGNORECASE,
)

_NUDGE_GREETING_RE = re.compile(
    r"^\s*(?:hi|hello|hey|yo|sup|howdy|good\s+(?:morning|afternoon|evening)|thanks?|thank\s+you|ok(?:ay)?|yes|no)[\s!.?,~-]*$",
    re.IGNORECASE,
)

def _should_emit_tracedecay_nudge(user_message) -> bool:
    """Return true only when the first user turn is actually code/project work.

    The hook text is appended to the user's first message. For greetings like
    "Hi", an unconditional nudge becomes the most salient content and the model
    may answer by talking about tracedecay instead of simply greeting back.
    """
    text = str(user_message or "").strip()
    if not text or _NUDGE_GREETING_RE.match(text):
        return False
    return bool(_NUDGE_CODE_REQUEST_RE.search(text) or _NUDGE_PATH_OR_CODE_RE.search(text))

def _pre_llm_call(*args, **kwargs):
    # Inject guidance only for first-turn code/project requests. The hook result
    # is appended to the user message, so unconditional text on a greeting ("Hi")
    # can hijack the assistant's response and surface tracedecay when the user
    # did not ask for code work. Keep it first-turn-only for prompt-cache
    # stability, and skip it entirely when no tracedecay tools registered on
    # this host, advertising unregistered tools invites hallucinated calls.
    if not kwargs.get("is_first_turn"):
        return None
    if not _REGISTERED_TOOL_NAMES:
        return None
    if not _plugin_toggle("nudge", True):
        return None
    if not _should_emit_tracedecay_nudge(kwargs.get("user_message")):
        return None
    return (
        "For this codebase request, prefer tracedecay tools for symbol lookup, call graphs, "
        "impact analysis, affected files, and architectural navigation before broad file reads. "
        "For the full workflow, load plugin skill `tracedecay:tracedecay` with `skill_view`."
    )

_TERMINAL_TOOL_NAMES = frozenset((
    "terminal", "bash", "shell", "exec_command", "run_command", "terminal.exec",
))
_HOST_RECEIPT_QUEUE_LIMIT = 64
_HOST_RECEIPT_QUEUE = deque()
_HOST_RECEIPT_QUEUE_CONDITION = threading.Condition()
_HOST_RECEIPT_WORKER = None

def _drain_host_receipts():
    global _HOST_RECEIPT_WORKER
    while True:
        with _HOST_RECEIPT_QUEUE_CONDITION:
            if not _HOST_RECEIPT_QUEUE:
                _HOST_RECEIPT_WORKER = None
                _HOST_RECEIPT_QUEUE_CONDITION.notify_all()
                return
            queued = _HOST_RECEIPT_QUEUE.popleft()
            _HOST_RECEIPT_QUEUE_CONDITION.notify_all()
        try:
            candidate = queued.pop("_project_candidate", None)
            hermes_home = queued.pop("_hermes_home", None)
            trusted_project = queued.pop("_trusted_project", False)
            if candidate:
                route_state, resolved = _project_scope_resolution(
                    candidate, hermes_home
                )
                if route_state == "unregistered" and trusted_project:
                    resolved = _code_project_root(
                        explicit=candidate,
                        hermes_home=hermes_home,
                    )
                if not resolved:
                    continue
                queued["cwd"] = str(resolved)
                route = queued.get("route")
                if isinstance(route, dict):
                    route["cwd"] = str(resolved)
            subprocess.run(
                [tools.TRACEDECAY_BIN, "hook-hermes-terminal-receipt"],
                input=json.dumps(queued),
                text=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=(
                    tools.TRACEDECAY_LONG_TIMEOUT_SECONDS
                    if queued.get("event") == "turnIngested" and not queued.get("cwd")
                    else 2
                ),
                check=False,
            )
        except Exception as exc:
            logger.debug("tracedecay host receipt notification failed: %s", exc)

def _notify_host_receipt(event, thread_name):
    global _HOST_RECEIPT_WORKER
    start_failed = False
    with _HOST_RECEIPT_QUEUE_CONDITION:
        while len(_HOST_RECEIPT_QUEUE) >= _HOST_RECEIPT_QUEUE_LIMIT:
            _HOST_RECEIPT_QUEUE_CONDITION.wait()
        _HOST_RECEIPT_QUEUE.append(event)
        if _HOST_RECEIPT_WORKER is None:
            worker = threading.Thread(
                target=_drain_host_receipts,
                name=thread_name,
                daemon=False,
            )
            _HOST_RECEIPT_WORKER = worker
            try:
                # Start while holding the condition so a concurrent session-end
                # join can never observe a Thread that has not started yet.
                worker.start()
            except Exception:
                _HOST_RECEIPT_WORKER = None
                _HOST_RECEIPT_QUEUE_CONDITION.notify_all()
                start_failed = True
    if start_failed:
        _drain_host_receipts()

def _join_host_receipts():
    while True:
        with _HOST_RECEIPT_QUEUE_CONDITION:
            worker = _HOST_RECEIPT_WORKER
        if worker is None:
            return
        worker.join()

atexit.register(_join_host_receipts)

def _post_tool_call(*args, **kwargs):
    """Send a bounded, fail-open terminal receipt to TraceDecay."""
    payload = {}
    if args and isinstance(args[0], dict):
        payload.update(args[0])
    payload.update(kwargs)
    tool_name = str(payload.get("tool_name") or payload.get("name") or "").lower()
    if tool_name not in _TERMINAL_TOOL_NAMES:
        return None
    tool_args = payload.get("args") if isinstance(payload.get("args"), dict) else {}
    candidate = _code_project_root(
        explicit=payload.get("project_root") or payload.get("project_path"),
        cwd=payload.get("cwd") or tool_args.get("cwd") or tool_args.get("workdir"),
        hermes_home=payload.get("hermes_home"),
    )
    if not candidate:
        return None
    session_id = payload.get("session_id") or payload.get("thread_id")
    turn_id = payload.get("turn_id")
    tool_call_id = payload.get("tool_call_id") or payload.get("call_id")
    status = str(payload.get("status") or ("error" if payload.get("error") else "success"))
    duration = payload.get("duration_ms")
    try:
        duration = max(0, min(int(duration), 86_400_000)) if duration is not None else None
    except (TypeError, ValueError):
        duration = None
    event = {
        "agent": "hermes",
        "event": "terminalReceipt",
        "_project_candidate": str(candidate),
        "_hermes_home": payload.get("hermes_home"),
        "_trusted_project": bool(
            payload.get("project_root") or payload.get("project_path")
        ),
        "route": {
            "session_id": str(session_id)[:256] if session_id else None,
            "thread_id": str(payload.get("thread_id"))[:256] if payload.get("thread_id") else None,
        },
        "receipt": {
            "tool_call_id": str(tool_call_id)[:256] if tool_call_id else None,
            "turn_id": str(turn_id)[:256] if turn_id else None,
            "status": status[:32],
            "duration_ms": duration,
            "transcript_watermark": str(
                payload.get("transcript_watermark") or turn_id or tool_call_id or ""
            )[:256] or None,
        },
    }

    _notify_host_receipt(event, "tracedecay-terminal-receipt")
    return None

def _turn_receipt_event(event_name, session_id, project_root, transcript_watermark):
    route = {"session_id": str(session_id)[:256]}
    event = {
        "agent": "hermes",
        "event": event_name,
        "route": route,
        "receipt": {
            "status": "success",
            "transcript_watermark": str(transcript_watermark)[:256],
        },
    }
    if project_root:
        event["cwd"] = str(project_root)
        route["cwd"] = str(project_root)
    return event

def _notify_turn_completed(session_id, project_root, transcript_watermark):
    _notify_host_receipt(
        _turn_receipt_event(
            "turnCompleted", session_id, project_root, transcript_watermark
        ),
        "tracedecay-turn-completed",
    )

def _notify_turn_ingested(session_id, project_root, transcript_watermark):
    _notify_host_receipt(
        _turn_receipt_event(
            "turnIngested", session_id, project_root, transcript_watermark
        ),
        "tracedecay-turn-ingested",
    )

def _tracedecay_status(raw_args: str = "", hermes_home=None):
    raw = tools.call_tracedecay_tool(
        "tracedecay_status", {}, hermes_home=hermes_home
    )
    try:
        payload = json.loads(json.loads(raw)["content"][0]["text"])
    except Exception:
        return raw
    if not isinstance(payload, dict) or payload.get("error"):
        return raw
    lines = ["tracedecay status:"]
    freshness = payload.get("code_index_freshness")
    freshness_status = (
        freshness.get("status") if isinstance(freshness, dict) else None
    )
    for label, value in (
        ("project", payload.get("project_root")),
        ("files", payload.get("file_count")),
        ("nodes", payload.get("node_count")),
        ("edges", payload.get("edge_count")),
        ("branch", payload.get("active_branch") or payload.get("branch")),
        ("index", freshness_status),
        ("db", payload.get("db_path")),
        ("last sync", payload.get("last_sync")),
    ):
        if value not in (None, ""):
            lines.append(f"  {label}: {value}")
    if len(lines) == 1:
        return raw
    return "\n".join(lines)

def _bridge_preview(value, limit: int = 2048) -> str:
    if isinstance(value, str):
        preview = value
    else:
        try:
            preview = json.dumps(value, sort_keys=True)
        except Exception:
            preview = repr(value)
    if len(preview) > limit:
        return preview[:limit] + "...[truncated]"
    return preview


# Response contract for `tracedecay tool <name> --json`, the only transport
# `tools.call_tracedecay_tool` uses. Its stdout is one of:
#
#   1. `tools.error_payload(...)`: {"error": str, "stdout"?: str, "stderr"?: str}
#      for a non-zero exit, timeout, or non-JSON stdout. The CLI exits non-zero
#      for every daemon-classified failure (`isError`, typed problem envelopes)
#      and for any truncation envelope it could not recover, so those states
#      arrive here already typed as this error dict
#      (crates/tracedecay-cli/src/tool_command.rs::tool_result_process_outcome,
#      ::reject_tool_result_truncation).
#   2. The daemon's MCP tool result, printed verbatim
#      (crates/tracedecay-cli/src/tool_command.rs::rendered_tool_output):
#      {"content": [{"type": "text", "text": <payload>}, ...], "isError"?: bool}.
#      Exactly one text block holds the JSON payload; any other block is a
#      prose trailer such as the stale-graph freshness note
#      (crates/tracedecay/src/daemon/core_client.rs::tool_json_payload is the
#      daemon-side reader of the same shape). Oversized payloads never reach
#      this plugin as a handle envelope: the CLI pages `tracedecay_retrieve`
#      under the original project handshake and request deadline and splices
#      the reassembled payload back into the block before printing
#      (crates/tracedecay-cli/src/commands/daemon.rs::recover_truncated_mcp_result).
#   3. Inside that block, either the tool's own result object (compatibility
#      tools such as tracedecay_status / tracedecay_hook_runtime) or, for
#      retained application surfaces (fact_store_*, fact_feedback,
#      memory_status, message_search, lcm_*), an ApplicationEnvelope whose
#      result sits at outcome.value.payload
#      (crates/tracedecay-contracts/src/result/envelope.rs::ApplicationOutcome,
#      serde tag "outcome" / content "value").
_APPLICATION_OUTCOMES = frozenset(("evidence", "preview", "effect"))

def _json_or_none(text):
    if not isinstance(text, str):
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None

def _content_json_payloads(content):
    """Every content block whose text parses as JSON, in block order."""
    if not isinstance(content, list):
        return []
    decoded = (
        _json_or_none(block.get("text")) for block in content if isinstance(block, dict)
    )
    return [payload for payload in decoded if payload is not None]

def _application_envelope_payload(payload: dict):
    """(result, None) for a plain result or an ApplicationEnvelope's payload;
    (None, error) for an envelope that carries no result payload or for a
    truncation envelope the CLI should have recovered or refused."""
    if (
        payload.get("truncated") is True
        and isinstance(payload.get("original_chars"), int)
        and isinstance(payload.get("preview"), str)
    ):
        # Same predicate as crates/tracedecay-cli/src/commands/daemon.rs::
        # is_truncation_envelope. Recovery is the CLI's job; a preview is
        # never a result.
        return None, "tracedecay tool returned a truncated preview instead of its result"
    outcome = payload.get("outcome")
    if not (
        "contract" in payload
        and isinstance(outcome, dict)
        and outcome.get("outcome") in _APPLICATION_OUTCOMES
    ):
        return payload, None
    packet = outcome.get("value")
    result = packet.get("payload") if isinstance(packet, dict) else None
    if not isinstance(result, dict):
        return None, f"tracedecay {outcome['outcome']} outcome omitted its result payload"
    return result, None

def call_tracedecay_json(name: str, args: dict, **kwargs) -> dict:
    raw = tools.call_tracedecay_tool(name, args, **kwargs)
    try:
        outer = json.loads(raw)
    except json.JSONDecodeError:
        return {
            "error": "tracedecay tool returned invalid JSON",
            "raw_preview": _bridge_preview(raw),
        }
    if isinstance(outer, dict) and "error" in outer:
        return outer
    if not isinstance(outer, dict) or "content" not in outer:
        return {
            "error": "tracedecay tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    payloads = _content_json_payloads(outer.get("content"))
    if not payloads:
        if any(
            isinstance(block, dict) and isinstance(block.get("text"), str)
            for block in outer.get("content") or []
        ):
            return {
                "error": "tracedecay tool returned invalid nested JSON",
                "text_preview": _bridge_preview(outer.get("content")),
            }
        return {
            "error": "tracedecay tool response missing text content",
            "raw_preview": _bridge_preview(raw),
        }
    if len(payloads) != 1 or not isinstance(payloads[0], dict):
        return {
            "error": f"tracedecay tool {name} returned {len(payloads)} JSON payloads; expected one object",
            "text_preview": _bridge_preview(outer.get("content")),
        }
    result, error = _application_envelope_payload(payloads[0])
    if error is not None:
        return {"error": error, "text_preview": _bridge_preview(payloads[0])}
    return result

def _memory_schema(tracedecay_name: str, hermes_name: str):
    """The canonical schema republished under its Hermes provider name, or
    None when this install's catalog does not advertise the tool."""
    schema = TOOL_SCHEMAS_BY_NAME.get(tracedecay_name)
    if schema is None:
        return None
    return {
        "name": hermes_name,
        "description": schema.get("description", ""),
        "parameters": copy.deepcopy(schema.get("parameters", {})),
    }


def _collapsed_fact_store_schema() -> dict:
    """Hermes-facing fact_store(action=...) built from the exact-route catalog."""
    properties = {
        "action": {
            "type": "string",
            "enum": sorted(FACT_STORE_EXACT_ROUTES),
            "description": "Exact fact-store operation to invoke.",
        }
    }
    for tracedecay_name in FACT_STORE_EXACT_ROUTES.values():
        params = TOOL_SCHEMAS_BY_NAME[tracedecay_name].get("parameters") or {}
        for key, value in (params.get("properties") or {}).items():
            properties.setdefault(key, value)
    return {
        "name": "fact_store",
        "description": (
            "Holographic memory fact store. Set action to select an exact route "
            "(add, search, list, and the other catalog operations)."
        ),
        "parameters": {
            "type": "object",
            "properties": properties,
            "required": ["action"],
        },
    }

def _agent_visible_schema(schema: dict) -> dict:
    """Hide routing fields selected by the Hermes session integration."""
    visible = copy.deepcopy(schema)
    properties = (visible.get("parameters") or {}).get("properties")
    if isinstance(properties, dict):
        properties.pop("storage_scope", None)
        properties.pop("hermes_home", None)
    return visible

def _decode_tool_args(arguments):
    if arguments is None:
        return {}
    if isinstance(arguments, dict):
        return arguments
    if isinstance(arguments, str):
        if not arguments.strip():
            return {}
        try:
            return json.loads(arguments)
        except json.JSONDecodeError:
            return {"arguments": arguments}
    return {"arguments": arguments}

def _normalize_memory_tool_call(name, arguments):
    if isinstance(name, dict):
        function = name.get("function") or {}
        tool_name = name.get("name") or function.get("name")
        tool_args = name.get("arguments", function.get("arguments", arguments))
        return tool_name, _decode_tool_args(tool_args)
    return name, _decode_tool_args(arguments)

def _tracedecay_binary_available() -> bool:
    if os.path.dirname(tools.TRACEDECAY_BIN):
        return Path(tools.TRACEDECAY_BIN).is_file() and os.access(tools.TRACEDECAY_BIN, os.X_OK)
    return shutil.which(tools.TRACEDECAY_BIN) is not None

def _project_call_kwargs(project_root=None, kwargs=None):
    """Route the CLI transport without adding fields to MCP arguments."""
    routed = dict(kwargs or {})
    if project_root and not routed.get("project_root"):
        routed["project_root"] = str(project_root)
    return routed

def _lcm_store_args(args, project_root):
    """Select the project shard or the profile-level user session store."""
    routed = dict(args or {})
    if not project_root:
        routed.setdefault("storage_scope", "user")
    return routed

def _project_scope_resolution(project_root, hermes_home=None):
    if not project_root:
        return "unregistered", None
    try:
        project_real = os.path.realpath(str(project_root))
        hermes_real = os.path.realpath(_resolve_hermes_home(hermes_home=hermes_home))
        if os.path.commonpath((hermes_real, project_real)) == hermes_real:
            return "rejected", None
    except (OSError, TypeError, ValueError):
        return "unregistered", None
    candidate = _code_project_root(explicit=project_root, hermes_home=hermes_home)
    if not candidate:
        return "unregistered", None
    try:
        home_real = os.path.realpath(_resolve_hermes_home(hermes_home=hermes_home))
        candidate_real = os.path.realpath(candidate)
        inside_hermes_home = os.path.commonpath((home_real, candidate_real)) == home_real
    except (OSError, TypeError, ValueError):
        inside_hermes_home = False
    unresolved_state = "rejected" if inside_hermes_home else "unregistered"
    unresolved_root = None if inside_hermes_home else candidate
    if not os.path.exists(candidate):
        return unresolved_state, unresolved_root
    try:
        status = call_tracedecay_json(
            "tracedecay_project_context",
            {"project_path": candidate},
        )
    except Exception:
        return unresolved_state, unresolved_root
    if not isinstance(status, dict) or status.get("error"):
        return unresolved_state, unresolved_root
    project = status.get("project") if isinstance(status.get("project"), dict) else status
    resolved = project.get("project_root") or project.get("canonical_root") or project.get("root")
    if not resolved:
        return unresolved_state, unresolved_root
    if not _code_project_root(explicit=resolved, hermes_home=hermes_home):
        return "rejected", None
    try:
        candidate_real = os.path.realpath(str(candidate))
        registered_roots = [resolved]
        for alias in status.get("aliases") or []:
            if isinstance(alias, dict) and alias.get("alias_path"):
                registered_roots.append(alias["alias_path"])
        matched = False
        for root in registered_roots:
            root_real = os.path.realpath(str(root))
            if os.path.commonpath((root_real, candidate_real)) == root_real:
                matched = True
                break
        if not matched:
            return unresolved_state, unresolved_root
    except (OSError, TypeError, ValueError):
        return unresolved_state, unresolved_root
    return "registered", str(resolved)

def _resolved_project_scope(project_root, hermes_home=None):
    state, resolved = _project_scope_resolution(project_root, hermes_home)
    return resolved if state == "registered" else None


def _decoded_tool_arguments(call):
    if not isinstance(call, dict):
        return None, {}
    function = call.get("function") if isinstance(call.get("function"), dict) else call
    name = str(function.get("name") or call.get("name") or "")
    arguments = function.get("arguments", call.get("arguments", {}))
    if isinstance(arguments, str):
        try:
            arguments = json.loads(arguments)
        except Exception:
            arguments = {}
    return name, arguments if isinstance(arguments, dict) else {}

def _terminal_cd_candidates(command):
    try:
        tokens = shlex.split(str(command or ""), posix=os.name != "nt")
    except Exception:
        return []
    candidates = []
    for index, token in enumerate(tokens[:-1]):
        if token == "cd":
            candidates.append(tokens[index + 1])
    return candidates

def _tool_project_candidates(messages):
    if not isinstance(messages, list):
        return []
    start = 0
    for index, message in enumerate(messages):
        if isinstance(message, dict) and message.get("role") == "user":
            start = index
    candidates = []
    for message in messages[start:]:
        if not isinstance(message, dict):
            continue
        calls = message.get("tool_calls") or []
        if isinstance(calls, dict):
            calls = [calls]
        for call in calls:
            name, arguments = _decoded_tool_arguments(call)
            selector = arguments.get("project_selector")
            for candidate in (
                arguments.get("project_root"),
                arguments.get("project_path"),
                _project_selector_path(selector),
                arguments.get("cwd"),
                arguments.get("workdir"),
            ):
                if isinstance(candidate, str) and _path_is_absolute(
                    os.path.expanduser(candidate)
                ):
                    candidates.append(candidate)
            if name in ("terminal", "bash", "shell", "exec_command"):
                candidates.extend(_terminal_cd_candidates(arguments.get("command") or arguments.get("cmd")))
    return candidates

def _turn_project_roots(messages, hermes_home=None):
    roots = []
    seen = set()
    for candidate in reversed(_tool_project_candidates(messages)):
        expanded = os.path.abspath(os.path.expanduser(candidate))
        if os.path.isfile(expanded):
            expanded = os.path.dirname(expanded)
        resolved = _resolved_project_scope(expanded, hermes_home)
        if resolved and resolved not in seen:
            seen.add(resolved)
            roots.append(resolved)
    roots.reverse()
    return roots


_UNSCOPED_DIRECT_TOOLS = frozenset((
    "tracedecay_project_list",
    "tracedecay_project_search",
))

def _project_selector_path(selector):
    if not isinstance(selector, dict):
        return None
    return selector.get("path") or selector.get("project_path")

def _make_project_safe_handler(name, handler, hermes_home):
    def safe_handler(args, **kwargs):
        tool_args = dict(args or {})
        selector = tool_args.get("project_selector")
        selector_path = _project_selector_path(selector)
        explicit_selector = bool(
            tool_args.get("project_id")
            or tool_args.get("project_path")
            or selector_path
            or (isinstance(selector, dict) and selector.get("project_id"))
        )
        # Only a tool the catalog annotates read-only (`readOnlyHint`) may be
        # routed at another registered project by an explicit selector.
        if explicit_selector and name not in READ_ONLY_TOOL_NAMES:
            return tools.error_payload(
                f"{name} does not permit a cross-project mutating selector"
            )
        if explicit_selector and name == "tracedecay_project_context":
            return handler(tool_args, hermes_home=hermes_home)
        candidate = (
            kwargs.get("project_root")
            or kwargs.get("cwd")
            or tool_args.get("project_root")
            or tool_args.get("project_path")
            or selector_path
            or _runtime_working_directory()
        )
        if explicit_selector:
            context_args = {}
            selector_id = tool_args.get("project_id") or (
                selector.get("project_id") if isinstance(selector, dict) else None
            )
            if selector_id:
                context_args["project_id"] = selector_id
            else:
                context_args["path"] = tool_args.get("project_path") or selector_path
            context = call_tracedecay_json(
                "tracedecay_project_context",
                context_args,
                hermes_home=hermes_home,
            )
            project = context.get("project") if isinstance(context, dict) else None
            resolved = (
                project.get("project_root") or project.get("canonical_root")
                if isinstance(project, dict)
                else None
            )
            if resolved and not _code_project_root(
                explicit=resolved, hermes_home=hermes_home
            ):
                resolved = None
        else:
            route_state, resolved = _project_scope_resolution(candidate, hermes_home)
            if route_state == "unregistered" and kwargs.get("project_root"):
                resolved = _code_project_root(
                    explicit=kwargs.get("project_root"),
                    hermes_home=hermes_home,
                )
        if explicit_selector and not resolved:
            return tools.error_payload(
                f"{name} project selector did not resolve to a registered non-Hermes project"
            )
        routed_kwargs = dict(kwargs)
        routed_kwargs["hermes_home"] = hermes_home
        if resolved:
            routed_kwargs["project_root"] = resolved
            return handler(tool_args, **routed_kwargs)
        routed_kwargs.pop("project_root", None)
        routed_kwargs.pop("cwd", None)
        if name in _UNSCOPED_DIRECT_TOOLS:
            return handler(tool_args, **routed_kwargs)
        if name.startswith("tracedecay_lcm_"):
            tool_args.setdefault("storage_scope", "user")
            return handler(tool_args, **routed_kwargs)
        if name in MEMORY_PROVIDER_TOOLS:
            tool_args.setdefault("memory_scope", "user")
            return handler(tool_args, **routed_kwargs)
        if name == "tracedecay_message_search":
            tool_args.setdefault("storage_scope", "user")
            return handler(tool_args, **routed_kwargs)
        return tools.error_payload(
            f"{name} requires a registered project; Hermes home and unregistered workspaces use user scope"
        )
    return safe_handler

# Conventional config home: a `plugins.tracedecay` block in the profile
# config.yaml (the same `plugins.<name>` convention bundled Hermes plugins
# use). Keys are flat and mirror the host-config attribute names the
# `_configured_*` / `_lcm_*_setting` helpers read. Every key the plugin
# consults is declared here (default, dashboard description) so
# register_config_defaults()/get_config_field_meta() expose the real
# surface instead of just the install pin.
PLUGIN_CONFIG_FIELDS = {
    "nudge": (True, "Inject a first-turn tracedecay guidance nudge only for codebase/navigation requests."),
    "sync_turn": (True, "Mirror each completed turn into the LCM raw store."),
    "prefetch": (True, "Background fact recall injected at turn start."),
    "expansion_model": ("", "Model used for lcm_expand_query synthesis."),
    "expansion_context_tokens": ("", "Expanded-context budget for lcm_expand_query (default 32000)."),
    "expansion_timeout_ms": ("", "lcm_expand_query synthesis timeout in milliseconds."),
}

PLUGIN_CONFIG_DEFAULTS = {
    key: default for key, (default, _description) in PLUGIN_CONFIG_FIELDS.items()
}

def _plugin_config_defaults():
    return dict(PLUGIN_CONFIG_DEFAULTS)

def _plugin_toggle(name, default=True):
    """Read a boolean kill switch from the plugins.tracedecay config block.

    config.yaml is the home for behavioral settings (host policy: .env is
    for secrets only).
    """
    value = tools.plugin_config_block().get(name)
    if value is None:
        return default
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in ("0", "false", "no", "off"):
            return False
        if normalized in ("1", "true", "yes", "on"):
            return True
        return default
    return bool(value)

class _ConfigChain:
    """Attribute-style config wrapper layering plugins.tracedecay under a host config object."""

    def __init__(self, primary, block):
        self._primary = primary
        self._block = dict(block)

    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        value = getattr(self._primary, name, None)
        if value is None:
            value = self._block.get(name)
        if value is None:
            raise AttributeError(name)
        return value

def _with_plugin_block(config, hermes_home=None):
    """Layer the profile's plugins.tracedecay config block under a host config.

    Host-provided values always win; the block fills the gaps so profile
    config.yaml settings reach tools and the provider without bespoke env vars.
    """
    block = {
        key: value
        for key, value in tools.plugin_config_block(hermes_home).items()
        if key in PLUGIN_CONFIG_FIELDS and value is not None and value != ""
    }
    if not block:
        return config
    if config is None:
        return dict(block)
    if isinstance(config, dict):
        merged = dict(block)
        for key, value in config.items():
            if value is not None:
                merged[key] = value
        return merged
    return _ConfigChain(config, block)

def _configured_hermes_home(config):
    if config is None:
        return None
    if isinstance(config, dict):
        return config.get("hermes_home") or config.get("home")
    for attr in ("hermes_home", "home"):
        value = getattr(config, attr, None)
        if value:
            return value
    return None

def _configured_project_root(config):
    # Accept host runtime context only. The plugins.tracedecay block is
    # filtered by `_with_plugin_block`, so legacy profile pins cannot route
    # TraceDecay storage.
    if config is None:
        return None
    if isinstance(config, dict):
        value = config.get("project_root") or config.get("tracedecay_project_root")
        return str(value) if value else None
    for attr in ("project_root", "tracedecay_project_root"):
        value = getattr(config, attr, None)
        if value:
            return str(value)
    return None


def _runtime_working_directory():
    if _hermes_resolve_agent_cwd is not None:
        try:
            resolved = _hermes_resolve_agent_cwd()
            if resolved:
                candidate = os.path.abspath(os.path.expanduser(str(resolved)))
                if os.path.isdir(candidate):
                    return candidate
        except Exception:
            pass
    raw = os.environ.get("TERMINAL_CWD", "").strip()
    if raw:
        candidate = os.path.abspath(os.path.expanduser(raw))
        if os.path.isdir(candidate):
            return candidate
    return os.getcwd()

def _path_is_absolute(candidate):
    if os.path.isabs(candidate):
        return True
    # ntpath.isabs rejects "/..." spellings as root-relative, but a host may
    # forward POSIX-absolute project roots to a Windows-hosted plugin; the
    # containment checks still run on the realpath'd candidate.
    return candidate.startswith("/")


def _code_project_root(explicit=None, cwd=None, configured=None, hermes_home=None):
    candidate = explicit or cwd or configured or _runtime_working_directory()
    if isinstance(candidate, str) and candidate.strip() and _path_is_absolute(candidate):
        candidate = candidate.strip()
        try:
            candidate_real = os.path.realpath(candidate)
            hermes_real = os.path.realpath(_resolve_hermes_home(hermes_home=hermes_home))
            if os.path.commonpath((hermes_real, candidate_real)) == hermes_real:
                return None
        except (OSError, TypeError, ValueError):
            return None
        return candidate
    return None

def _resolve_hermes_home(config=None, hermes_home=None):
    for candidate in (
        hermes_home,
        _configured_hermes_home(config),
        # The generated package physically belongs to <profile>/plugins.
        # Prefer that stable owner over process-global helpers and inherited
        # environment values when no explicit/configured home was supplied.
        tools.hermes_home_dir(),
    ):
        if candidate:
            return str(candidate)
    if _hermes_get_hermes_home is not None:
        try:
            resolved = _hermes_get_hermes_home()
            if resolved:
                return str(resolved)
        except Exception:
            pass
    return str(tools.hermes_home_dir())

def _configured_value(config, *names, default=None):
    if config is None:
        return default
    if isinstance(config, dict):
        for name in names:
            if name in config and config[name] is not None:
                return config[name]
        return default
    for name in names:
        value = getattr(config, name, None)
        if value is not None:
            return value
    return default

def _configured_int(config, *names, default=None):
    value = _configured_value(config, *names, default=default)
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None

# Env-aware settings mirroring hermes-lcm LCMConfig.from_env: documented LCM_*
# env vars take precedence over host ctx.config attributes, which take
# precedence over the hermes-lcm hardcoded defaults.

def _lcm_str_setting(config, env_key, *names, default=None):
    env_value = os.environ.get(env_key)
    if env_value is not None:
        return env_value
    value = _configured_value(config, *names)
    return value if value is not None else default

def _lcm_int_setting(config, env_key, *names, default=None):
    raw = os.environ.get(env_key)
    if raw is not None:
        try:
            return int(raw)
        except (TypeError, ValueError):
            pass
    return _configured_int(config, *names, default=default)

def _lcm_expansion_model(config):
    value = _lcm_str_setting(config, "LCM_EXPANSION_MODEL", "expansion_model", default="")
    return str(value or "").strip()

def _lcm_clamped_int_setting(config, env_key, *names, default, minimum=1):
    value = _lcm_int_setting(config, env_key, *names, default=default)
    if value is None:
        value = default
    return max(minimum, int(value))

def _lcm_expansion_settings(config):
    return {
        "model": _lcm_expansion_model(config),
        "context_tokens": _lcm_clamped_int_setting(
            config,
            "LCM_EXPANSION_CONTEXT_TOKENS",
            "expansion_context_tokens",
            default=32000,
            minimum=1,
        ),
        "timeout_ms": _lcm_clamped_int_setting(
            config,
            "LCM_EXPANSION_TIMEOUT_MS",
            "expansion_timeout_ms",
            default=120000,
            minimum=1,
        ),
    }

def _lcm_expansion_context_tokens(config):
    return _lcm_expansion_settings(config)["context_tokens"]

def _lcm_expansion_timeout_ms(config):
    return _lcm_expansion_settings(config)["timeout_ms"]

REASONING_TAGS = ("think", "thinking", "reasoning", "thought", "REASONING_SCRATCHPAD")

def _strip_reasoning(text: str) -> str:
    output = text or ""
    for tag in REASONING_TAGS:
        escaped = re.escape(tag)
        output = re.sub(
            rf"<{escaped}>.*?</{escaped}>",
            "",
            output,
            flags=re.IGNORECASE | re.DOTALL,
        )
    return output.strip()

def _llm_response_text(response) -> str:
    if isinstance(response, str):
        return response
    if isinstance(response, dict):
        content = response.get("content")
        if isinstance(content, str):
            return content
        choices = response.get("choices")
        if isinstance(choices, list) and choices:
            message = choices[0].get("message") if isinstance(choices[0], dict) else None
            if isinstance(message, dict) and isinstance(message.get("content"), str):
                return message["content"]
    choices = getattr(response, "choices", None)
    if choices:
        message = getattr(choices[0], "message", None)
        content = getattr(message, "content", None)
        if isinstance(content, str):
            return content
    return "" if response is None else str(response)

def _bounded_expand_query_answer(text: str, max_tokens: int):
    try:
        token_budget = int(max_tokens or 2000)
    except Exception:
        token_budget = 2000
    char_limit = max(1, token_budget) * 4
    answer = (text or "").strip()
    if len(answer) <= char_limit:
        return answer, False
    return answer[:char_limit].rstrip(), True

def _expand_query_degraded_payload(retrieval, reason: str, *, timeout_seconds=None):
    payload = {}
    if isinstance(retrieval, dict):
        for key in (
            "status",
            "prompt",
            "query",
            "model",
            "max_tokens",
            "context_max_tokens",
            "context_truncated",
            "context_pagination",
            "node_ids",
            "matches",
            "provider",
            "session_id",
        ):
            if key in retrieval:
                payload[key] = retrieval[key]
    payload["status"] = payload.get("status") or "ok"
    payload["needs_synthesis"] = False
    payload["degraded"] = True
    payload["error"] = reason
    if timeout_seconds is not None:
        payload["timeout_seconds"] = timeout_seconds
    return payload

def _synthesize_expand_query_payload(retrieval, agent=None, **kwargs):
    if not isinstance(retrieval, dict) or not retrieval.get("needs_synthesis"):
        return retrieval
    client = _resolve_auxiliary_client(agent)
    if client is None or not callable(getattr(client, "call_llm", None)):
        return _expand_query_degraded_payload(
            retrieval,
            "Hermes auxiliary_client.call_llm is unavailable",
        )

    synthesis_prompt = retrieval.get("synthesis_prompt") or {}
    context_blocks = retrieval.get("context_blocks") or []
    system_prompt = synthesis_prompt.get("system") or (
        "You answer questions using expanded LCM retrieval context. "
        "Be concise, factual, and grounded in the provided context. "
        "If the context is insufficient, say so plainly."
    )
    user_prompt = synthesis_prompt.get("user") or (
        f"QUESTION:\n{retrieval.get('prompt', '')}\n\n"
        "EXPANDED CONTEXT:\n"
        f"{json.dumps(context_blocks, ensure_ascii=False, indent=2)}"
    )
    max_tokens = retrieval.get("max_tokens") or kwargs.get("max_tokens") or 2000
    timeout = kwargs.get("timeout") or kwargs.get("expansion_timeout") or 60
    call_kwargs = {
        "task": "compression",
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt},
        ],
        "max_tokens": max_tokens,
        "timeout": timeout,
    }
    model = kwargs.get("model") or retrieval.get("model")
    if model:
        call_kwargs["model"] = model
    try:
        response = client.call_llm(**call_kwargs)
    except TimeoutError:
        return _expand_query_degraded_payload(
            retrieval,
            f"lcm_expand_query synthesis timed out after {float(timeout):.3g}s",
            timeout_seconds=timeout,
        )
    except Exception as exc:
        # The auxiliary client raises RuntimeError / provider SDK / httpx
        # errors too; letting those escape loses the retrieval entirely
        # behind a generic registry error. Degrade with the retrieval
        # payload intact instead.
        return _expand_query_degraded_payload(
            retrieval,
            f"lcm_expand_query synthesis failed: {exc}",
        )

    answer = _strip_reasoning(_llm_response_text(response)).strip()
    if not answer:
        return _expand_query_degraded_payload(
            retrieval,
            "lcm_expand_query synthesis returned an empty answer",
        )
    bounded_answer, truncated = _bounded_expand_query_answer(answer, max_tokens)
    payload = dict(retrieval)
    payload.pop("context_blocks", None)
    payload.pop("synthesis_prompt", None)
    payload["status"] = payload.get("status") or "ok"
    payload["needs_synthesis"] = False
    payload["answer"] = bounded_answer
    if truncated:
        payload["answer_truncated"] = True
    return payload

def _handle_lcm_expand_query(args, **kwargs) -> str:
    kwargs = dict(kwargs)
    agent = kwargs.pop("agent", None)
    config = _with_plugin_block(kwargs.pop("config", None), kwargs.pop("hermes_home", None))
    args = dict(args or {})
    args.setdefault("provider", STANDARD_HERMES_LCM_PROVIDER)
    args.setdefault("context_max_tokens", _lcm_expansion_context_tokens(config))
    retrieval = call_tracedecay_json(
        "tracedecay_lcm_expand_query", args,
        **_project_call_kwargs(kwargs.get("project_root")),
    )
    if kwargs.get("model") is None:
        model = _lcm_expansion_model(config)
        if model:
            kwargs["model"] = model
    if kwargs.get("timeout") is None and kwargs.get("expansion_timeout") is None:
        kwargs["expansion_timeout"] = _lcm_expansion_timeout_ms(config) / 1000
    payload = _synthesize_expand_query_payload(retrieval, agent=agent, **kwargs)
    return json.dumps(payload)


def _tokens_from_native_max(max_tokens):
    if max_tokens is None:
        return None
    try:
        return max(1, min(8192, int(max_tokens) * 4))
    except Exception:
        return None

def _native_expand_target(args: dict):
    provided = [key for key in ("node_id", "store_id", "externalized_ref") if args.get(key) is not None]
    if len(provided) > 1:
        return None, "lcm_expand expects exactly one of node_id, store_id, or externalized_ref"
    if not provided:
        return None, None
    key = provided[0]
    if key == "node_id":
        return {"kind": "summary_node", "node_id": str(args[key])}, None
    if key == "store_id":
        return {"kind": "raw_message", "store_id": args[key]}, None
    return {"kind": "external_payload", "payload_ref": args[key]}, None

def _native_describe_target(args: dict):
    provided = [key for key in ("node_id", "externalized_ref") if args.get(key) is not None]
    if len(provided) > 1:
        return None, "lcm_describe expects at most one of node_id or externalized_ref"
    if not provided:
        return {"kind": "session"}, None
    key = provided[0]
    if key == "node_id":
        return {"kind": "summary_node", "node_id": str(args[key])}, None
    return {"kind": "external_payload", "payload_ref": args[key]}, None

def _translate_lcm_args(native_name: str, args: dict) -> dict:
    translated = dict(args or {})
    if native_name == "lcm_grep":
        if "session_scope" in translated:
            translated["scope"] = translated.pop("session_scope")
        else:
            translated.setdefault("scope", "current")
        translated.setdefault("sort", "relevance")
        translated.setdefault("include_summaries", False)
        translated.setdefault("temporal_mode", {"kind": "current"})
        if "time_from" in translated:
            translated["start_time"] = translated.pop("time_from")
        if "time_to" in translated:
            translated["end_time"] = translated.pop("time_to")
        return translated
    if native_name == "lcm_load_session":
        translated.setdefault("limit", 100)
        translated.setdefault("content_limit", 4000)
        translated.setdefault("temporal_mode", {"kind": "forensic"})
        if "max_content_chars" in translated:
            translated["content_limit"] = translated.pop("max_content_chars")
        if "time_from" in translated:
            translated["start_time"] = translated.pop("time_from")
        if "time_to" in translated:
            translated["end_time"] = translated.pop("time_to")
        return translated
    if native_name == "lcm_describe":
        if "target" not in translated:
            target, error = _native_describe_target(translated)
            if error is not None:
                return {"error": error}
            translated["target"] = target
        translated.pop("node_id", None)
        translated.pop("externalized_ref", None)
        return translated
    if native_name == "lcm_expand":
        if "target" not in translated:
            target, error = _native_expand_target(translated)
            if error is not None:
                return {"error": error}
            if target is not None:
                translated["target"] = target
        for public_key in ("node_id", "store_id", "externalized_ref"):
            translated.pop(public_key, None)
        translated.pop("source_offset", None)
        if translated["target"]["kind"] != "summary_node":
            translated.pop("source_limit", None)
            translated.pop("cursor", None)
        content_limit = _tokens_from_native_max(translated.pop("max_tokens", None))
        if content_limit is not None and "content_limit" not in translated:
            translated["content_limit"] = content_limit
        return translated
    return translated

def _handle_lcm_tool_call(name, arguments=None, config=None, **kwargs) -> str:
    """Dispatch retrieval using the calling Hermes session, without owning compaction."""
    tool_name, tool_args = _normalize_memory_tool_call(name, arguments)
    native_name = LCM_DIRECT_TO_NATIVE.get(tool_name, tool_name)
    tracedecay_name = LCM_TOOL_ALIASES.get(native_name)
    if tracedecay_name is None:
        return tools.error_payload(f"unknown LCM tool: {tool_name}")

    tool_args = _translate_lcm_args(native_name, dict(tool_args))
    if tool_args.get("error"):
        return json.dumps({"error": tool_args["error"]})
    tool_args.setdefault("provider", STANDARD_HERMES_LCM_PROVIDER)
    if kwargs.get("session_id"):
        tool_args.setdefault("session_id", kwargs["session_id"])
    if (
        native_name == "lcm_grep"
        and tool_args.get("scope") == "current"
        and not tool_args.get("session_id")
    ):
        return tools.error_payload("lcm_grep current scope requires a Hermes session_id")
    project_root = kwargs.get("project_root")
    tool_args = _lcm_store_args(tool_args, project_root)
    if tracedecay_name == "tracedecay_lcm_expand_query":
        return _handle_lcm_expand_query(
            tool_args,
            config=config,
            hermes_home=kwargs.get("hermes_home"),
            agent=kwargs.get("agent"),
            **_project_call_kwargs(project_root),
        )
    # Retrieval never sends the host's live messages or other execution kwargs
    # to the CLI. Completed turns still ingest through the memory provider.
    return tools.call_tracedecay_tool(
        tracedecay_name, tool_args, **_project_call_kwargs(project_root)
    )

class TracedecayMemoryProvider(MemoryProvider):
    provider_id = "tracedecay"

    def __init__(self):
        self.hermes_home = None
        self._registered_hermes_home = None
        self.project_root = None
        self.session_id = None
        self.agent_context = ""
        self._prefetch_lock = threading.Lock()
        self._prefetch_cache = {}
        self._sync_turn_sequence = 0

    @property
    def name(self) -> str:
        return "tracedecay"

    def is_available(self) -> bool:
        return _tracedecay_binary_available()

    def initialize(self, session_id=None, **kwargs):
        self.hermes_home = (
            kwargs.get("hermes_home")
            or self._registered_hermes_home
            or _resolve_hermes_home()
        )
        config = _with_plugin_block(kwargs.get("config"), self.hermes_home)
        explicit_project_root = kwargs.get("project_root")
        candidate_root = _code_project_root(
            explicit=explicit_project_root,
            cwd=kwargs.get("cwd"),
            configured=_configured_project_root(config),
            hermes_home=self.hermes_home,
        )
        route_state, resolved_root = _project_scope_resolution(
            candidate_root, self.hermes_home
        )
        if route_state == "registered":
            self.project_root = resolved_root
        elif route_state == "unregistered" and explicit_project_root:
            self.project_root = candidate_root
        else:
            self.project_root = None
        self.session_id = session_id
        # Execution context ("", "cron", "flush", ...): cron/flush runs are
        # not primary conversations and must not write turn state (cron
        # already passes skip_memory=True by design; this is the belt for
        # hosts that initialize the provider anyway).
        self.agent_context = str(kwargs.get("agent_context") or "")

    def post_setup(self, hermes_home, config):
        """`hermes memory setup` hand-off: verify the binary and warm the store.

        tracedecay_memory_status repairs derived holographic vectors/banks
        and creates the resolved user-level store on first touch, so one call doubles
        as install repair + initialization.
        """
        if not _tracedecay_binary_available():
            print(
                f"  tracedecay binary not found at {tools.TRACEDECAY_BIN}. "
                "install it (cargo install tracedecay) and re-run `hermes memory setup`."
            )
            return
        home = str(hermes_home or self.hermes_home or _resolve_hermes_home())
        resolved_config = _with_plugin_block(config, home)
        project_root = _resolved_project_scope(
            self.project_root or _configured_project_root(resolved_config),
            home,
        )
        status = call_tracedecay_json(
            "tracedecay_memory_status",
            {} if project_root else {"memory_scope": "user"},
            **_project_call_kwargs(project_root),
        )
        if isinstance(status, dict) and not status.get("error"):
            facts = status.get("fact_count", status.get("facts"))
            suffix = f" ({facts} facts)" if facts is not None else ""
            label = project_root or "the active TraceDecay user profile"
            print(f"  tracedecay memory store ready for {label}{suffix}.")
        else:
            detail = status.get("error") if isinstance(status, dict) else status
            print(f"  tracedecay memory store check failed: {detail}")

    def system_prompt_block(self):
        # Built once per session by the host during system prompt assembly,
        # cache-stable, unlike per-turn pre_llm_call injection.
        return (
            "tracedecay memory is active: durable facts live in the holographic "
            "fact store. Use fact_store(action='add') for facts worth keeping "
            "across sessions and fact_store(action='search') for explicit "
            "recall; relevant facts are also prefetched automatically."
        )

    def _prefetch_key(self, session_id=""):
        return str(session_id or self.session_id or "")

    def queue_prefetch(self, query, *, session_id=""):
        """Queue background fact recall for the NEXT turn.

        The host calls this after each turn completes; a daemon thread runs
        the subprocess recall and parks the result for prefetch() to consume
        (same shape as the honcho/mem0 providers).
        """
        if not _plugin_toggle("prefetch", True):
            return
        text = str(query or "").strip()
        if not text:
            return
        key = self._prefetch_key(session_id)

        def _worker():
            try:
                recall = self._recall_facts(text)
            except Exception as exc:
                logger.debug("tracedecay queue_prefetch failed: %s", exc)
                return
            with self._prefetch_lock:
                self._prefetch_cache[key] = recall

        threading.Thread(
            target=_worker, name="tracedecay-memory-prefetch", daemon=True
        ).start()

    def prefetch(self, query, *, session_id=""):
        """Return the recall queued at the end of the previous turn.

        Called inline before every API call, so it must never block on a
        subprocess (memory_provider ABC: be fast here, recall in
        queue_prefetch). Empty string means nothing relevant yet.
        """
        del query
        with self._prefetch_lock:
            return self._prefetch_cache.pop(self._prefetch_key(session_id), "") or ""

    def _recall_facts(self, query):
        """Subprocess fact recall, formatted for cache-safe injection.

        Kept small on purpose: the recall is injected context, and the MCP
        envelope truncates tool payloads past 15k chars, so a low limit
        keeps giant facts from corrupting the response JSON.
        """
        text = str(query or "").strip()
        if not text:
            return ""
        payloads = []
        scopes = ["user"] + (["project"] if self.project_root else [])
        for scope in scopes:
            try:
                args = {
                    "query": text[:512],
                    "limit": 3,
                    "memory_scope": scope,
                }
                payload = call_tracedecay_json(
                    "tracedecay_fact_store_search",
                    args,
                    **_project_call_kwargs(self.project_root if scope == "project" else None),
                )
            except Exception as exc:
                logger.debug("tracedecay %s memory prefetch failed: %s", scope, exc)
                continue
            if isinstance(payload, dict) and not payload.get("error"):
                payloads.append((scope, payload))
        lines = []
        seen_content = set()
        for scope, payload in payloads:
            facts = (
                payload.get("hits")
                or payload.get("facts")
                or payload.get("results")
                or []
            )
            for item in facts:
                if not isinstance(item, dict):
                    continue
                fact = item.get("fact") if isinstance(item.get("fact"), dict) else item
                content = str(fact.get("content") or "").strip()
                if not content or content in seen_content:
                    continue
                seen_content.add(content)
                if len(content) > 600:
                    content = content[:600].rstrip() + "..."
                fact_id = fact.get("fact_id")
                prefix = f"[{scope} fact {fact_id}] " if fact_id is not None else f"[{scope}] "
                lines.append(f"- {prefix}{content}")
        return "\n".join(lines)

    def sync_turn(self, user_content, assistant_content, *, session_id="", messages=None):
        """Submit a completed turn to the daemon-owned transcript authority."""
        project_roots = _turn_project_roots(messages, self.hermes_home)
        if not project_roots and self.project_root:
            project_roots = [self.project_root]
        if not _plugin_toggle("sync_turn", True):
            return
        if self.agent_context in ("cron", "flush"):
            # Non-primary execution contexts must not write turn state.
            return
        sid = session_id or self.session_id
        if not sid:
            return
        turn_messages = []
        if user_content:
            turn_messages.append({"role": "user", "content": str(user_content)})
        if assistant_content:
            turn_messages.append({"role": "assistant", "content": str(assistant_content)})
        if not turn_messages:
            return
        self._sync_turn_sequence += 1
        batch_id = self._sync_turn_sequence
        timestamp_ns = time.time_ns()
        timestamp = time.time()
        for idx, entry in enumerate(turn_messages):
            role = str(entry.get("role") or "user")
            entry["id"] = f"tracedecay_sync_{batch_id}_{timestamp_ns}_{idx}_{role}"
            entry["timestamp"] = timestamp
            entry["associated_project_roots"] = list(project_roots)
        # The profile-level user store is the canonical Hermes conversation.
        # Project shards receive projections with the same stable message IDs,
        # so one turn can be searched from every repository it actually touched
        # without binding the long-lived host session to any one project.
        for project_root in [None, *project_roots]:
            # The internal hook tool is not format-capable, so the JSON
            # result this reads back is requested explicitly.
            args = {
                "action": "ingest_transcript",
                "provider": STANDARD_HERMES_LCM_PROVIDER,
                "session_id": sid,
                "messages": turn_messages,
                "user_scope": project_root is None,
                "format": "json",
            }
            try:
                result = call_tracedecay_json(
                    "tracedecay_hook_runtime",
                    args,
                    **_project_call_kwargs(project_root),
                )
                if (
                    not result.get("error")
                    and result.get("status") in ("accepted", "committed", "exact_duplicate")
                ):
                    _notify_turn_completed(sid, project_root, turn_messages[-1]["id"])
                    _notify_turn_ingested(sid, project_root, turn_messages[-1]["id"])
                else:
                    logger.debug(
                        "tracedecay daemon transcript ingest rejected for %s: %s",
                        project_root or "user",
                        result.get("error") or result.get("status"),
                    )
            except Exception as exc:
                logger.debug(
                    "tracedecay daemon transcript ingest failed for %s: %s",
                    project_root or "user",
                    exc,
                )

    def on_memory_write(self, action, target, content, metadata=None):
        """Mirror built-in memory tool writes into the fact store."""
        if action not in ("add", "replace"):
            # Removals carry no stable fact identity to mirror.
            return
        text = str(content or "").strip()
        if not text:
            return
        fact_metadata = {"hermes_action": str(action), "hermes_target": str(target)}
        for key in ("session_id", "platform", "write_origin", "tool_name"):
            value = (metadata or {}).get(key)
            if value:
                fact_metadata[key] = str(value)
        fact_args = {
            "content": text,
            "category": "user_pref" if target == "user" else "general",
            "metadata": fact_metadata,
            "memory_scope": "user" if target == "user" or not self.project_root else "project",
        }
        try:
            tools.call_tracedecay_tool(
                "tracedecay_fact_store_add",
                fact_args,
                **_project_call_kwargs(
                    self.project_root if fact_args["memory_scope"] == "project" else None
                ),
            )
        except Exception as exc:
            logger.debug("tracedecay on_memory_write mirror failed: %s", exc)

    def get_config_schema(self):
        return []

    def get_config_defaults(self):
        # Hermes layers these under DEFAULT_CONFIG so plugins.tracedecay
        # exists in loaded configs without core changes.
        return {"plugins": {"tracedecay": _plugin_config_defaults()}}

    def get_config_field_meta(self):
        # Dashboard config-page hints for every provider-owned dot-path.
        return {
            f"plugins.tracedecay.{key}": {
                "category": "plugins",
                "description": description,
            }
            for key, (_default, description) in PLUGIN_CONFIG_FIELDS.items()
        }

    def save_config(self, values, hermes_home):
        # `hermes memory setup` hands collected non-secret values here; the
        # conventional home is the plugins.tracedecay block. Prefer hermes'
        # canonical raw-read + save path (config lock, atomic write,
        # managed-config guard) and only fall back to raw YAML outside a
        # hermes install.
        updates = {key: value for key, value in (values or {}).items()}
        if _hermes_cli_config is not None and callable(
            getattr(_hermes_cli_config, "save_config", None)
        ):
            try:
                reader = getattr(_hermes_cli_config, "read_raw_config", None)
                existing = reader() if callable(reader) else None
                if not isinstance(existing, dict):
                    existing = {}
                plugins_cfg = existing.get("plugins")
                if not isinstance(plugins_cfg, dict):
                    plugins_cfg = {}
                block = plugins_cfg.get("tracedecay")
                if not isinstance(block, dict):
                    block = {}
                block.update(updates)
                plugins_cfg["tracedecay"] = block
                existing["plugins"] = plugins_cfg
                _hermes_cli_config.save_config(existing)
                return
            except Exception as exc:
                logger.warning(
                    "tracedecay save_config via hermes_cli.config failed; falling back to raw YAML: %s",
                    exc,
                )
        config_path = Path(hermes_home) / "config.yaml"
        try:
            import yaml
            existing = {}
            if config_path.exists():
                with open(config_path, encoding="utf-8-sig") as config_file:
                    existing = yaml.safe_load(config_file) or {}
            plugins_cfg = existing.get("plugins")
            if not isinstance(plugins_cfg, dict):
                plugins_cfg = {}
            block = plugins_cfg.get("tracedecay")
            if not isinstance(block, dict):
                block = {}
            block.update(updates)
            plugins_cfg["tracedecay"] = block
            existing["plugins"] = plugins_cfg
            with open(config_path, "w", encoding="utf-8") as config_file:
                yaml.dump(existing, config_file, default_flow_style=False)
        except Exception as exc:
            logger.warning("tracedecay memory provider save_config failed: %s", exc)

    def get_tool_schemas(self):
        # Collapsed surface: fact_store(action=...) covers every fact-store
        # operation without a per-action schema footprint.
        # A required canonical tool this catalog does not advertise is left
        # out (reported by register()) rather than described by a stand-in.
        described = (
            _memory_schema("tracedecay_fact_feedback", "fact_feedback"),
            _memory_schema("tracedecay_memory_status", "memory_status"),
        )
        return [_collapsed_fact_store_schema(), *(s for s in described if s is not None)]

    def handle_tool_call(self, name, arguments=None, **kwargs) -> str:
        tool_name, tool_args = _normalize_memory_tool_call(name, arguments)
        mapping = MEMORY_TOOL_MAP.get(tool_name)
        if mapping is None:
            return tools.error_payload(f"unknown memory tool: {tool_name}")
        tool_args = dict(tool_args)
        resolved_action = ""
        if mapping.get("resolve_action"):
            resolved_action = str(tool_args.pop("action", "") or "")
            tracedecay_name = FACT_STORE_EXACT_ROUTES.get(resolved_action)
            if tracedecay_name is None:
                return tools.error_payload(
                    f"fact_store action {resolved_action!r} does not map to an exact route"
                )
        else:
            tracedecay_name = mapping["tracedecay_name"]
            if tracedecay_name not in TOOL_SCHEMAS_BY_NAME:
                return tools.error_payload(
                    f"{tool_name} routes to {tracedecay_name}, which this install's tool catalog does not advertise"
                )
        if "memory_scope" not in tool_args:
            category = str(tool_args.get("category") or "")
            if not self.project_root or (
                resolved_action in ("add", "update") and category == "user_pref"
            ):
                tool_args["memory_scope"] = "user"
            else:
                tool_args["memory_scope"] = "project"
        routed_project = self.project_root if tool_args["memory_scope"] == "project" else None
        routed_kwargs = dict(kwargs)
        if routed_project is None:
            routed_kwargs.pop("project_root", None)
        return tools.call_tracedecay_tool(
            tracedecay_name,
            tool_args,
            **_project_call_kwargs(routed_project, routed_kwargs),
        )

def register(ctx):
    context_config = getattr(ctx, "config", None)
    explicit_context_home = (
        getattr(ctx, "hermes_home", None) or getattr(ctx, "_hermes_home", None)
    )
    context_hermes_home = _resolve_hermes_home(
        None,
        explicit_context_home
        or _configured_hermes_home(context_config)
        or tools.hermes_home_dir(),
    )

    def bind_hermes_home(handler):
        def bound(*args, **kwargs):
            kwargs.setdefault("hermes_home", context_hermes_home)
            return handler(*args, **kwargs)
        return bound

    ctx.register_hook("pre_llm_call", _pre_llm_call)
    ctx.register_hook("on_session_end", lambda **_kwargs: _join_host_receipts())
    try:
        ctx.register_hook("post_tool_call", bind_hermes_home(_post_tool_call))
    except Exception as exc:
        logger.debug("tracedecay post_tool_call hook unavailable: %s", exc)
    # Declare the plugins.tracedecay config block so its keys exist in
    # load_config() even before the user edits config.yaml.
    register_config_defaults = getattr(ctx, "register_config_defaults", None)
    if callable(register_config_defaults):
        try:
            register_config_defaults({"plugins": {"tracedecay": _plugin_config_defaults()}})
        except Exception as exc:
            logger.warning("tracedecay config defaults registration failed: %s", exc)
    register_command = getattr(ctx, "register_command", None)
    if callable(register_command):
        register_command(
            "/tracedecay_status",
            bind_hermes_home(_tracedecay_status),
            description="Show tracedecay project status.",
        )

    if callable(getattr(ctx, "register_memory_provider", None)):
        for missing in MISSING_MEMORY_PROVIDER_TOOLS:
            logger.warning(
                "tracedecay memory provider cannot describe %s: this install's tool catalog does not advertise it",
                missing,
            )
        memory_provider = TracedecayMemoryProvider()
        memory_provider._registered_hermes_home = context_hermes_home
        memory_provider.hermes_home = context_hermes_home
        ctx.register_memory_provider(memory_provider)

    # TraceDecay supplies retrieval and memory. Hermes keeps its configured
    # context engine (the built-in compressor by default), which owns native
    # context-window pressure, compression, and conversation boundaries.
    register_tool = getattr(ctx, "register_tool", None)
    tracedecay_is_memory_provider = _active_memory_provider(ctx) == "tracedecay"
    if callable(register_tool):
        for schema in schemas.TOOL_SCHEMAS:
            name = schema["name"]
            if name in MEMORY_PROVIDER_TOOLS and tracedecay_is_memory_provider:
                # The active memory provider already exposes this store as
                # fact_store/fact_feedback/memory_status, registering the
                # prefixed twins would double the schema footprint.
                continue
            raw_handler = (
                _handle_lcm_expand_query
                if name == "tracedecay_lcm_expand_query"
                else tools.make_handler(name, hermes_home=context_hermes_home)
            )
            handler = _make_project_safe_handler(
                name,
                raw_handler,
                context_hermes_home,
            )
            visible_schema = _agent_visible_schema(schema)
            try:
                register_tool(
                    name=name,
                    toolset="tracedecay",
                    schema=visible_schema,
                    handler=handler,
                )
            except Exception as exc:
                logger.warning(
                    "tracedecay tool registration failed for %s; continuing: %s",
                    name,
                    exc,
                )
            else:
                _REGISTERED_TOOL_NAMES.add(name)
        # LCM readers use stored transcripts and forwarded session identity;
        # they do not require access to the live in-memory message list.
        for schema in LCM_NATIVE_SCHEMAS:
            name = schema["name"]
            try:
                register_tool(
                    name=name,
                    toolset="tracedecay",
                    schema=_agent_visible_schema(schema),
                    handler=_make_wrapped_lcm_handler(
                        name, config=context_config, hermes_home=context_hermes_home
                    ),
                    description=schema.get("description", ""),
                )
            except Exception as exc:
                logger.warning(
                    "tracedecay LCM tool registration failed for %s; continuing: %s",
                    name,
                    exc,
                )
            else:
                _REGISTERED_TOOL_NAMES.add(name)
    else:
        logger.info("tracedecay direct tool registration unavailable on this Hermes host")

    register_skill = getattr(ctx, "register_skill", None)
    skills_dir = Path(__file__).parent / "skills"
    if callable(register_skill) and skills_dir.is_dir():
        direct_skills = [
            path
            for path in sorted(skills_dir.iterdir(), key=lambda path: path.name)
            if path.name != "agent-managed"
        ]
        managed_dir = skills_dir / "agent-managed"
        managed_skills = (
            sorted(managed_dir.iterdir(), key=lambda path: path.name)
            if managed_dir.is_dir()
            else []
        )
        registered_skills = set()
        for skill_dir in direct_skills + managed_skills:
            if not skill_dir.is_dir():
                logger.debug("tracedecay skill entry is not a directory; skipping: %s", skill_dir)
                continue
            skill_name = skill_dir.name
            if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", skill_name):
                logger.warning("tracedecay skill name is invalid; skipping: %s", skill_name)
                continue
            if skill_name in registered_skills:
                logger.warning("tracedecay skill name collides; skipping: %s", skill_name)
                continue
            skill_path = skill_dir / "SKILL.md"
            if not skill_path.is_file():
                logger.debug("tracedecay skill has no SKILL.md; skipping: %s", skill_dir)
                continue
            registered_skills.add(skill_name)
            # Hermes derives the plugin namespace and rejects ':' in skill
            # names, so register every bundled/exported skill by bare name.
            try:
                register_skill(skill_name, skill_path)
            except Exception as exc:
                logger.warning(
                    "tracedecay skill registration failed for %s: %s",
                    skill_name,
                    exc,
                )
