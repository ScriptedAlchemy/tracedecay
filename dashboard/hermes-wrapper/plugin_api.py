"""Hermes Intelligence dashboard plugin — tokensave-backed API routes.

Mounted at /api/plugins/hermes-intelligence/ by the Hermes dashboard plugin
system.

This is a THIN reverse proxy onto the canonical implementation: a local
``tokensave dashboard`` HTTP server (see the tokensave repo, ``src/dashboard``).
It does not reimplement any data access. The wrapper:

- lazily spawns ``tokensave dashboard --port 0`` bound to 127.0.0.1 (or uses
  an externally managed server via ``TOKENSAVE_DASHBOARD_URL``),
- forwards ``/holographic/*`` -> upstream ``/api/plugins/holographic/*``,
  ``/lcm/*`` -> upstream ``/api/plugins/hermes-lcm/*``, and
  ``/graph/*`` -> upstream ``/api/plugins/graph/*``,
- exposes upstream ``/api/capabilities`` at ``/capabilities`` so the UI (and
  future Hermes-specific extensions) can feature-detect the backend.

Auth: requests inherit the Hermes dashboard session-token middleware (this
router is mounted under ``/api/plugins/...``); the upstream tokensave server
only listens on loopback.

Configuration (environment):

- ``TOKENSAVE_DASHBOARD_URL``      use an existing server instead of spawning.
- ``TOKENSAVE_BIN``                path to the tokensave binary.
- ``TOKENSAVE_DASHBOARD_PROJECT``  project root to serve (must be
  ``tokensave init``-ed); defaults to the Hermes process cwd.

Hermes-only extension: ``POST /curation/llm-plan`` layers LLM-based curation
(ported from the holographic_plus curator's one-shot review tier) on top of
the tokensave server, which itself only does similarity math. See the
"LLM curation" section below.
"""

from __future__ import annotations

import atexit
import ctypes
import json
import logging
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import deque
from pathlib import Path
from typing import Any, IO

from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import JSONResponse

# Hermes' centralized auxiliary LLM client (provider/model resolution, auth,
# request shaping) — used by the Hermes-only LLM curation layer below.
# Guarded so the dashboard proxy still works when the wrapper is exercised
# outside a full Hermes install (e.g. minimal test envs); the LLM curation
# endpoint then reports 503 and capabilities report llm_curation: false.
try:
    from agent.auxiliary_client import call_llm as _hermes_call_llm
except Exception:  # pragma: no cover - aux client requires full hermes env
    _hermes_call_llm = None

router = APIRouter()

logger = logging.getLogger(__name__)

_SPAWN_TIMEOUT_SECONDS = 30.0
_PROXY_TIMEOUT_SECONDS = 30.0
# After a failed spawn, fail fast with the cached error instead of
# re-spawning (and re-waiting up to _SPAWN_TIMEOUT_SECONDS) on every request.
_SPAWN_RETRY_BACKOFF_SECONDS = 30.0
_STDERR_TAIL_LINES = 20

_lock = threading.Lock()
_process: subprocess.Popen | None = None
_base_url: str | None = None
# (monotonic timestamp, detail) of the last failed spawn, for fast-fail.
_last_spawn_failure: tuple[float, str] | None = None

# Linux parent-death guard: without it, atexit-only shutdown orphans the
# spawned server whenever the Hermes host is SIGKILLed / OOM-killed.
_PR_SET_PDEATHSIG = 1
try:
    _libc = ctypes.CDLL(None, use_errno=True) if sys.platform.startswith("linux") else None
except Exception:  # pragma: no cover - exotic libc
    _libc = None


def _child_preexec() -> None:
    """Runs in the forked child: deliver SIGTERM when the parent dies.

    Best-effort, Linux-only (PR_SET_PDEATHSIG). Other platforms rely on
    atexit plus the dead-instance reap in ``_upstream_base``.
    """
    if _libc is not None:
        try:
            _libc.prctl(_PR_SET_PDEATHSIG, signal.SIGTERM, 0, 0, 0)
        except Exception:  # pragma: no cover - prctl unavailable
            pass


def _drain_pipe(pipe: IO[str] | None, sink: deque | None = None) -> None:
    """Continuously consume a child pipe so the ~64KB buffer never fills.

    A blocked pipe stalls the Rust server's eprintln!/logging and freezes all
    proxied requests. ``sink`` (bounded) keeps a tail for error reporting.
    """
    if pipe is None:
        return
    try:
        for line in pipe:
            if sink is not None:
                sink.append(line.rstrip("\n"))
    except Exception:  # pragma: no cover - pipe torn down mid-read
        pass


def _find_tokensave_bin() -> str | None:
    explicit = os.environ.get("TOKENSAVE_BIN")
    if explicit and Path(explicit).is_file():
        return explicit
    found = shutil.which("tokensave")
    if found:
        return found
    # Vendored engine build inside the hermes_intelligence plugin checkout.
    here = Path(__file__).resolve().parent.parent
    for profile in ("release", "debug"):
        candidate = here / "tokensave_engine" / "target" / profile / "tokensave"
        if candidate.is_file():
            return str(candidate)
    return None


def _project_root() -> str:
    return os.environ.get("TOKENSAVE_DASHBOARD_PROJECT") or os.getcwd()


def _dashboard_env() -> dict[str, str]:
    """Environment for the spawned tokensave dashboard process.

    `subprocess.Popen` inherits by default, but constructing the child env
    explicitly makes the Hermes profile contract visible and stable: the
    spawned Rust server must resolve `HERMES_HOME` and any `TOKENSAVE_*`
    overrides exactly like the wrapper process did.
    """
    env = os.environ.copy()
    for key, value in os.environ.items():
        if key == "HERMES_HOME" or key.startswith("TOKENSAVE_"):
            env[key] = value
    return env


def _spawn_dashboard() -> str:
    """Starts ``tokensave dashboard`` and returns its base URL."""
    binary = _find_tokensave_bin()
    if not binary:
        raise HTTPException(
            status_code=503,
            detail=(
                "tokensave binary not found. Install tokensave or set "
                "TOKENSAVE_BIN / TOKENSAVE_DASHBOARD_URL."
            ),
        )
    project = _project_root()
    cmd = [
        binary,
        "dashboard",
        "--host",
        "127.0.0.1",
        "--port",
        "0",
        "--path",
        project,
    ]
    process = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_dashboard_env(),
        preexec_fn=_child_preexec if _libc is not None else None,  # noqa: PLW1509 — minimal prctl-only hook
    )

    # Single reader per pipe, for the child's whole lifetime: the stderr
    # drain keeps a bounded tail for error detail; the stdout reader parses
    # the URL line then KEEPS draining (a stopped reader would eventually
    # block the server on a full pipe buffer and 502 every proxied request).
    stderr_tail: deque = deque(maxlen=_STDERR_TAIL_LINES)
    threading.Thread(
        target=_drain_pipe, args=(process.stderr, stderr_tail), daemon=True
    ).start()

    # The first stdout line is stable: "tokensave dashboard listening on <url>"
    url_ready = threading.Event()
    url_holder: dict[str, str] = {}

    def _read_stdout() -> None:
        if process.stdout is None:
            url_ready.set()
            return
        for line in process.stdout:
            stripped = line.strip()
            if not url_ready.is_set() and "listening on" in stripped:
                url_holder["url"] = stripped.rsplit(" ", 1)[-1].rstrip("/")
                url_ready.set()
        # EOF (process died): unblock the waiter even without a URL.
        url_ready.set()

    threading.Thread(target=_read_stdout, daemon=True).start()
    url_ready.wait(timeout=_SPAWN_TIMEOUT_SECONDS)
    url = url_holder.get("url")

    if url is None:
        # The drain threads own the pipes — never communicate() here (that
        # would race a second reader against them on the same fd).
        try:
            process.terminate()
            process.wait(timeout=5)
        except Exception:
            process.kill()
            try:
                process.wait(timeout=5)  # reap; a kill without wait leaves a zombie
            except Exception:  # pragma: no cover - unkillable child
                pass
        error_lines = list(stderr_tail)[-5:]
        raise HTTPException(
            status_code=503,
            detail=(
                "tokensave dashboard failed to start for project "
                f"{project!r}: " + (" / ".join(error_lines) or "no output")
            ),
        )

    global _process
    _process = process
    logger.info("tokensave dashboard started at %s (project %s)", url, project)
    return url


def _shutdown() -> None:
    global _process
    if _process is not None and _process.poll() is None:
        try:
            _process.terminate()
            _process.wait(timeout=5)
        except Exception:
            _process.kill()
            try:
                _process.wait(timeout=5)  # reap; a kill without wait leaves a zombie
            except Exception:  # pragma: no cover - unkillable child
                pass
    _process = None


atexit.register(_shutdown)


def _upstream_base() -> str:
    """Returns the base URL of the tokensave dashboard server, starting it
    on first use unless an external URL is configured.

    Spawn failures are cached for ``_SPAWN_RETRY_BACKOFF_SECONDS`` so a
    persistently failing spawn (e.g. project root not tokensave-initialized)
    fails fast with a clear 503 instead of serializing every request behind a
    repeated ``_SPAWN_TIMEOUT_SECONDS`` spawn attempt under the module lock.
    """
    configured = os.environ.get("TOKENSAVE_DASHBOARD_URL")
    if configured:
        return configured.rstrip("/")
    global _base_url, _last_spawn_failure
    with _lock:
        if _base_url is not None and _process is not None and _process.poll() is None:
            return _base_url
        if _last_spawn_failure is not None:
            failed_at, detail = _last_spawn_failure
            remaining = _SPAWN_RETRY_BACKOFF_SECONDS - (time.monotonic() - failed_at)
            if remaining > 0:
                raise HTTPException(
                    status_code=503,
                    detail=(
                        f"tokensave dashboard spawn failed recently; retrying in "
                        f"{remaining:.0f}s. Last error: {detail}"
                    ),
                )
            _last_spawn_failure = None
        # Stale-instance reap: clear any previous (dead or live) child before
        # spawning a replacement.
        _shutdown()
        try:
            _base_url = _spawn_dashboard()
        except HTTPException as exc:
            _last_spawn_failure = (time.monotonic(), str(exc.detail))
            raise
        return _base_url


def _proxy(method: str, upstream_path: str, request: Request, body: bytes | None) -> JSONResponse:
    base = _upstream_base()
    query = request.url.query
    url = f"{base}{upstream_path}" + (f"?{query}" if query else "")
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise HTTPException(status_code=502, detail="invalid upstream URL scheme")
    req = urllib.request.Request(
        url,
        data=body if method == "POST" else None,
        method=method,
        headers={"Content-Type": "application/json"} if body else {},
    )
    try:
        with urllib.request.urlopen(req, timeout=_PROXY_TIMEOUT_SECONDS) as resp:  # noqa: S310 — loopback/configured upstream only
            payload = json.loads(resp.read().decode("utf-8"))
            return JSONResponse(payload, status_code=resp.status)
    except urllib.error.HTTPError as exc:
        try:
            payload = json.loads(exc.read().decode("utf-8"))
        except Exception:
            payload = {"detail": str(exc)}
        return JSONResponse(payload, status_code=exc.code)
    except Exception as exc:
        logger.exception("tokensave dashboard proxy request failed")
        raise HTTPException(status_code=502, detail=f"tokensave dashboard unreachable: {exc}")


class _DummyRequest:
    """Minimal Request stand-in for proxy calls without an inbound query."""

    class _URL:
        query = ""

    url = _URL()


@router.get("/capabilities")
def get_capabilities() -> JSONResponse:
    """Backend feature discovery (proxied from the tokensave server).

    Hermes-specific extensions added to this wrapper in later phases should
    merge their own flags into this payload so the UI can feature-detect.
    """
    response = _proxy("GET", "/api/capabilities", _DummyRequest(), None)
    try:
        payload = json.loads(bytes(response.body).decode("utf-8"))
        payload["mode"] = "hermes"
        # Standalone tokensave reports llm_curation: false (similarity-only
        # dedup). Under Hermes the wrapper layers LLM curation on top using
        # Hermes' model access, so feature-detecting UIs can show the option.
        features = payload.get("features")
        if not isinstance(features, dict):
            features = {}
            payload["features"] = features
        features["llm_curation"] = _hermes_call_llm is not None
        payload["llm_curation"] = _hermes_call_llm is not None
        return JSONResponse(payload, status_code=response.status_code)
    except Exception:
        return response


@router.get("/holographic")
@router.get("/holographic/")
def get_holographic_root(request: Request) -> JSONResponse:
    """Holographic memory overview (proxied).

    Forwards to upstream ``GET /api/plugins/holographic/`` — the dashboard
    overview payload (provider status, facts, entities, association graph).
    Query parameters (``q``, ``limit``, ``graph_limit``) pass through verbatim.
    """
    return _proxy("GET", "/api/plugins/holographic/", request, None)


@router.get("/holographic/{path:path}")
def get_holographic(path: str, request: Request) -> JSONResponse:
    """Catch-all GET proxy for the holographic memory API.

    Maps ``/holographic/<path>`` to upstream
    ``GET /api/plugins/holographic/<path>`` (e.g. ``projection``,
    ``similarity``, ``fact/{id}``, ``curation/status``, ``curation/activity``,
    ``curation/preview``), preserving the query string.
    """
    return _proxy("GET", f"/api/plugins/holographic/{path}", request, None)


@router.post("/holographic/{path:path}")
async def post_holographic(path: str, request: Request) -> JSONResponse:
    """Catch-all POST proxy for the holographic memory API.

    Maps ``/holographic/<path>`` to upstream
    ``POST /api/plugins/holographic/<path>`` (e.g. ``curate``,
    ``curate/apply``), forwarding the JSON request body unmodified.
    (There is no archive/restore: curation deletes are permanent.)
    """
    body = await request.body()
    return _proxy("POST", f"/api/plugins/holographic/{path}", request, body)


@router.get("/lcm/{path:path}")
def get_lcm(path: str, request: Request) -> JSONResponse:
    """Catch-all GET proxy for the LCM session-store API.

    Maps ``/lcm/<path>`` to upstream ``GET /api/plugins/hermes-lcm/<path>``
    (e.g. ``overview``, ``search``, ``session/{id}``, ``node/{id}``,
    ``timeline``, ``compression``), preserving the query string.
    """
    return _proxy("GET", f"/api/plugins/hermes-lcm/{path}", request, None)


@router.post("/lcm/{path:path}")
async def post_lcm(path: str, request: Request) -> JSONResponse:
    """Catch-all POST proxy for the LCM session-store API.

    Maps ``/lcm/<path>`` to upstream ``POST /api/plugins/hermes-lcm/<path>``,
    forwarding the JSON request body unmodified. (The current LCM API is
    read-only; this exists so future write endpoints proxy without changes.)
    """
    body = await request.body()
    return _proxy("POST", f"/api/plugins/hermes-lcm/{path}", request, body)


@router.get("/graph/{path:path}")
def get_graph(path: str, request: Request) -> JSONResponse:
    return _proxy("GET", f"/api/plugins/graph/{path}", request, None)


@router.post("/graph/{path:path}")
async def post_graph(path: str, request: Request) -> JSONResponse:
    body = await request.body()
    return _proxy("POST", f"/api/plugins/graph/{path}", request, body)


# ---------------------------------------------------------------------------
# LLM curation (Hermes-only layer)
#
# Ported from plugins/memory/holographic_plus/curator.py's one-shot LLM review
# tier (`_call_llm_oneshot` + `_LLM_SYSTEM_PROMPT` + `_extract_json_plan`),
# adapted to the tokensave curation contract:
#
#   POST <upstream>/api/plugins/holographic/curate/apply
#     {"ops": [{"op": "delete", "fact_id": int, "reason"?: str}
#              | {"op": "merge", "winner_id": int, "loser_ids": [int],
#                 "merged_content"?: str}]}
#
# tokensave has no archive state (curation hard-deletes), so the original
# verdict vocabulary is narrowed: merge stays merge; supersede becomes a
# delete of the stale fact; reflect (time-aware consolidation) becomes a
# merge with `merged_content`; recategorize/retag have no contract op and are
# out of scope here. The duplicate/conflict policy text is kept verbatim in
# spirit: only act on same-subject same-claim facts, conservative by default.
#
# Flow (POST /curation/llm-plan):
#   1. Fetch similar pairs from the tokensave server (`/similarity`) and
#      cluster them (union-find over shared fact ids).
#   2. One LLM call via Hermes' auxiliary client (task="memory_curator", the
#      same task key the original curator used, so provider/model resolution
#      matches), temperature=0, strict-JSON response.
#   3. Validate proposed ops against the reviewed cluster ids (the original's
#      evidence guard) and an optional confidence floor.
#   4. dry_run=true (default): return the plan for UI preview.
#      dry_run=false: POST the contract-shaped ops to the tokensave apply
#      endpoint and return its per-op results alongside the plan.
# ---------------------------------------------------------------------------

_CURATION_LLM_TASK = "memory_curator"
_CURATION_MAX_TOKENS = 2048
_CURATION_DEFAULT_MAX_CLUSTERS = 12
_CURATION_DEFAULT_MIN_CONFIDENCE = 0.5
_CURATION_CLUSTER_CLASSIFICATIONS = {"likely_duplicate", "merge_candidate"}

_CURATION_SYSTEM_PROMPT = (
    "You are a memory hygiene engine for an AI agent's long-term fact store. "
    "You are given candidate fact clusters and must return STRICT JSON "
    "describing one op per reviewed cluster. NEVER invent facts. Be "
    "conservative: only act when confident.\n\n"
    "Duplicate policy: semantic relatedness is not enough. Only merge facts "
    "when they assert the same durable fact about the same subject, with "
    "matching key nouns/numbers/entities or direct textual evidence. Related "
    "facts, same-topic facts, implementation notes about the same project, "
    "and facts that merely share an entity should remain separate (use "
    "\"keep\").\n\n"
    "Conflict policy: when two facts about the SAME subject conflict, keep "
    "the higher-trust one and delete the stale one. Only use age/recency "
    "after the same-subject / same-claim conflict is established (created_at "
    "is the freshness signal; updated_at is maintenance metadata). If the "
    "facts describe an EVOLUTION over time (a preference pivot, not a true "
    "contradiction, e.g. 'used React' then 'switched to Vue'), emit a merge "
    "whose merged_content is ONE time-aware fact built strictly from the "
    "cluster's own text. Distinct contexts that merely look similar are NOT "
    "contradictions - leave them with \"keep\".\n\n"
    "There is NO archive: delete and merge losers are removed permanently, "
    "so prefer \"keep\" whenever unsure.\n\n"
    "Return JSON of shape: {\"ops\": [ ... ]}. Each op MUST include: "
    "cluster_id (string, from the input), op (one of merge, delete, keep), "
    "confidence (0.0-1.0), and reason (short string). Use op \"keep\" for "
    "reviewed clusters that need no change; do not omit keep reviews.\n"
    "Per-op required fields:\n"
    "  merge: {\"winner_id\": <id>, \"loser_ids\": [<id>, ...]} and optional "
    "\"merged_content\" (string) when the winner's text should be replaced "
    "by a consolidated fact.\n"
    "  delete: {\"fact_id\": <id>}\n"
    "Only reference fact ids that appear in the input clusters. "
    "Return ONLY the JSON object."
)


def _get_upstream_json(path: str) -> dict:
    """GET an upstream tokensave path and return the parsed JSON object."""
    base = _upstream_base()
    url = f"{base}{path}"
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=_PROXY_TIMEOUT_SECONDS) as resp:  # noqa: S310 — loopback/configured upstream only
            payload = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        raise HTTPException(
            status_code=502,
            detail=f"tokensave dashboard returned {exc.code} for {path}",
        ) from exc
    except Exception as exc:
        raise HTTPException(
            status_code=502,
            detail=f"tokensave dashboard unreachable: {exc}",
        ) from exc
    if not isinstance(payload, dict):
        raise HTTPException(
            status_code=502, detail=f"unexpected upstream payload for {path}"
        )
    return payload


def _post_upstream_json(path: str, payload: dict) -> tuple[int, Any]:
    """POST JSON to an upstream tokensave path; returns (status, body)."""
    base = _upstream_base()
    url = f"{base}{path}"
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode("utf-8"),
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=_PROXY_TIMEOUT_SECONDS) as resp:  # noqa: S310 — loopback/configured upstream only
            return resp.status, json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        try:
            return exc.code, json.loads(exc.read().decode("utf-8"))
        except Exception:
            return exc.code, {"detail": str(exc)}
    except Exception as exc:
        raise HTTPException(
            status_code=502,
            detail=f"tokensave dashboard unreachable: {exc}",
        ) from exc


def _build_curation_clusters(
    pairs: list, facts_by_id: dict, max_clusters: int
) -> list[dict]:
    """Group candidate similarity pairs into reviewable clusters.

    Union-find over shared fact ids, mirroring the original curator's cluster
    construction (HRR near-duplicate pairs grouped before LLM review). Pairs
    arrive sorted by similarity (upstream sorts descending), so cluster caps
    keep the strongest candidates.
    """
    parent: dict[int, int] = {}

    def find(x: int) -> int:
        while parent.setdefault(x, x) != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: int, b: int) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[rb] = ra

    kept_pairs = []
    for pair in pairs:
        classification = str(pair.get("classification") or "")
        if classification not in _CURATION_CLUSTER_CLASSIFICATIONS:
            continue
        try:
            a_id = int(pair["a_id"])
            b_id = int(pair["b_id"])
        except (KeyError, TypeError, ValueError):
            continue
        union(a_id, b_id)
        kept_pairs.append((a_id, b_id, pair))

    groups: dict[int, dict] = {}
    for a_id, b_id, pair in kept_pairs:
        root = find(a_id)
        group = groups.setdefault(root, {"fact_ids": set(), "pairs": []})
        group["fact_ids"].update((a_id, b_id))
        group["pairs"].append(
            {
                "a_id": a_id,
                "b_id": b_id,
                "similarity": pair.get("similarity"),
                "classification": pair.get("classification"),
            }
        )

    clusters: list[dict] = []
    for index, group in enumerate(groups.values()):
        if len(clusters) >= max_clusters:
            break
        members = []
        for fact_id in sorted(group["fact_ids"]):
            fact = facts_by_id.get(fact_id) or {}
            members.append(
                {
                    "fact_id": fact_id,
                    "content": fact.get("content", ""),
                    "category": fact.get("category", "general"),
                    "tags": fact.get("tags", ""),
                    "trust_score": fact.get("trust_score"),
                    "created_at": fact.get("created_at"),
                    "updated_at": fact.get("updated_at"),
                }
            )
        clusters.append(
            {
                "cluster_id": f"cluster-{index:04d}",
                "members": members,
                "pairs": group["pairs"],
            }
        )
    return clusters


def _call_curation_llm(messages: list) -> str:
    """One-shot curation LLM call through Hermes' auxiliary client.

    Module-level seam (tests monkeypatch this) mirroring the original
    curator's `_call_llm_oneshot`: task="memory_curator" resolves the same
    provider/model auxiliary-task config the holographic_plus curator used.
    """
    if _hermes_call_llm is None:
        raise HTTPException(
            status_code=503,
            detail="hermes auxiliary LLM client unavailable; LLM curation disabled",
        )
    resp = _hermes_call_llm(
        task=_CURATION_LLM_TASK,
        messages=messages,
        temperature=0,
        max_tokens=_CURATION_MAX_TOKENS,
    )
    return resp.choices[0].message.content or ""


def _extract_json_object(text: str) -> dict | None:
    """Strict-JSON extraction with code-fence tolerance (as the original)."""
    if not text:
        return None
    cleaned = text.strip()
    if cleaned.startswith("```") and cleaned.endswith("```"):
        lines = cleaned.splitlines()
        if (
            len(lines) >= 3
            and lines[0].strip() in {"```", "```json"}
            and lines[-1].strip() == "```"
        ):
            cleaned = "\n".join(lines[1:-1]).strip()
    try:
        parsed = json.loads(cleaned)
    except Exception:
        return None
    return parsed if isinstance(parsed, dict) else None


def _validate_llm_ops(
    raw_ops: list, allowed_ids: set, min_confidence: float
) -> tuple[list[dict], list[dict]]:
    """Split LLM-proposed ops into (valid actionable ops, rejected ops).

    Mirrors the original curator's apply-phase validation: required fields,
    op vocabulary, and an evidence guard — every referenced fact id must be a
    member of a reviewed cluster (the LLM may not invent targets). "keep" ops
    are valid but never actionable.
    """
    valid: list[dict] = []
    rejected: list[dict] = []
    for raw in raw_ops:
        if not isinstance(raw, dict):
            rejected.append({"op": raw, "rejected_reason": "not an object"})
            continue
        op = str(raw.get("op") or "")
        try:
            confidence = float(raw.get("confidence", 0.0))
        except (TypeError, ValueError):
            confidence = 0.0
        if op == "keep":
            continue
        if op not in {"merge", "delete"}:
            rejected.append({**raw, "rejected_reason": f"unknown op {op!r}"})
            continue
        if confidence < min_confidence:
            rejected.append(
                {**raw, "rejected_reason": f"confidence {confidence} below floor"}
            )
            continue
        if op == "delete":
            try:
                fact_id = int(raw["fact_id"])
            except (KeyError, TypeError, ValueError):
                rejected.append({**raw, "rejected_reason": "missing/invalid fact_id"})
                continue
            if fact_id not in allowed_ids:
                rejected.append(
                    {**raw, "rejected_reason": "fact_id not in reviewed clusters"}
                )
                continue
            valid.append({**raw, "fact_id": fact_id, "confidence": confidence})
            continue
        # op == "merge"
        try:
            winner_id = int(raw["winner_id"])
            loser_ids = [int(x) for x in raw.get("loser_ids") or []]
        except (KeyError, TypeError, ValueError):
            rejected.append(
                {**raw, "rejected_reason": "missing/invalid winner_id/loser_ids"}
            )
            continue
        if not loser_ids or winner_id in loser_ids:
            rejected.append(
                {**raw, "rejected_reason": "empty loser_ids or winner among losers"}
            )
            continue
        if winner_id not in allowed_ids or any(x not in allowed_ids for x in loser_ids):
            rejected.append(
                {**raw, "rejected_reason": "fact ids not in reviewed clusters"}
            )
            continue
        valid.append(
            {**raw, "winner_id": winner_id, "loser_ids": loser_ids, "confidence": confidence}
        )
    return valid, rejected


def _contract_op(op: dict) -> dict:
    """Strip wrapper annotations down to the tokensave apply contract shape."""
    if op["op"] == "delete":
        out = {"op": "delete", "fact_id": op["fact_id"]}
        if op.get("reason"):
            out["reason"] = str(op["reason"])
        return out
    out = {"op": "merge", "winner_id": op["winner_id"], "loser_ids": op["loser_ids"]}
    if op.get("merged_content"):
        out["merged_content"] = str(op["merged_content"])
    return out


@router.post("/curation/llm-plan")
async def post_curation_llm_plan(request: Request) -> JSONResponse:
    """LLM curation: plan (dry-run, default) or plan + apply.

    Body (all optional):
      {"dry_run": bool = true,
       "limit": int = 200,           # facts fetched for cluster context
       "threshold": float | null,    # similarity threshold override
       "max_clusters": int = 12,
       "min_confidence": float = 0.5}
    """
    try:
        body = await request.json()
    except Exception:
        body = {}
    if not isinstance(body, dict):
        body = {}
    dry_run = bool(body.get("dry_run", True))
    limit = max(1, min(int(body.get("limit", 200) or 200), 500))
    max_clusters = max(1, min(int(body.get("max_clusters", _CURATION_DEFAULT_MAX_CLUSTERS) or _CURATION_DEFAULT_MAX_CLUSTERS), 50))
    min_confidence = float(body.get("min_confidence", _CURATION_DEFAULT_MIN_CONFIDENCE))

    similarity_path = "/api/plugins/holographic/similarity?limit=200"
    threshold = body.get("threshold")
    if threshold is not None:
        similarity_path += f"&threshold={float(threshold)}"
    similarity = _get_upstream_json(similarity_path)
    pairs = similarity.get("pairs") or []

    root = _get_upstream_json(f"/api/plugins/holographic/?limit={limit}")
    holographic = root.get("holographic") if isinstance(root.get("holographic"), dict) else root
    facts = holographic.get("facts") or []
    facts_by_id = {}
    for fact in facts:
        try:
            facts_by_id[int(fact["fact_id"])] = fact
        except (KeyError, TypeError, ValueError):
            continue

    clusters = _build_curation_clusters(pairs, facts_by_id, max_clusters)
    plan: dict[str, Any] = {
        "dry_run": dry_run,
        "clusters_reviewed": len(clusters),
        "clusters": clusters,
        "ops": [],
        "rejected_ops": [],
        "applied": None,
    }
    if not clusters:
        return JSONResponse(plan)

    user_message = (
        "Review these candidate clusters and return ops as strict JSON.\n\n"
        + json.dumps({"clusters": clusters}, default=str)
    )
    content = _call_curation_llm(
        [
            {"role": "system", "content": _CURATION_SYSTEM_PROMPT},
            {"role": "user", "content": user_message},
        ]
    )
    parsed = _extract_json_object(content)
    if parsed is None or not isinstance(parsed.get("ops"), list):
        raise HTTPException(
            status_code=502,
            detail="curation LLM returned no valid JSON ops",
        )

    allowed_ids = {
        member["fact_id"] for cluster in clusters for member in cluster["members"]
    }
    valid_ops, rejected_ops = _validate_llm_ops(
        parsed["ops"], allowed_ids, min_confidence
    )
    plan["ops"] = valid_ops
    plan["rejected_ops"] = rejected_ops

    if dry_run or not valid_ops:
        return JSONResponse(plan)

    status, apply_result = _post_upstream_json(
        "/api/plugins/holographic/curate/apply",
        {"ops": [_contract_op(op) for op in valid_ops]},
    )
    plan["applied"] = {"status": status, "result": apply_result}
    return JSONResponse(plan, status_code=200 if status < 400 else status)
