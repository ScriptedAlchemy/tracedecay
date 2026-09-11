import { Fragment, useEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent, type PointerEvent } from "react";
import { Corners } from "../app/shell/Corners";
import { useDemo, useWorkspaceState } from "../app/workspace";
import { PACK, shortId, type PackSession } from "../data/pack";
import { loomPivotUrl } from "../loom/pivots";
import {
  ALL_SESSIONS,
  COLUMNS,
  FIRST_AT,
  FIRST_TS,
  LAST_AT,
  LAST_TS,
  PAGE_SIZE,
  PARENT_IDS,
  PROJECTS,
  PROVIDERS,
  bucketLevel,
  clampWindow,
  fmtDay,
  fmtDayYear,
  inWindow,
  isTimeWindow,
  logAxisLabels,
  logFrac,
  sortSessions,
  spikeColor,
  stripUtc,
  volumeInWindow,
  windowFor,
  zoomWindow,
  type SpikeTick,
  type TimeWindow,
  type ColId,
  type RangeId,
  type SortDir,
} from "./model";
import "./sessions.css";

type Filters = {
  provider: string | "all";
  project: string | "all";
  sub: "all" | "root" | "sub";
};

const DEFAULT_COLS: Record<ColId, boolean> = {
  id: true,
  provider: true,
  project: true,
  span: true,
  messages: true,
  tokens: true,
  coverage: true,
  status: true,
};

function loomSessionUrl(sessionId: string) {
  const url = new URL(location.href);
  ["loom_pivot", "loom_target_session", "loom_target_event", "loom_return"].forEach((key) => url.searchParams.delete(key));
  url.searchParams.set("surface", "loom");
  url.searchParams.delete("state");
  url.searchParams.set("loom_source", "mac");
  url.searchParams.set("loom_session", sessionId);
  url.searchParams.set("loom_follow", "0");
  return url.pathname + url.search;
}

function SearchIco() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="7" cy="7" r="4.2" fill="none" stroke="currentColor" strokeWidth="1.25" />
      <path d="M10.2 10.2 13.4 13.4" fill="none" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
    </svg>
  );
}

function CalIco() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <rect x="2.5" y="3.5" width="11" height="10" rx="1" fill="none" stroke="currentColor" strokeWidth="1.15" />
      <path d="M2.5 6.5h11M5.5 2.5v2M10.5 2.5v2" fill="none" stroke="currentColor" strokeWidth="1.15" strokeLinecap="round" />
    </svg>
  );
}

function DotsIco() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="3.5" r="1.1" fill="currentColor" />
      <circle cx="8" cy="8" r="1.1" fill="currentColor" />
      <circle cx="8" cy="12.5" r="1.1" fill="currentColor" />
    </svg>
  );
}

function FilterIco() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M3 4.5h10M5 8h6M6.5 11.5h3" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
    </svg>
  );
}

function ColIco() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <rect x="2.5" y="3.5" width="3.2" height="9" fill="none" stroke="currentColor" strokeWidth="1.1" />
      <rect x="6.4" y="3.5" width="3.2" height="9" fill="none" stroke="currentColor" strokeWidth="1.1" />
      <rect x="10.3" y="3.5" width="3.2" height="9" fill="none" stroke="currentColor" strokeWidth="1.1" />
    </svg>
  );
}


function SpanTimes(props: { start: string | null; end: string | null }) {
  return (
    <>
      <b>{stripUtc(props.start)}</b>
      <i> → {stripUtc(props.end)}</i>
    </>
  );
}

function SortGlyph(props: { dir: SortDir | null }) {
  return (
    <svg className="sn-sort" viewBox="0 0 8 12" aria-hidden="true">
      <path d="M4 1 7 4.6 1 4.6Z" fill="currentColor" opacity={props.dir === "asc" ? 1 : 0.38} />
      <path d="M4 11 1 7.4 7 7.4Z" fill="currentColor" opacity={props.dir === "desc" ? 1 : 0.38} />
    </svg>
  );
}

function VolumeField(props: {
  sessions: PackSession[];
  camera: TimeWindow;
  selectedId: string | null;
  onPick: (id: string) => void;
  onCamera: (camera: TimeWindow, remember: boolean) => void;
}) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const drag = useRef<{ x: number; camera: TimeWindow; moved: boolean } | null>(null);
  const lastWheel = useRef(0);
  const activeIds = useMemo(() => new Set(props.sessions.map((s) => s.id)), [props.sessions]);
  const level = bucketLevel(props.camera.t1 - props.camera.t0);
  const ticks = useMemo(
    () => volumeInWindow(ALL_SESSIONS, props.camera.t0, props.camera.t1, level.seconds),
    [props.camera.t0, props.camera.t1, level.seconds],
  );
  const selTicks = useMemo(
    () => (props.selectedId ? ticks.filter((t) => t.id === props.selectedId) : []),
    [ticks, props.selectedId],
  );
  const [focus, setFocus] = useState(0);
  const focused = ticks[Math.min(focus, Math.max(0, ticks.length - 1))] ?? null;
  const yMax = Math.max(1, ...ticks.map((t) => t.n));
  const span = Math.max(1, props.camera.t1 - props.camera.t0);

  useEffect(() => {
    const el = wrap.current;
    const cv = canvas.current;
    if (!el || !cv) return;
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const draw = () => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      cv.width = Math.max(1, Math.floor(w * dpr));
      cv.height = Math.max(1, Math.floor(h * dpr));
      cv.style.width = `${w}px`;
      cv.style.height = `${h}px`;
      const ctx = cv.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      ctx.lineCap = "butt";

      const rgba = (col: string, a: number) =>
        col.replace("rgb(", "rgba(").replace(")", `,${a})`);
      const baseline = h - 1;
      const maxH = h - 10;

      // Baseline haze under real activity, plate atmosphere.
      for (const tick of ticks) {
        const u = (tick.ts - props.camera.t0) / span;
        const col = spikeColor(tick.provider, u);
        const halo = ctx.createRadialGradient(u * w, baseline, 0, u * w, baseline, 7);
        halo.addColorStop(0, rgba(col, 0.06));
        halo.addColorStop(1, rgba(col, 0));
        ctx.fillStyle = halo;
        ctx.beginPath();
        ctx.arc(u * w, baseline, 7, 0, Math.PI * 2);
        ctx.fill();
      }

      // One needle per session per semantic bucket, scaled to the visible max.
      // Buckets without real messages stay unpainted — never invented.
      const needle = (tick: SpikeTick, on: boolean) => {
        const active = activeIds.has(tick.id);
        const u = (tick.ts - props.camera.t0) / span;
        const x = u * w;
        const hh = Math.max(10, logFrac(tick.n, yMax) * maxH);
        const tipY = baseline - hh;
        const col = spikeColor(tick.provider, u);

        const stem = ctx.createLinearGradient(0, baseline, 0, tipY);
        stem.addColorStop(0, rgba(col, active ? (on ? 0.34 : 0.14) : 0.025));
        stem.addColorStop(1, rgba(col, active ? (on ? 1 : 0.82) : 0.14));
        ctx.strokeStyle = stem;
        ctx.lineWidth = on ? 1.6 : 1;
        ctx.beginPath();
        ctx.moveTo(x, baseline);
        ctx.lineTo(x, tipY);
        ctx.stroke();

        // Faint trail dots along tall stems, plate species.
        if (hh > maxH * 0.34) {
          ctx.fillStyle = rgba(col, active ? (on ? 0.75 : 0.45) : 0.08);
          for (const f of [0.3, 0.55, 0.8]) {
            ctx.beginPath();
            ctx.arc(x, tipY + hh * f, on ? 1 : 0.7, 0, Math.PI * 2);
            ctx.fill();
          }
        }

        if (!reduce) {
          ctx.shadowColor = col;
          ctx.shadowBlur = on ? 10 : 6;
        }
        ctx.fillStyle = rgba(col, active ? (on ? 1 : 0.95) : 0.18);
        ctx.beginPath();
        ctx.arc(x, tipY, on ? 1.8 : 1.3, 0, Math.PI * 2);
        ctx.fill();
        ctx.shadowBlur = 0;
      };

      for (const tick of ticks) needle(tick, false);
      for (const tick of selTicks) needle(tick, true);
      ctx.globalAlpha = 1;
    };
    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(el);
    return () => ro.disconnect();
  }, [activeIds, ticks, selTicks, props.camera.t0, span, yMax]);

  function nearest(clientX: number) {
    const el = wrap.current;
    if (!el) return -1;
    const rect = el.getBoundingClientRect();
    const x = clientX - rect.left;
    let best = -1;
    let bestD = Infinity;
    for (let i = 0; i < ticks.length; i++) {
      const sx = ((ticks[i].ts - props.camera.t0) / span) * rect.width;
      const d = Math.abs(sx - x);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    }
    return bestD < 18 ? best : -1;
  }

  function onPointerDown(e: PointerEvent<HTMLCanvasElement>) {
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, camera: props.camera, moved: false };
  }

  function onPointerMove(e: PointerEvent<HTMLCanvasElement>) {
    if (!drag.current) {
      const index = nearest(e.clientX);
      if (index >= 0) setFocus(index);
      return;
    }
    const el = wrap.current;
    if (!el) return;
    const dx = e.clientX - drag.current.x;
    const delta = (-dx / el.clientWidth) * (drag.current.camera.t1 - drag.current.camera.t0);
    props.onCamera(clampWindow({ t0: drag.current.camera.t0 + delta, t1: drag.current.camera.t1 + delta }), !drag.current.moved);
    drag.current.moved = true;
  }

  function onPointerUp(e: PointerEvent<HTMLCanvasElement>) {
    const current = drag.current;
    drag.current = null;
    e.currentTarget.releasePointerCapture(e.pointerId);
    if (!current?.moved) {
      const index = nearest(e.clientX);
      if (index >= 0) {
        setFocus(index);
        props.onPick(ticks[index].id);
      }
    }
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const rect = canvas.current?.getBoundingClientRect();
    if (!rect) return;
    const anchor = props.camera.t0 + ((e.clientX - rect.left) / rect.width) * span;
    const now = performance.now();
    props.onCamera(zoomWindow(props.camera, anchor, e.deltaY > 0 ? 1.25 : 0.8), now - lastWheel.current > 250);
    lastWheel.current = now;
  }

  useEffect(() => {
    const element = canvas.current;
    if (!element) return;
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  });

  function onKeyDown(e: KeyboardEvent<HTMLCanvasElement>) {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      setFocus((i) => Math.max(0, Math.min(ticks.length - 1, i + (e.key === "ArrowLeft" ? -1 : 1))));
    } else if (e.key === "+" || e.key === "=") {
      e.preventDefault();
      props.onCamera(zoomWindow(props.camera, (props.camera.t0 + props.camera.t1) / 2, 0.8), true);
    } else if (e.key === "-") {
      e.preventDefault();
      props.onCamera(zoomWindow(props.camera, (props.camera.t0 + props.camera.t1) / 2, 1.25), true);
    } else if ((e.key === "Enter" || e.key === " ") && focused) {
      e.preventDefault();
      props.onPick(focused.id);
    }
  }

  const axis = useMemo(() => {
    const n = 9;
    return Array.from({ length: n }, (_, i) => {
      const t = props.camera.t0 + (span * i) / (n - 1);
      const d = new Date(t * 1000);
      return { t, lab: level.label === "DAY" ? fmtDay(t) : `${String(d.getUTCHours()).padStart(2, "0")}:${String(d.getUTCMinutes()).padStart(2, "0")}` };
    });
  }, [level.label, props.camera.t0, span]);

  const yLabs = logAxisLabels(yMax, 7);
  const focusLabel = focused
    ? `${new Date(focused.bucketStart * 1000).toISOString()}${focused.bucketEnd === focused.bucketStart ? "" : ` → ${new Date(focused.bucketEnd * 1000).toISOString()}`} · ${focused.n.toLocaleString()} messages · ${focused.provider} · ${shortId(focused.id, 14)}${activeIds.has(focused.id) ? "" : " · dimmed by filter"}`
    : "No recorded bucket in viewport";

  return (
    <div className="sn-chart">
      <div className="sn-y" aria-hidden="true">
        {yLabs.map((l, i) => (
          <span key={`${l}-${i}`}>{l}</span>
        ))}
      </div>
      <div className="sn-field" ref={wrap}>
        <output className="sn-bucket-focus" aria-live="polite">
          {focusLabel}
        </output>
        <canvas
          ref={canvas}
          tabIndex={0}
          role="application"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onKeyDown={onKeyDown}
          aria-label={`Message volume ${level.label.toLowerCase()} buckets. Arrow keys focus buckets, plus and minus zoom, Enter selects.`}
        />
      </div>
      <div className="sn-x" aria-hidden="true">
        {axis.map((a) => (
          <span key={a.t}>{a.lab}</span>
        ))}
      </div>
    </div>
  );
}

function OverviewBrush(props: {
  camera: TimeWindow;
  sessions: PackSession[];
  onCamera: (camera: TimeWindow, remember: boolean) => void;
}) {
  const track = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);
  const ticks = useMemo(() => volumeInWindow(ALL_SESSIONS, FIRST_TS, LAST_TS, 86400), []);
  const yMax = Math.max(1, ...ticks.map((tick) => tick.n));
  const active = useMemo(() => new Set(props.sessions.map((s) => s.id)), [props.sessions]);
  const bounds = LAST_TS - FIRST_TS;
  const left = ((props.camera.t0 - FIRST_TS) / bounds) * 100;
  const width = ((props.camera.t1 - props.camera.t0) / bounds) * 100;

  function move(clientX: number, remember: boolean) {
    const rect = track.current?.getBoundingClientRect();
    if (!rect) return;
    const center = FIRST_TS + Math.max(0, Math.min(1, (clientX - rect.left) / rect.width)) * bounds;
    const span = props.camera.t1 - props.camera.t0;
    props.onCamera(clampWindow({ t0: center - span / 2, t1: center + span / 2 }), remember);
  }

  return (
    <div className="sn-overview">
      <span>OVERVIEW / BRUSH</span>
      <div
        ref={track}
        className="sn-overview-track"
        role="slider"
        tabIndex={0}
        aria-label="Timeline viewport"
        aria-valuemin={FIRST_TS}
        aria-valuemax={LAST_TS}
        aria-valuenow={(props.camera.t0 + props.camera.t1) / 2}
        onPointerDown={(e) => {
          dragging.current = true;
          e.currentTarget.setPointerCapture(e.pointerId);
          move(e.clientX, true);
        }}
        onPointerMove={(e) => dragging.current && move(e.clientX, false)}
        onPointerUp={(e) => {
          dragging.current = false;
          e.currentTarget.releasePointerCapture(e.pointerId);
        }}
        onKeyDown={(e) => {
          const span = props.camera.t1 - props.camera.t0;
          if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
            e.preventDefault();
            props.onCamera(clampWindow({ t0: props.camera.t0 + span * (e.key === "ArrowLeft" ? -0.1 : 0.1), t1: props.camera.t1 + span * (e.key === "ArrowLeft" ? -0.1 : 0.1) }), true);
          }
        }}
      >
        {ticks.map((tick, i) => (
          <i
            key={`${tick.id}-${tick.ts}-${i}`}
            style={{
              left: `${((tick.ts - FIRST_TS) / bounds) * 100}%`,
              height: `${Math.max(12, logFrac(tick.n, yMax) * 100)}%`,
              background: spikeColor(tick.provider, (tick.ts - FIRST_TS) / bounds),
              opacity: active.has(tick.id) ? 0.8 : 0.12,
            }}
          />
        ))}
        <b className="sn-brush" style={{ left: `${left}%`, width: `${width}%` }} />
      </div>
      <span>{fmtDay(FIRST_TS)} → {fmtDay(LAST_TS)}</span>
    </div>
  );
}

const PROV_SWATCH: Record<string, string> = {
  Complete: "var(--signal-cyan)",
  Partial: "#e0a33d",
  Unknown: "#68788a",
  "Served Empty": "#8f7fd8",
  Unavailable: "var(--activity-amber)",
  "Transport Failure": "#d96a55",
};

function Prov(props: { lab: string; n: number; total: number; kind?: "live" | "total" | "mute" }) {
  // Grades that were never served are typed absence (—), not a 0.0% of a
  // scope, and an empty scope has no denominator to fabricate 100% from.
  const hasScope = props.total > 0;
  const total = props.kind === "total";
  const absent = !hasScope || (!total && props.n === 0);
  const pct = hasScope ? (props.n / props.total) * 100 : 0;
  return (
    <div className={`sn-prov${props.kind === "live" ? " is-live" : total ? " is-total" : ""}`}>
      {total ? (
        <i className="sw is-blank" aria-hidden="true" />
      ) : (
        <i
          className="sw"
          aria-hidden="true"
          style={{ background: PROV_SWATCH[props.lab] ?? "#68788a", opacity: props.n ? 1 : 0.3 }}
        />
      )}
      <span className="lab">{props.lab}</span>
      <span className={absent ? "n is-absent" : "n"}>{absent ? "—" : props.n.toLocaleString()}</span>
      <span className={absent ? "pct is-absent" : "pct"}>
        {absent ? "—" : total ? "100%" : `${pct.toFixed(1)}%`}
      </span>
    </div>
  );
}

function Inspector(props: {
  scoped: PackSession[];
  page: number;
  pages: number;
  selected: PackSession | null;
  fixture: boolean;
  onClose: () => void;
  onNavigate: (surface: string, params: Record<string, string>) => void;
}) {
  const n = props.scoped.length;
  const first = props.scoped.length
    ? props.scoped.reduce((a, s) => ((s.startedTs ?? Infinity) < (a.startedTs ?? Infinity) ? s : a))
    : null;
  const last = props.scoped.length
    ? props.scoped.reduce((a, s) => ((s.endedTs ?? s.startedTs ?? 0) > (a.endedTs ?? a.startedTs ?? 0) ? s : a))
    : null;

  if (props.selected) {
    const s = props.selected;
    const route = (event: MouseEvent<HTMLAnchorElement>, surface: string, href: string) => {
      if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      const params = new URL(href, location.origin).searchParams;
      params.delete("surface");
      props.onNavigate(surface, Object.fromEntries(params));
    };
    return (
      <aside className="sn-inspect" aria-label="Selected session provenance inspector">
        <Corners />
        <h2>
          <span>SESSION DETAIL</span>
          <button type="button" className="sn-inspect-close" onClick={props.onClose} aria-label="Close session detail">
            ×
          </button>
        </h2>
        <div className="sn-block sn-sect sn-detail-first">
          <div className="k">PERSISTED IDENTITY · EXACT</div>
          <div className="sn-kv sn-detail-id">
            <span>Session ID</span>
            <b>{s.id}</b>
          </div>
          <div className="sn-kv">
            <span>Provider</span>
            <b>{s.provider}</b>
          </div>
          <div className="sn-kv">
            <span>Project</span>
            <b>{s.project}</b>
          </div>
          <div className="sn-kv">
            <span>Source path</span>
            <b>{s.path ?? "unavailable — not recorded"}</b>
          </div>
          <div className="sn-kv">
            <span>Loaded source</span>
            <b>{props.fixture ? "synthetic session spine · fixture" : "mac profile export · exact"}</b>
          </div>
        </div>
        <div className="sn-block sn-sect">
          <div className="k">RECORDED SPAN · EXACT</div>
          <div className="sn-kv">
            <span>Started</span>
            <b>{s.startedAt ?? "unavailable"}</b>
          </div>
          <div className="sn-kv">
            <span>Ended</span>
            <b>{s.endedAt ?? "unavailable"}</b>
          </div>
          <div className="sn-row">
            <span>Messages</span>
            <span className="r">{s.messages.toLocaleString()}</span>
          </div>
          <div className="sn-row">
            <span>Lifecycle</span>
            <span className="r is-amber">unavailable — not recorded</span>
          </div>
          <div className="sn-row">
            <span>Source status</span>
            <span className="r is-amber">{s.status}</span>
          </div>
        </div>
        <div className="sn-block sn-sect">
          <div className="k">RELATIONSHIPS</div>
          <div className="sn-kv">
            <span>Parent session · EXACT</span>
            <b>{s.parentId ?? "none"}</b>
          </div>
          <div className="sn-kv">
            <span>Agent · EXACT</span>
            <b>{s.agentId ?? "unavailable — not recorded"}</b>
          </div>
          <div className="sn-kv">
            <span>Branch / worktree / commit / task</span>
            <b className="is-amber">unavailable — not present in spine index</b>
          </div>
          <nav className="sn-pivots" aria-label="Selected session destinations">
            <a href={loomSessionUrl(s.id)} onClick={(e) => route(e, "loom", loomSessionUrl(s.id))}>LOOM ↗</a>
            {(["agents", "work", "code", "delivery"] as const).map((surface) => {
              const href = loomPivotUrl(surface, s.id);
              return <a key={surface} href={href} onClick={(e) => route(e, surface, href)}>{surface.toUpperCase()} ↗</a>;
            })}
          </nav>
        </div>
        <div className="sn-block sn-sect">
          <div className="k">CONTENT AUTHORITY</div>
          <div className="sn-row">
            <span>Coverage</span>
            <span className="r">spine-only</span>
          </div>
          <div className="sn-row">
            <span>Tokens</span>
            <span className="r is-amber">unavailable</span>
          </div>
          <div className="sn-row">
            <span>Transcript</span>
            <span className="r is-amber">bodies not copied</span>
          </div>
          <p className="sn-copy sn-detail-note">
            Private chain-of-thought is unavailable by design. Only persisted visible artifacts may appear here.
          </p>
        </div>
        <div className="sn-block sn-sect sn-source-matrix">
          <div className="k">SOURCE AVAILABILITY MATRIX</div>
          <div className="sn-row"><span>Transcript</span><span className="r is-amber">unavailable · bodies not ingested</span></div>
          <div className="sn-row"><span>Pagination</span><span className="r">loaded page {props.page + 1} of {props.pages}</span></div>
          <div className="sn-row"><span>Redaction</span><span className="r">not reported by source</span></div>
          <div className="sn-row"><span>Links</span><span className="r is-amber">spine relationship index unavailable</span></div>
          <details className="sn-transcript-fallback" open>
            <summary>EXACT EVENT FALLBACK</summary>
            <p>Transcript and event bodies were not ingested for this session. Identity and recorded span remain exact; no transcript is reconstructed.</p>
          </details>
        </div>
      </aside>
    );
  }

  return (
    <aside className="sn-inspect" aria-label="Session provenance inspector">
      <Corners />
      <h2>
        LCM OVERVIEW
        <span className="sn-ready">READY</span>
      </h2>
      <div className="sn-block sn-stack">
        <div className="sn-kv">
          <span>Temporal Scope</span>
          <b>
            {first && last ? `${fmtDayYear(first.startedTs ?? FIRST_TS)} → ${fmtDayYear(last.endedTs ?? last.startedTs ?? LAST_TS)}` : "—"}
          </b>
        </div>
        <div className="sn-kv">
          <span>Sessions In Scope</span>
          <b>{n.toLocaleString()}</b>
        </div>
        <div className="sn-kv">
          <span>First Seen</span>
          <b>{first?.startedAt ?? FIRST_AT}</b>
        </div>
        <div className="sn-kv">
          <span>Last Seen</span>
          <b>{last?.endedAt ?? last?.startedAt ?? LAST_AT}</b>
        </div>
      </div>
      <div className="sn-block sn-pageblock">
        <div className="sn-row">
          <span>Page Status</span>
          <span className="r is-cyan">LOADED</span>
        </div>
        <div className="sn-row">
          <span>Page</span>
          <span className="r">
            {props.page + 1} of {props.pages}
          </span>
        </div>
        <div className="sn-row">
          <span>Page Size</span>
          <span className="r">{PAGE_SIZE}</span>
        </div>
        <div className="sn-kv sn-cursor">
          <span>Next Cursor</span>
          <b className="is-amber">unavailable — not served</b>
        </div>
      </div>
      <div className="sn-block sn-sect">
        <div className="k">TOKEN PROVENANCE</div>
        <Prov lab="Complete" n={0} total={n} />
        <Prov lab="Partial" n={0} total={n} />
        <Prov lab="Unknown" n={0} total={n} />
        <Prov lab="Served Empty" n={0} total={n} />
        <Prov lab="Unavailable" n={n} total={n} kind="live" />
        <Prov lab="Transport Failure" n={0} total={n} />
        <Prov lab="Total" n={n} total={n} kind="total" />
        <p className="sn-copy">Token counts were not copied. Every row in this snapshot is unavailable — complete/partial grades were not served.</p>
      </div>
      <div className="sn-block sn-sect">
        <div className="k">TRANSCRIPT SEARCH</div>
        <p className="sn-copy">
          Full-text search across session transcripts is unavailable. Message bodies and FTS were not copied into this snapshot. Identity search (id / provider / project) is the table filter, separate from this control.
        </p>
        <input className="sn-fts" disabled placeholder="Transcript FTS unavailable" aria-label="Transcript search unavailable" />
      </div>
      <div className="sn-hint">
        Private chain-of-thought is unavailable by design. Spine counts are not transcript text. Tokens were not copied.
      </div>
    </aside>
  );
}

export function SessionsPage(props: { initialSessionId?: string; onInspect?: unknown; state?: string; onState?: (id: string) => void } = {}) {
  const { navigate, mode } = useDemo();
  const initialSessionId = ALL_SESSIONS.find(s => s.id === props.initialSessionId)?.id ?? null;
  const arrival = useMemo(() => {
    if (initialSessionId) return { sessionId: initialSessionId, issue: null };
    const params = new URLSearchParams(location.search);
    const sessionParam = params.get("session")?.trim() ?? "";
    const eventParam = params.get("event")?.trim() ?? "";
    if (sessionParam) {
      const session = ALL_SESSIONS.find((candidate) => candidate.id === sessionParam);
      return session
        ? { sessionId: session.id, issue: null }
        : { sessionId: null, issue: `Session ${sessionParam} is unavailable in this recorded snapshot.` };
    }
    if (eventParam) {
      const event = PACK.loomEvents.find((candidate) => candidate.id === eventParam);
      const session = event ? ALL_SESSIONS.find((candidate) => candidate.id === event.sessionId) : null;
      return session
        ? { sessionId: session.id, issue: null }
        : { sessionId: null, issue: `Event ${eventParam} is unavailable in this recorded snapshot.` };
    }
    return { sessionId: null, issue: null };
  }, [initialSessionId]);
  const [range, setRange] = useWorkspaceState<RangeId>("sessions.range", "custom");
  const [q, setQ] = useWorkspaceState("sessions.query", arrival.sessionId ?? "");
  const [page, setPage] = useWorkspaceState("sessions.page", 0);
  const [sel, setSel] = useWorkspaceState<string | null>("sessions.selection", arrival.sessionId);
  const [open, setOpen] = useWorkspaceState<string | null>("sessions.expanded", null);
  const [detail, setDetail] = useWorkspaceState<string | null>("sessions.detail", arrival.sessionId);
  const [sortCol, setSortCol] = useWorkspaceState<ColId | null>("sessions.sort-column", null);
  const [sortDir, setSortDir] = useWorkspaceState<SortDir>("sessions.sort-direction", "asc");
  const [cameraState, setCamera] = useWorkspaceState<TimeWindow>("sessions.camera", windowFor("custom"));
  const [cameraHistoryState, setCameraHistory] = useWorkspaceState<TimeWindow[]>("sessions.camera-history", []);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [colsOpen, setColsOpen] = useState(false);
  const [infoOpen, setInfoOpen] = useState(false);
  const [dateOpen, setDateOpen] = useState(false);
  const [filters, setFilters] = useWorkspaceState<Filters>("sessions.filters", { provider: "all", project: "all", sub: "all" });
  const [cols, setCols] = useWorkspaceState("sessions.columns", DEFAULT_COLS);
  const camera = isTimeWindow(cameraState) ? clampWindow(cameraState) : windowFor("custom");
  const cameraHistory = Array.isArray(cameraHistoryState)
    ? cameraHistoryState.filter(isTimeWindow).map((window) => clampWindow(window))
    : [];
  const t0 = camera.t0;
  const t1 = camera.t1;
  const customStart = new Date(t0 * 1000).toISOString().slice(0, 10);
  const customEnd = new Date(t1 * 1000).toISOString().slice(0, 10);

  function moveCamera(next: TimeWindow, remember: boolean, nextRange: RangeId = "custom") {
    const cameraNext = clampWindow(next);
    if (remember && (cameraNext.t0 !== camera.t0 || cameraNext.t1 !== camera.t1)) {
      setCameraHistory([...cameraHistory.slice(-11), camera]);
    }
    setCamera(cameraNext);
    setRange(nextRange);
  }

  function cameraBack() {
    const previous = cameraHistory.at(-1);
    if (!previous) return;
    setCamera(previous);
    setCameraHistory(cameraHistory.slice(0, -1));
    setRange("custom");
  }

  function selectRange(id: RangeId) {
    if (id === "custom") {
      setRange(id);
      setDateOpen(true);
      return;
    }
    moveCamera(windowFor(id), true, id);
    setDateOpen(false);
  }

  const facetSessions = useMemo(() => ALL_SESSIONS.filter((s) => {
    if (filters.provider !== "all" && s.provider !== filters.provider) return false;
    if (filters.project !== "all" && s.project !== filters.project) return false;
    if (filters.sub === "root" && s.isSubagent) return false;
    if (filters.sub === "sub" && !s.isSubagent) return false;
    return true;
  }), [filters]);

  const ranged = useMemo(() => facetSessions.filter((s) => inWindow(s, t0, t1)), [facetSessions, t0, t1]);

  const filtered = useMemo(() => {
    const n = q.trim().toLowerCase();
    return ranged.filter((s) => {
      if (!n) return true;
      return (
        s.id.toLowerCase().includes(n) ||
        s.project.toLowerCase().includes(n) ||
        s.provider.toLowerCase().includes(n)
      );
    });
  }, [ranged, q]);

  const sorted = useMemo(
    () => sortSessions(filtered, sortCol, sortDir),
    [filtered, sortCol, sortDir],
  );

  const pages = Math.max(1, Math.ceil(sorted.length / PAGE_SIZE));
  const safePage = Math.min(page, pages - 1);
  const slice = sorted.slice(safePage * PAGE_SIZE, safePage * PAGE_SIZE + PAGE_SIZE);
  const from = sorted.length ? safePage * PAGE_SIZE + 1 : 0;
  const to = safePage * PAGE_SIZE + slice.length;
  const firstVisibleId = slice[0]?.id ?? null;
  const selOnPage = Boolean(sel && slice.some((s) => s.id === sel));

  useEffect(() => {
    setPage(0);
  }, [q, t0, t1, filters]);

  useEffect(() => {
    if (arrival.issue) {
      if (sel !== null) setSel(null);
      return;
    }
    if (!firstVisibleId) return;
    if (!selOnPage) setSel(firstVisibleId);
  }, [arrival.issue, firstVisibleId, sel, selOnPage]);

  useEffect(() => {
    if (!arrival.issue) return;
    setQ("");
    setSel(null);
    setDetail(null);
    setPage(0);
  }, [arrival.issue]);

  useEffect(() => {
    if (!arrival.sessionId) return;
    const target = ALL_SESSIONS.find((session) => session.id === arrival.sessionId);
    if (!target) return;

    setQ(target.id);
    setSel(target.id);
    setDetail(target.id);
    setPage(0);
    setFilters((current) => ({
      provider: current.provider === "all" || current.provider === target.provider ? current.provider : "all",
      project: current.project === "all" || current.project === target.project ? current.project : "all",
      sub:
        current.sub === "all" || (current.sub === "sub") === target.isSubagent
          ? current.sub
          : "all",
    }));

    const targetTs = target.startedTs;
    if (targetTs != null && (targetTs < camera.t0 || targetTs > camera.t1)) {
      const span = camera.t1 - camera.t0;
      setCamera(clampWindow({ t0: targetTs - span / 2, t1: targetTs + span / 2 }));
      setRange("custom");
    }
  }, [arrival.sessionId]);

  function pick(id: string, inspect = false) {
    if (!ALL_SESSIONS.some((session) => session.id === id)) return;
    const url = new URL(location.href);
    url.searchParams.set("session", id);
    url.searchParams.delete("event");
    if (url.searchParams.get("loom_pivot") === "1") {
      url.searchParams.set("loom_target_session", id);
      url.searchParams.delete("loom_target_event");
    }
    if (url.href !== location.href) history.replaceState(null, "", url);
    setSel(id);
    if (inspect) setDetail(id);
    const idx = sorted.findIndex((s) => s.id === id);
    if (idx >= 0) setPage(Math.floor(idx / PAGE_SIZE));
  }

  function closeDetail() {
    const id = detail;
    setDetail(null);
    if (id) window.requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-session-id="${CSS.escape(id)}"]`)?.focus());
  }

  function toggleCol(id: ColId) {
    setCols((c) => ({ ...c, [id]: !c[id] }));
  }

  function toggleSort(id: ColId) {
    if (sortCol === id) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortCol(id);
      setSortDir("asc");
    }
  }

  function th(id: ColId, lab: string, cls?: string) {
    if (!cols[id]) return null;
    return (
      <th className={cls} aria-sort={sortCol === id ? (sortDir === "asc" ? "ascending" : "descending") : undefined}>
        <button type="button" className="sn-th" onClick={() => toggleSort(id)}>
          <span>{lab}</span>
          <SortGlyph dir={sortCol === id ? sortDir : null} />
        </button>
      </th>
    );
  }

  const level = bucketLevel(t1 - t0);

  return (
    <div className={detail ? "sn-root has-detail" : "sn-root"}>
      <div className="sn-main">
        {arrival.issue ? <p className="sn-arrival-unavailable" role="status">{arrival.issue} No session has been substituted.</p> : null}
        <section className="sn-pane sn-timeline" aria-label="Message volume timeline">
          <div className="sn-tl-head">
            <b>MESSAGE VOLUME TIMELINE — ALL SESSIONS</b>
            <span className={`sn-mode is-${mode}`}>{mode === "fixture" ? "FIXTURE · SYNTHETIC SESSION SPINE" : "SNAPSHOT · RECORDED SESSION SPINE"}</span>
            <button
              type="button"
              className="sn-info"
              aria-label="Timeline source"
              title={`Each needle is one session's real spine message volume in one ${level.label === "EVENT" ? "recorded event" : `${level.label.toLowerCase()} bucket`}. Height is message count on the printed log scale. Empty buckets stay unpainted. Bodies were not copied.`}
              onClick={() => setInfoOpen((v) => !v)}
            >
              i
            </button>
            <div className="sn-ranges">
              {([
                ["24h", "24H"],
                ["7d", "7D"],
                ["30d", "30D"],
                ["custom", "CUSTOM"],
              ] as const).map(([id, lab]) => (
                <button
                  key={id}
                  type="button"
                  className={range === id ? "sn-range is-on" : "sn-range"}
                  aria-pressed={range === id}
                  onClick={() => selectRange(id)}
                >
                  {lab}
                </button>
              ))}
              <button
                type="button"
                className="sn-ico-btn"
                aria-label="Snapshot window"
                title="CUSTOM uses first-seen → last-seen in this snapshot"
                aria-expanded={dateOpen}
                onClick={() => {
                  setRange("custom");
                  setDateOpen((v) => !v);
                }}
              >
                <CalIco />
              </button>
              <button
                type="button"
                className="sn-ico-btn"
                aria-label="Timeline notes"
                onClick={() => setInfoOpen((v) => !v)}
              >
                <DotsIco />
              </button>
              <button type="button" className="sn-range" disabled={!cameraHistory.length} onClick={cameraBack}>BACK</button>
              <button type="button" className="sn-range" onClick={() => moveCamera(windowFor("custom"), true, "custom")}>FIT</button>
            </div>
          </div>
          {dateOpen ? (
            <div className="sn-date-pop">
              <label>
                FROM
                <input
                  type="date"
                  min={new Date(FIRST_TS * 1000).toISOString().slice(0, 10)}
                  max={customEnd}
                  value={customStart}
                  onChange={(e) => e.target.value && moveCamera({ t0: Date.parse(`${e.target.value}T00:00:00Z`) / 1000, t1 }, true)}
                />
              </label>
              <span aria-hidden="true">→</span>
              <label>
                TO
                <input
                  type="date"
                  min={customStart}
                  max={new Date(LAST_TS * 1000).toISOString().slice(0, 10)}
                  value={customEnd}
                  onChange={(e) => e.target.value && moveCamera({ t0, t1: Date.parse(`${e.target.value}T00:00:00Z`) / 1000 + 86399 }, true)}
                />
              </label>
              <button type="button" onClick={() => setDateOpen(false)}>DONE</button>
            </div>
          ) : null}
          {infoOpen ? (
            <p className="sn-copy" style={{ margin: "0 0 6px" }}>
              Spike field is per-session volume from real spine message timestamps. 71 sessions — absent periods remain empty and no token series is invented.
            </p>
          ) : null}
          <div className="sn-encoding" aria-label="Timeline encoding">
            <span>ALTITUDE <b>RANGE → {level.label} → SESSION</b></span>
            <span>HEIGHT <b>LOG₁₊ MESSAGES / SESSION-{level.label}</b></span>
            <span className="sn-hues"><i className="is-claude" /> CLAUDE <i className="is-cursor" /> CURSOR <i className="is-codex" /> CODEX</span>
          </div>
          <VolumeField sessions={facetSessions} camera={camera} selectedId={sel} onPick={pick} onCamera={moveCamera} />
          <OverviewBrush camera={camera} sessions={facetSessions} onCamera={moveCamera} />
        </section>

        <section className="sn-pane sn-index" aria-label="Session index">
          <div className="sn-index-head">
            <b>
              SESSIONS ({from}–{to} of {filtered.length.toLocaleString()})
            </b>
            <div className="sn-search">
              <button type="button" tabIndex={-1} aria-hidden="true">
                <SearchIco />
              </button>
              <input
                value={q}
                onChange={(e) => setQ(e.target.value)}
                placeholder="Search ids / provider / project"
                aria-label="Search session index (not transcript FTS)"
              />
            </div>
            <div className="sn-toolwrap">
            <button
              type="button"
              className={filtersOpen ? "sn-tool is-on" : "sn-tool"}
              aria-expanded={filtersOpen}
              onClick={() => {
                setFiltersOpen((v) => !v);
                setColsOpen(false);
              }}
            >
              <FilterIco /> FILTERS
            </button>
              {filtersOpen ? (
                <div className="sn-pop">
                  <div className="k">PROVIDER</div>
                  <label>
                    <input
                      type="radio"
                      name="prov"
                      checked={filters.provider === "all"}
                      onChange={() => setFilters((f) => ({ ...f, provider: "all" }))}
                    />
                    all
                  </label>
                  {PROVIDERS.map((p) => (
                    <label key={p}>
                      <input
                        type="radio"
                        name="prov"
                        checked={filters.provider === p}
                        onChange={() => setFilters((f) => ({ ...f, provider: p }))}
                      />
                      {p}
                    </label>
                  ))}
                  <div className="k">PROJECT</div>
                  <label>
                    <input
                      type="radio"
                      name="proj"
                      checked={filters.project === "all"}
                      onChange={() => setFilters((f) => ({ ...f, project: "all" }))}
                    />
                    all
                  </label>
                  {PROJECTS.map((p) => (
                    <label key={p}>
                      <input
                        type="radio"
                        name="proj"
                        checked={filters.project === p}
                        onChange={() => setFilters((f) => ({ ...f, project: p }))}
                      />
                      {p}
                    </label>
                  ))}
                  <div className="k">PARENT</div>
                  <label>
                    <input type="radio" name="sub" checked={filters.sub === "all"} onChange={() => setFilters((f) => ({ ...f, sub: "all" }))} />
                    all
                  </label>
                  <label>
                    <input type="radio" name="sub" checked={filters.sub === "root"} onChange={() => setFilters((f) => ({ ...f, sub: "root" }))} />
                    root only
                  </label>
                  <label>
                    <input type="radio" name="sub" checked={filters.sub === "sub"} onChange={() => setFilters((f) => ({ ...f, sub: "sub" }))} />
                    subagent
                  </label>
                </div>
              ) : null}
            </div>
            <div className="sn-toolwrap">
            <button
              type="button"
              className={colsOpen ? "sn-tool is-on" : "sn-tool"}
              aria-expanded={colsOpen}
              onClick={() => {
                setColsOpen((v) => !v);
                setFiltersOpen(false);
              }}
            >
              <ColIco /> COLUMNS
            </button>
              {colsOpen ? (
                <div className="sn-pop">
                  {COLUMNS.map((c) => (
                    <label key={c.id}>
                      <input type="checkbox" checked={cols[c.id]} onChange={() => toggleCol(c.id)} />
                      {c.lab}
                    </label>
                  ))}
                </div>
              ) : null}
            </div>
          </div>
          <div className="sn-scroll">
            <table className="sn-table">
              <colgroup>
                <col className="c-chev" />
                {cols.id ? <col className="c-id" /> : null}
                {cols.provider ? <col className="c-provider" /> : null}
                {cols.project ? <col className="c-project" /> : null}
                {cols.span ? <col className="c-span" /> : null}
                {cols.messages ? <col className="c-messages" /> : null}
                {cols.tokens ? <col className="c-tokens" /> : null}
                {cols.coverage ? <col className="c-coverage" /> : null}
                {cols.status ? <col className="c-status" /> : null}
              </colgroup>
              <thead>
                <tr>
                  <th aria-hidden="true" />
                  {th("id", "SESSION ID")}
                  {th("provider", "PROVIDER")}
                  {th("project", "PROJECT")}
                  {th("span", "START / END")}
                  {th("messages", "MESSAGES", "h-msg")}
                  {th("tokens", "TOKENS (PROVENANCE)", "h-tok")}
                  {th("coverage", "COVERAGE", "h-cov")}
                  {th("status", "SOURCE", "h-stat")}
                </tr>
              </thead>
              <tbody>
                {slice.map((s, rowIndex) => {
                  const expanded = open === s.id;
                  return (
                    <Fragment key={s.id}>
                      <tr
                        className={s.id === sel ? "is-on" : ""}
                        data-session-id={s.id}
                        tabIndex={0}
                        aria-selected={s.id === sel}
                        onClick={() => pick(s.id, true)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            pick(s.id, true);
                          } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                            e.preventDefault();
                            const next = slice[rowIndex + (e.key === "ArrowDown" ? 1 : -1)];
                            if (!next) return;
                            pick(next.id);
                            window.requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-session-id="${CSS.escape(next.id)}"]`)?.focus());
                          }
                        }}
                      >
                        <td>
                          <button
                            type="button"
                            className={expanded ? "sn-chev is-open" : "sn-chev"}
                            aria-label={expanded ? "Collapse session" : "Expand session"}
                            onClick={(e) => {
                              e.stopPropagation();
                              setOpen(expanded ? null : s.id);
                              pick(s.id);
                            }}
                          >
                            <svg viewBox="0 0 12 12" aria-hidden="true">
                              <path d="M4 2.5 8.5 6 4 9.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
                            </svg>
                          </button>
                        </td>
                        {cols.id ? (
                          <td className="id" title={s.id}>
                            {shortId(s.id, 17)}
                          </td>
                        ) : null}
                        {cols.provider ? <td className="prov">{s.provider}</td> : null}
                        {cols.project ? (
                          <td className="proj" title={s.project}>
                            {s.project}
                          </td>
                        ) : null}
                        {cols.span ? (
                          <td className="span">
                            <SpanTimes start={s.startedAt} end={s.endedAt} />
                          </td>
                        ) : null}
                        {cols.messages ? <td className="msg">{s.messages.toLocaleString()}</td> : null}
                        {cols.tokens ? (
                          <td className="tok">
                            <span className="dim">—</span> <span className="amber">(unavailable)</span>
                          </td>
                        ) : null}
                        {cols.coverage ? (
                          <td className="cov">
                            <span className="dim" title={`coverage: ${s.coverage} — no transcript coverage to grade`}>
                              —
                            </span>
                          </td>
                        ) : null}
                        {cols.status ? (
                          <td className="stat">
                            <span className="sn-status">{s.status}</span>
                          </td>
                        ) : null}
                      </tr>
                      {expanded ? (
                        <tr key={`${s.id}-x`} className="sn-expand">
                          <td colSpan={1 + Object.values(cols).filter(Boolean).length}>
                            <div className="sn-expand-grid">
                              <div>
                                <span>PARENT_SESSION_ID</span>
                                <b>
                                  {s.parentId ? (
                                    PARENT_IDS.has(s.parentId) ? (
                                      shortId(s.parentId, 22)
                                    ) : (
                                      <>
                                        {shortId(s.parentId, 18)} <em>not in snapshot</em>
                                      </>
                                    )
                                  ) : (
                                    "none"
                                  )}
                                </b>
                              </div>
                              <div>
                                <span>AGENT</span>
                                <b>{s.agentId ? shortId(s.agentId, 18) : "—"}</b>
                              </div>
                              <div>
                                <span>KINDS</span>
                                <b>
                                  {Object.entries(s.kinds)
                                    .map(([k, v]) => `${k} ${v}`)
                                    .join(" · ") || "—"}
                                </b>
                              </div>
                              <div>
                                <span>ROLES</span>
                                <b>
                                  {Object.entries(s.roles)
                                    .map(([k, v]) => `${k} ${v}`)
                                    .join(" · ") || "—"}
                                </b>
                              </div>
                              <div>
                                <span>TOKENS</span>
                                <b>
                                  <em>unavailable in snapshot</em>
                                </b>
                              </div>
                              <div>
                                <span>TRANSCRIPT</span>
                                <b>
                                  <em>bodies not copied</em>
                                </b>
                              </div>
                              <p className="sn-note">
                                Private chain-of-thought is unavailable by design. Branch / worktree / commit links are not in this spine index.
                              </p>
                            </div>
                          </td>
                        </tr>
                      ) : null}
                    </Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
          <div className="sn-pager">
            <span>Rows per page: {PAGE_SIZE}</span>
            <div className="grow">
              <button type="button" disabled={safePage === 0} onClick={() => setPage(0)} aria-label="First page">
                «
              </button>
              <button
                type="button"
                disabled={safePage === 0}
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                aria-label="Previous page"
              >
                ‹
              </button>
              <span>
                Page {safePage + 1} of {pages}
              </span>
              <button
                type="button"
                disabled={safePage >= pages - 1}
                onClick={() => setPage((p) => p + 1)}
                aria-label="Next page"
              >
                ›
              </button>
              <button
                type="button"
                disabled={safePage >= pages - 1}
                onClick={() => setPage(pages - 1)}
                aria-label="Last page"
              >
                »
              </button>
            </div>
          </div>
        </section>
      </div>
      <Inspector
        scoped={filtered}
        page={safePage}
        pages={pages}
        selected={ALL_SESSIONS.find((s) => s.id === detail) ?? null}
        fixture={mode === "fixture"}
        onClose={closeDetail}
        onNavigate={navigate}
      />
    </div>
  );
}
