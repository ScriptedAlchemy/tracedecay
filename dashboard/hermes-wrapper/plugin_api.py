"""TraceDecay dashboard plugin for Hermes — tracedecay-backed API routes.

Mounted at /api/plugins/tracedecay/ by the Hermes dashboard plugin system.

This is a thin host adapter for the canonical implementation: a local
``tracedecay dashboard`` HTTP server (see the tracedecay repo, ``src/dashboard``).
It does not ship or reimplement any dashboard UI. The adapter:

- lazily spawns ``tracedecay dashboard --port 0`` bound to 127.0.0.1 (or uses
  an externally managed server via ``TRACEDECAY_DASHBOARD_URL``),
- returns the server root from ``/dashboard-url`` so the Hermes entry can mount
  the one embedded dashboard, and
- retains compatibility API forwarding: ``/holographic/*`` -> upstream ``/api/plugins/holographic/*``,
  ``/lcm/*`` -> upstream ``/api/plugins/hermes-lcm/*``,
  ``/graph/*`` -> upstream ``/api/plugins/graph/*``, and
  ``/savings/*`` -> upstream ``/api/plugins/savings/*``,
  ``/analytics/*`` -> upstream ``/api/plugins/analytics/*``,
  ``/automation/*`` -> upstream ``/api/automation/*``,
- exposes upstream ``/api/capabilities`` at ``/capabilities`` so the UI (and
  future Hermes-specific extensions) can feature-detect the backend.

Auth: requests inherit the Hermes dashboard session-token middleware (this
router is mounted under ``/api/plugins/...``); the upstream tracedecay server
only listens on loopback.

Configuration (environment always wins, then deploy-time defaults below):

- ``TRACEDECAY_DASHBOARD_URL``      use an existing server instead of spawning
- ``TRACEDECAY_BIN``                path to the tracedecay binary
- ``TRACEDECAY_DASHBOARD_PROJECT``  project root/store to serve. When unset,
  the wrapper uses the Hermes process cwd. Hermes homes and profiles never
  select a TraceDecay project or store.

Use ``TRACEDECAY_*`` environment variables for runtime configuration.
"""

from __future__ import annotations

import atexit
import concurrent.futures
import ctypes
import json
import logging
import os
import re
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
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse

router = APIRouter()

logger = logging.getLogger(__name__)

# Deploy-time defaults, rewritten in the installed copy by
# `tracedecay install --agent hermes` (src/agents/hermes_dashboard.rs in the
# tracedecay repo): the installer pins only the binary that performed the
# install. TRACEDECAY_BIN is still overridable at runtime.
DEPLOYED_TRACEDECAY_BIN = None

_LISTENING_URL_RE = re.compile(r"https?://[^\s]+")


def _env(name: str) -> str | None:
    """Read TRACEDECAY_<name>."""
    return os.environ.get(f"TRACEDECAY_{name}")

_SPAWN_TIMEOUT_SECONDS = 30.0
_PROXY_TIMEOUT_SECONDS = 30.0
# After the spawned server prints its URL, wait until /api/capabilities
# actually answers before proxying anything: the listener can be bound while
# the engine is still warming up (DB opens, graph load), and proxying into
# that window surfaced as a cold-start 502 "connection reset by peer" on the
# first request.
_READY_TIMEOUT_SECONDS = 30.0
_READY_POLL_INTERVAL_SECONDS = 0.25
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

# PR_SET_PDEATHSIG fires when the *thread* that forked the child exits, not
# the process (prctl(2) warns about exactly this). FastAPI runs sync
# endpoints on anyio threadpool workers that are reaped after ~10s idle, so
# spawning from the request thread used to SIGTERM the child seconds later
# (surfacing as random 502 "connection reset by peer" on the next request).
# All Popen calls therefore run on this single long-lived worker thread,
# which survives until interpreter shutdown — restoring the intended
# "die with the Hermes host process" semantics.
_spawn_pool = concurrent.futures.ThreadPoolExecutor(
    max_workers=1, thread_name_prefix="tracedecay-dashboard-spawn"
)


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


def _find_tracedecay_bin() -> str | None:
    explicit = _env("BIN")
    if explicit and Path(explicit).is_file():
        return explicit
    if DEPLOYED_TRACEDECAY_BIN and Path(DEPLOYED_TRACEDECAY_BIN).is_file():
        return DEPLOYED_TRACEDECAY_BIN
    found = shutil.which("tracedecay")
    if found:
        return found
    # Vendored engine build inside the hermes_intelligence plugin checkout.
    here = Path(__file__).resolve().parent.parent
    for profile in ("release", "debug"):
        for engine_dir, binary_name in (("tracedecay_engine", "tracedecay"),):
            candidate = here / engine_dir / "target" / profile / binary_name
            if candidate.is_file():
                return str(candidate)
    return None


def _project_root() -> str:
    return _env("DASHBOARD_PROJECT") or os.getcwd()


def _dashboard_env() -> dict[str, str]:
    """Inherit the normal process environment for the dashboard child."""
    return os.environ.copy()


def _spawn_dashboard() -> str:
    """Starts ``tracedecay dashboard`` and returns its base URL."""
    binary = _find_tracedecay_bin()
    if not binary:
        raise HTTPException(
            status_code=503,
            detail=(
                "tracedecay binary not found. Install tracedecay or set "
                "TRACEDECAY_BIN / TRACEDECAY_DASHBOARD_URL."
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
    # Spawned on the dedicated long-lived thread so PDEATHSIG binds the
    # child's lifetime to the Hermes process, not a transient request thread.
    process = _spawn_pool.submit(
        subprocess.Popen,
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=_dashboard_env(),
        preexec_fn=_child_preexec if _libc is not None else None,  # noqa: PLW1509 — minimal prctl-only hook
    ).result(timeout=_SPAWN_TIMEOUT_SECONDS)

    # Single reader per pipe, for the child's whole lifetime: the stderr
    # drain keeps a bounded tail for error detail; the stdout reader parses
    # the URL line then KEEPS draining (a stopped reader would eventually
    # block the server on a full pipe buffer and 502 every proxied request).
    stderr_tail: deque = deque(maxlen=_STDERR_TAIL_LINES)
    threading.Thread(
        target=_drain_pipe, args=(process.stderr, stderr_tail), daemon=True
    ).start()

    # The first stdout line is stable: "tracedecay dashboard listening on <url>".
    # Extract the URL itself rather than depending on any surrounding text.
    url_ready = threading.Event()
    url_holder: dict[str, str] = {}

    def _read_stdout() -> None:
        if process.stdout is None:
            url_ready.set()
            return
        for line in process.stdout:
            stripped = line.strip()
            if not url_ready.is_set() and "listening on" in stripped:
                match = _LISTENING_URL_RE.search(stripped)
                if match:
                    url_holder["url"] = match.group(0).rstrip("/")
                    url_ready.set()
        # EOF (process died): unblock the waiter even without a URL.
        url_ready.set()

    threading.Thread(target=_read_stdout, daemon=True).start()
    url_ready.wait(timeout=_SPAWN_TIMEOUT_SECONDS)
    url = url_holder.get("url")

    if url is None:
        _terminate_process(process)
        error_lines = list(stderr_tail)[-5:]
        raise HTTPException(
            status_code=503,
            detail=(
                "tracedecay dashboard failed to start for project "
                f"{project!r}: " + (" / ".join(error_lines) or "no output")
            ),
            headers={"Retry-After": "5"},
        )

    _wait_until_ready(process, url, project, stderr_tail)

    global _process
    _process = process
    logger.info("tracedecay dashboard started at %s (project %s)", url, project)
    return url


def _terminate_process(process: subprocess.Popen) -> None:
    """Terminate-then-kill a spawned child without touching its pipes.

    The drain threads own the pipes — never communicate() here (that would
    race a second reader against them on the same fd).
    """
    try:
        process.terminate()
        process.wait(timeout=5)
    except Exception:
        process.kill()
        try:
            process.wait(timeout=5)  # reap; a kill without wait leaves a zombie
        except Exception:  # pragma: no cover - unkillable child
            pass


def _wait_until_ready(
    process: subprocess.Popen, url: str, project: str, stderr_tail: deque
) -> None:
    """Blocks until the spawned engine answers /api/capabilities.

    Bounded by ``_READY_TIMEOUT_SECONDS``; on timeout (or child death) the
    child is reaped and a 503 with Retry-After is raised so clients know the
    engine is still warming up rather than broken.
    """
    deadline = time.monotonic() + _READY_TIMEOUT_SECONDS
    last_error = "no response"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            error_lines = list(stderr_tail)[-5:]
            raise HTTPException(
                status_code=503,
                detail=(
                    "tracedecay dashboard exited during startup for project "
                    f"{project!r}: " + (" / ".join(error_lines) or "no output")
                ),
                headers={"Retry-After": "5"},
            )
        try:
            request = urllib.request.Request(f"{url}/api/capabilities", method="GET")
            with urllib.request.urlopen(request, timeout=2.0) as response:
                if response.status < 500:
                    return
                last_error = f"HTTP {response.status}"
        except Exception as exc:
            last_error = str(exc)
        time.sleep(_READY_POLL_INTERVAL_SECONDS)
    _terminate_process(process)
    raise HTTPException(
        status_code=503,
        detail=(
            f"tracedecay dashboard for project {project!r} did not become "
            f"ready within {_READY_TIMEOUT_SECONDS:.0f}s: {last_error}"
        ),
        headers={"Retry-After": "5"},
    )


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
    """Returns the base URL of the tracedecay dashboard server, starting it
    on first use unless an external URL is configured.

    Spawn failures are cached for ``_SPAWN_RETRY_BACKOFF_SECONDS`` so a
    persistently failing spawn (e.g. project root not tracedecay-initialized)
    fails fast with a clear 503 instead of serializing every request behind a
    repeated ``_SPAWN_TIMEOUT_SECONDS`` spawn attempt under the module lock.
    """
    configured = _env("DASHBOARD_URL")
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
                        f"tracedecay dashboard spawn failed recently; retrying in "
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
    # Connection-level failures (reset/refused) on GETs are retried once
    # after re-resolving the upstream: _upstream_base reaps a dead child and
    # respawns it (then waits for readiness), so a mid-flight engine death
    # heals transparently instead of surfacing a one-off 502. POSTs are never
    # retried — curation applies must not run twice.
    attempts = 2 if method == "GET" else 1
    last_exc: Exception | None = None
    for attempt in range(attempts):
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
            last_exc = exc
            if attempt + 1 < attempts:
                logger.warning(
                    "tracedecay dashboard proxy request failed (%s); retrying once", exc
                )
                continue
            logger.exception("tracedecay dashboard proxy request failed")
    raise HTTPException(status_code=502, detail=f"tracedecay dashboard unreachable: {last_exc}")


class _DummyRequest:
    """Minimal Request stand-in for proxy calls without an inbound query."""

    class _URL:
        query = ""

    url = _URL()


@router.get("/dashboard-url")
def get_dashboard_url() -> JSONResponse:
    """Return the canonical dashboard root for the Hermes iframe mount."""
    base = _upstream_base()
    return JSONResponse({"url": f"{base}/"})


@router.get("/capabilities")
def get_capabilities() -> JSONResponse:
    """Backend feature discovery (proxied from the tracedecay server).

    Hermes-specific extensions added to this wrapper in later phases should
    merge their own flags into this payload so the UI can feature-detect.
    """
    response = _proxy("GET", "/api/capabilities", _DummyRequest(), None)
    try:
        payload = json.loads(bytes(response.body).decode("utf-8"))
        payload["mode"] = "hermes"
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
    ``similarity``, ``fact/{id}``, ``curation/plan``), preserving the query
    string.
    """
    return _proxy("GET", f"/api/plugins/holographic/{path}", request, None)


@router.post("/holographic/{path:path}")
async def post_holographic(path: str, request: Request) -> JSONResponse:
    """Catch-all POST proxy for the holographic memory API.

    Maps ``/holographic/<path>`` to upstream
    ``POST /api/plugins/holographic/<path>``. Request bodies are forwarded
    unmodified.

    ``_proxy`` blocks (urllib + possible spawn/ready wait), so it runs on the
    threadpool so a slow apply round-trip does not stall the event loop.
    """
    body = await request.body()
    return await run_in_threadpool(
        _proxy, "POST", f"/api/plugins/holographic/{path}", request, body
    )


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
    return await run_in_threadpool(
        _proxy, "POST", f"/api/plugins/hermes-lcm/{path}", request, body
    )


@router.get("/graph/{path:path}")
def get_graph(path: str, request: Request) -> JSONResponse:
    return _proxy("GET", f"/api/plugins/graph/{path}", request, None)


@router.post("/graph/{path:path}")
async def post_graph(path: str, request: Request) -> JSONResponse:
    body = await request.body()
    return await run_in_threadpool(
        _proxy, "POST", f"/api/plugins/graph/{path}", request, body
    )


@router.get("/savings/{path:path}")
def get_savings(path: str, request: Request) -> JSONResponse:
    """Catch-all GET proxy for the savings & cost API.

    Maps ``/savings/<path>`` to upstream ``GET /api/plugins/savings/<path>``
    (e.g. ``overview``, ``ledger``, ``sessions``, ``models``, ``pricing``),
    preserving the query string.
    """
    return _proxy("GET", f"/api/plugins/savings/{path}", request, None)


@router.post("/savings/{path:path}")
async def post_savings(path: str, request: Request) -> JSONResponse:
    """Catch-all POST proxy for the savings & cost API.

    The current savings API is read-only; this exists so future write
    endpoints proxy without changes (mirrors the LCM proxy).
    """
    body = await request.body()
    return await run_in_threadpool(
        _proxy, "POST", f"/api/plugins/savings/{path}", request, body
    )


@router.get("/analytics/{path:path}")
def get_analytics(path: str, request: Request) -> JSONResponse:
    return _proxy("GET", f"/api/plugins/analytics/{path}", request, None)


@router.get("/automation/{path:path}")
def get_automation(path: str, request: Request) -> JSONResponse:
    return _proxy("GET", f"/api/automation/{path}", request, None)


@router.post("/automation/{path:path}")
async def post_automation(path: str, request: Request) -> JSONResponse:
    body = await request.body()
    return await run_in_threadpool(
        _proxy, "POST", f"/api/automation/{path}", request, body
    )
