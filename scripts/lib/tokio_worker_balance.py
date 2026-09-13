#!/usr/bin/env python3
"""Tokio async-worker balance analysis from Hotpath's metrics server.

Answers one question with falsifiable numbers: is the daemon's Tokio worker
pool skewed because a few tasks execute long *synchronous* slices inside
`poll()`, or is it merely idle because the async side has little to do?

Two independent signals are combined, both taken from the already-present
Hotpath instrumentation (no product code changes are required):

* ``GET /tokio_runtime`` -- Tokio's own per-worker counters. ``busy_duration``
  minus ``park`` time is what the worker was *not* parked for;
  ``poll_count`` divides it into per-poll occupancy. Microseconds per poll is
  the discriminator: single-digit microseconds is a healthy async poll, and
  milliseconds means the worker ran synchronous work inside one poll and could
  not be preempted.
* ``GET /futures`` -- Hotpath's labelled-future poll accounting.
  ``total_poll_duration_ns`` is wall time strictly *inside* ``Future::poll``,
  so it excludes every ``.await``. Divided by the sample window it reads as
  "cores of synchronous execution this label pinned to the async pool".

Both are deltas between two samples, never lifetime totals, so a long-lived
daemon and a fresh one are comparable.

This module is deliberately dependency-free (stdlib only) and importable, so
``--self-test`` can exercise the arithmetic over fixtures with no daemon.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Iterable

__all__ = [
    "WorkerDelta",
    "FutureDelta",
    "BalanceReport",
    "fetch_json",
    "worker_deltas",
    "future_deltas",
    "build_report",
    "render_report",
]


@dataclass(frozen=True)
class WorkerDelta:
    """One Tokio worker's counters between two samples."""

    index: int
    busy_ms: int
    polls: int
    steals: int
    parks: int

    @property
    def us_per_poll(self) -> float:
        return (self.busy_ms * 1000.0 / self.polls) if self.polls else 0.0


@dataclass(frozen=True)
class FutureDelta:
    """One labelled future's in-poll execution between two samples."""

    label: str
    source: str
    poll_ns: int
    polls: int

    @property
    def us_per_poll(self) -> float:
        return (self.poll_ns / 1000.0 / self.polls) if self.polls else 0.0

    def cores(self, window_secs: float) -> float:
        return (self.poll_ns / 1e9 / window_secs) if window_secs > 0 else 0.0


@dataclass
class BalanceReport:
    window_secs: float
    workers: list[WorkerDelta] = field(default_factory=list)
    futures: list[FutureDelta] = field(default_factory=list)
    num_workers: int = 0
    blocking_threads: int | None = None
    idle_blocking_threads: int | None = None

    @property
    def total_busy_ms(self) -> int:
        return sum(w.busy_ms for w in self.workers)

    @property
    def busy_cores(self) -> float:
        if self.window_secs <= 0:
            return 0.0
        return self.total_busy_ms / 1000.0 / self.window_secs

    def busy_share(self, worker: WorkerDelta) -> float:
        total = self.total_busy_ms
        return (100.0 * worker.busy_ms / total) if total else 0.0

    def top_share(self, count: int) -> float:
        total = self.total_busy_ms
        if not total:
            return 0.0
        ranked = sorted(self.workers, key=lambda w: -w.busy_ms)[:count]
        return 100.0 * sum(w.busy_ms for w in ranked) / total

    @property
    def active_workers(self) -> int:
        return sum(1 for w in self.workers if w.busy_ms > 0)

    @property
    def total_steals(self) -> int:
        return sum(w.steals for w in self.workers)

    def worst_us_per_poll(self) -> float:
        """Highest per-poll occupancy among workers that did real work.

        Workers with a handful of polls are excluded: one slow poll on an
        otherwise idle worker is noise, not a funnel.
        """
        candidates = [w for w in self.workers if w.polls >= 100]
        return max((w.us_per_poll for w in candidates), default=0.0)


def fetch_json(host: str, port: int, path: str, timeout: float = 5.0) -> Any:
    """GET one Hotpath metrics route. Returns None when it is not served."""
    url = f"http://{host}:{port}{path}"
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            return json.loads(response.read().decode())
    except (urllib.error.URLError, urllib.error.HTTPError, OSError, ValueError):
        return None


def _rows(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, dict):
        payload = payload.get("data", [])
    if not isinstance(payload, list):
        return []
    return [row for row in payload if isinstance(row, dict)]


def worker_deltas(before: Any, after: Any) -> list[WorkerDelta]:
    """Per-worker counter deltas between two ``/tokio_runtime`` snapshots."""
    if not isinstance(before, dict) or not isinstance(after, dict):
        return []
    first = {w["index"]: w for w in _rows(before.get("workers"))}
    deltas = []
    for row in _rows(after.get("workers")):
        prior = first.get(row["index"])
        if prior is None:
            continue
        deltas.append(
            WorkerDelta(
                index=row["index"],
                busy_ms=row["busy_duration_ms"] - prior["busy_duration_ms"],
                polls=(row.get("poll_count") or 0) - (prior.get("poll_count") or 0),
                steals=(row.get("steal_count") or 0) - (prior.get("steal_count") or 0),
                parks=row["park_count"] - prior["park_count"],
            )
        )
    deltas.sort(key=lambda w: -w.busy_ms)
    return deltas


def future_deltas(before: Any, after: Any) -> list[FutureDelta]:
    """Per-label in-poll deltas between two ``/futures`` snapshots."""
    first = {row["id"]: row for row in _rows(before) if "id" in row}
    deltas = []
    for row in _rows(after):
        prior = first.get(row.get("id"))
        if prior is None:
            continue
        deltas.append(
            FutureDelta(
                label=row.get("label") or row.get("source", "?"),
                source=row.get("source", "?"),
                poll_ns=row["total_poll_duration_ns"] - prior["total_poll_duration_ns"],
                polls=row["total_polls"] - prior["total_polls"],
            )
        )
    deltas.sort(key=lambda f: -f.poll_ns)
    return deltas


def build_report(
    window_secs: float,
    runtime_before: Any,
    runtime_after: Any,
    futures_before: Any,
    futures_after: Any,
) -> BalanceReport:
    after = runtime_after if isinstance(runtime_after, dict) else {}
    return BalanceReport(
        window_secs=window_secs,
        workers=worker_deltas(runtime_before, runtime_after),
        futures=future_deltas(futures_before, futures_after),
        num_workers=after.get("num_workers", 0),
        blocking_threads=after.get("num_blocking_threads"),
        idle_blocking_threads=after.get("num_idle_blocking_threads"),
    )


def render_report(report: BalanceReport, future_limit: int = 8) -> str:
    out: list[str] = []
    out.append(
        f"window {report.window_secs:.1f}s   workers {report.num_workers}   "
        f"blocking threads {report.blocking_threads} "
        f"({report.idle_blocking_threads} idle)"
    )
    out.append(
        f"async pool busy {report.total_busy_ms / 1000.0:.2f}s "
        f"= {report.busy_cores:.2f} cores   steals {report.total_steals}"
    )
    out.append("")
    out.append(
        f"{'wrk':>4} {'busy_ms':>10} {'share%':>8} {'polls':>10} "
        f"{'us/poll':>10} {'steals':>8} {'parks':>9}"
    )
    for worker in report.workers:
        out.append(
            f"{worker.index:>4} {worker.busy_ms:>10} "
            f"{report.busy_share(worker):>8.2f} {worker.polls:>10} "
            f"{worker.us_per_poll:>10.1f} {worker.steals:>8} {worker.parks:>9}"
        )
    out.append("")
    out.append(f"top-1 busy share  {report.top_share(1):.2f}%")
    out.append(f"top-2 busy share  {report.top_share(2):.2f}%")
    out.append(f"workers with any busy time  {report.active_workers}/{len(report.workers)}")
    out.append(f"worst us/poll (>=100 polls) {report.worst_us_per_poll():.1f}")
    out.append("")
    out.append("in-poll execution by labelled future (excludes every .await):")
    out.append(f"{'cores':>8} {'poll_s':>9} {'polls':>9} {'us/poll':>10}  label")
    for entry in report.futures[:future_limit]:
        if entry.poll_ns <= 0:
            continue
        out.append(
            f"{entry.cores(report.window_secs):>8.2f} {entry.poll_ns / 1e9:>9.2f} "
            f"{entry.polls:>9} {entry.us_per_poll:>10.1f}  {entry.label}"
        )
    return "\n".join(out)


def check_thresholds(
    report: BalanceReport,
    max_top2_share: float | None,
    max_us_per_poll: float | None,
) -> list[str]:
    """Return one message per breached threshold; empty means the gate passed."""
    failures: list[str] = []
    if max_top2_share is not None:
        share = report.top_share(2)
        if share > max_top2_share:
            failures.append(
                f"top-2 worker busy share {share:.2f}% exceeds {max_top2_share:.2f}%"
            )
    if max_us_per_poll is not None:
        worst = report.worst_us_per_poll()
        if worst > max_us_per_poll:
            failures.append(
                f"worst worker occupancy {worst:.1f} us/poll exceeds "
                f"{max_us_per_poll:.1f} us/poll"
            )
    return failures


# --------------------------------------------------------------------------
# Self-test: exercise the arithmetic over fixtures, with no daemon involved.
# --------------------------------------------------------------------------


def _runtime_fixture(busy: Iterable[int], polls: Iterable[int]) -> dict[str, Any]:
    workers = [
        {
            "index": index,
            "park_count": 10 * index,
            "busy_duration_ms": busy_ms,
            "poll_count": poll_count,
            "steal_count": 0,
        }
        for index, (busy_ms, poll_count) in enumerate(zip(busy, polls))
    ]
    return {
        "num_workers": len(workers),
        "num_blocking_threads": 6,
        "num_idle_blocking_threads": 6,
        "workers": workers,
    }


def self_test() -> int:
    failures: list[str] = []

    def check(name: str, condition: bool) -> None:
        if not condition:
            failures.append(name)

    zero = _runtime_fixture([0, 0, 0, 0], [0, 0, 0, 0])
    skewed = _runtime_fixture([9000, 900, 50, 0], [9000, 900, 50, 0])
    report = build_report(10.0, zero, skewed, [], [])

    check("four worker deltas", len(report.workers) == 4)
    check("busy total", report.total_busy_ms == 9950)
    check("busy cores", abs(report.busy_cores - 0.995) < 1e-6)
    check("top1 share", abs(report.top_share(1) - 90.452) < 0.01)
    check("top2 share", abs(report.top_share(2) - 99.497) < 0.01)
    check("active workers", report.active_workers == 3)
    # 9000 ms over 9000 polls is exactly 1000 us per poll.
    check("us per poll", abs(report.workers[0].us_per_poll - 1000.0) < 1e-6)

    # A worker with very few polls must not set the funnel verdict.
    noisy = _runtime_fixture([5, 0, 0, 0], [1, 0, 0, 0])
    noisy_report = build_report(10.0, zero, noisy, [], [])
    check("low-poll worker ignored", noisy_report.worst_us_per_poll() == 0.0)

    before = [
        {"id": 1, "label": "a", "source": "s1", "total_polls": 0, "total_poll_duration_ns": 0},
        {"id": 2, "label": "b", "source": "s2", "total_polls": 0, "total_poll_duration_ns": 0},
    ]
    after = [
        {
            "id": 1,
            "label": "a",
            "source": "s1",
            "total_polls": 1000,
            "total_poll_duration_ns": 2_000_000_000,
        },
        {
            "id": 2,
            "label": "b",
            "source": "s2",
            "total_polls": 1000,
            "total_poll_duration_ns": 1_000_000,
        },
    ]
    futures = future_deltas(before, after)
    check("futures ranked by poll time", [f.label for f in futures] == ["a", "b"])
    check("future cores", abs(futures[0].cores(10.0) - 0.2) < 1e-9)
    check("future us per poll", abs(futures[0].us_per_poll - 2000.0) < 1e-6)

    # Ids absent from the earlier snapshot cannot produce a delta.
    check("unmatched ids dropped", future_deltas([], after) == [])
    # Envelope form ({"data": [...]}) must parse the same as a bare list.
    check("envelope form", len(future_deltas({"data": before}, {"data": after})) == 2)

    check(
        "threshold gate fails on skew",
        check_thresholds(report, max_top2_share=60.0, max_us_per_poll=None) != [],
    )
    check(
        "threshold gate passes when balanced",
        check_thresholds(report, max_top2_share=100.0, max_us_per_poll=None) == [],
    )
    check(
        "threshold gate fails on slow polls",
        check_thresholds(report, max_top2_share=None, max_us_per_poll=100.0) != [],
    )

    # A report with no samples must be inert rather than divide by zero.
    empty = build_report(0.0, None, None, None, None)
    check("empty report is inert", empty.busy_cores == 0.0 and empty.top_share(2) == 0.0)
    check("empty renders", "window 0.0s" in render_report(empty))

    if failures:
        for name in failures:
            print(f"FAIL {name}")
        return 1
    print("tokio_worker_balance self-test: ok")
    return 0
