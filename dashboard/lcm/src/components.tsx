/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Presentational components for the hermes-lcm dashboard plugin.
 *
 * Ported 1:1 from the original hand-written IIFE. All `hermes-lcm-*` class
 * names are preserved so `style.css` continues to apply unchanged. Behavior,
 * DOM structure, and prop flow are identical to the original `h(...)` calls —
 * only the surface syntax changed (`React.createElement` → JSX).
 */

import React, { useCallback, useEffect, useRef, useState } from "react";
import { MarkdownText } from "./markdown";
import {
  SESSION_FETCH_BATCH,
  SESSION_MESSAGE_PAGE_SIZE,
  clampText,
  copyTextValue,
  escapeRegExp,
  fmtAbsoluteTime,
  fmtInt,
  fmtTime,
  parseJsonArray,
  parseLeadingJSON,
  queryTerms,
  ratioStr,
  sessionLabel,
  short,
  stripMd,
  summaryTitle,
} from "./helpers";
import { EmptyState, Stat } from "../../lib/primitives";

// --- search-highlight rendering -------------------------------------------

export function renderHighlightedText(text: any, query: any): any {
  const raw = String(text || "");
  const terms = queryTerms(query);
  if (!terms.length) return raw;
  const re = new RegExp("(" + terms.map(escapeRegExp).join("|") + ")", "ig");
  const parts = raw.split(re);
  return parts.map(function (part, idx) {
    if (!part) return null;
    return terms.some(function (term) { return part.toLowerCase() === term.toLowerCase(); })
      ? <mark key={"hl" + idx} className="hermes-lcm-mark">{part}</mark>
      : part;
  });
}

export function renderSnippet(text: any): any {
  const raw = String(text || "");
  const parts: any[] = [];
  const re = /\[([^\]]*)\]/g;
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = re.exec(raw)) !== null) {
    if (m.index > last) parts.push(raw.slice(last, m.index));
    parts.push(<mark key={"mk" + i++} className="hermes-lcm-mark">{m[1]}</mark>);
    last = re.lastIndex;
  }
  if (last < raw.length) parts.push(raw.slice(last));
  return parts.length ? parts : raw;
}

export function renderSearchSnippet(text: any, query: any): any {
  const raw = String(text || "");
  if (/\[[^\]]+\]/.test(raw)) return renderSnippet(raw);
  return renderHighlightedText(raw, query);
}

// --- shared atoms ----------------------------------------------------------

function codeBlock(text: any): React.ReactElement {
  return <pre className="hermes-lcm-md-pre"><code>{clampText(text, 4000)}</code></pre>;
}

export function toolBadge(label: any, kind?: string): React.ReactElement {
  return <span className={"hermes-lcm-tag" + (kind ? " hermes-lcm-tag-" + kind : "")}>{label}</span>;
}

export function Pager(props: { totalPages?: number; page: number; onChange: (n: number) => void }): React.ReactElement | null {
  if (!props.totalPages || props.totalPages <= 1) return null;
  return (
    <div className="hermes-lcm-pager">
      <button
        type="button"
        className="hermes-lcm-btn"
        disabled={props.page <= 1}
        onClick={() => props.onChange(props.page - 1)}
      >Prev</button>
      <span className="hermes-lcm-pager-status">{`Page ${props.page} / ${props.totalPages}`}</span>
      <button
        type="button"
        className="hermes-lcm-btn"
        disabled={props.page >= props.totalPages}
        onClick={() => props.onChange(props.page + 1)}
      >Next</button>
    </div>
  );
}

export function TimeText(props: { epoch: any; className?: string }): React.ReactElement {
  if (!props.epoch) {
    return <span className={props.className || "hermes-lcm-dim"}>—</span>;
  }
  const absolute = fmtAbsoluteTime(props.epoch);
  let dateTime = "";
  try {
    dateTime = new Date(Number(props.epoch) * 1000).toISOString();
  } catch (e) { /* best-effort */ }
  return (
    <time
      className={props.className || "hermes-lcm-dim"}
      title={absolute}
      dateTime={dateTime || undefined}
    >
      {fmtTime(props.epoch)}
    </time>
  );
}

export function CopyButton(props: { text: any; label?: string; title?: string }): React.ReactElement {
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  const onCopy = useCallback(function (e: any) {
    if (e) {
      e.preventDefault();
      e.stopPropagation();
    }
    copyTextValue(props.text).then(function (ok) {
      setStatus(ok ? "copied" : "failed");
      setTimeout(function () { setStatus("idle"); }, 1400);
    });
  }, [props.text]);
  const label = status === "copied" ? "Copied" : (status === "failed" ? "Retry copy" : (props.label || "Copy"));
  return (
    <button
      type="button"
      className={"hermes-lcm-btn hermes-lcm-copy" + (status === "copied" ? " is-copied" : "")}
      onClick={onCopy}
      title={props.title || "Copy to clipboard"}
    >{label}</button>
  );
}

// --- chart-ish presentational components -----------------------------------
// NOTE: BarList (label/value rows with proportional fills) now uses the shared
// `tdp-bar-list` primitive (lib/primitives.tsx, `proportional` mode), which
// ports this component's head + fill-track layout verbatim. The genuinely
// chart-shaped presentational components below (TimelineChart, CompressionBars)
// are NOT BarList duplicates — they render dated histograms and dual kept/saved
// bars respectively — so they stay plugin-local.

/** Responsive CSS bar chart (no SVG stretching, so bars stay crisp and the
 *  summary markers render as true round dots regardless of bucket count). */
export function TimelineChart(props: { buckets?: any[]; nodeBuckets?: any[]; undatedCount?: any }): React.ReactElement {
  // Buckets cover only messages with real timestamps; the server reports
  // messages without one separately so they surface as an honest note
  // instead of a fake single bar.
  const buckets = (props.buckets || []).filter(function (b) { return b && b.bucket != null; });
  const nodeBuckets = props.nodeBuckets || [];
  const undatedCount = Number(props.undatedCount) || 0;
  if (!buckets.length) {
    return (
      <EmptyState className="hermes-lcm-empty">
        {undatedCount > 0
          ? `No dated messages yet — ${fmtInt(undatedCount)} stored messages have no timestamp`
          : "No timeline data"}
      </EmptyState>
    );
  }
  const maxCount = buckets.reduce((acc, b) => Math.max(acc, Number(b.count) || 0), 0) || 1;
  const nodeByBucket: Record<string, number> = {};
  nodeBuckets.forEach(function (nb) { nodeByBucket[nb.bucket] = Number(nb.count) || 0; });

  const cols = buckets.map(function (b, i) {
    const count = Number(b.count) || 0;
    const pct = count > 0 ? Math.max(3, Math.round((count / maxCount) * 100)) : 0;
    const nodes = nodeByBucket[b.bucket] || 0;
    const tip = `${b.bucket}: ${fmtInt(count)} messages`
      + (nodes ? ` · ${fmtInt(nodes)} summaries` : "");
    return (
      <div key={b.bucket + i} className="hermes-lcm-tl-col" title={tip}>
        <div className={"hermes-lcm-tl-dot" + (nodes ? " hermes-lcm-tl-dot-on" : "")} />
        <div className="hermes-lcm-tl-bar" style={{ height: pct + "%" }} />
      </div>
    );
  });

  return (
    <div className="hermes-lcm-tl">
      <div className="hermes-lcm-tl-bars">{cols}</div>
      <div className="hermes-lcm-svg-axis">
        <span>{short(buckets[0].bucket, 16)}</span>
        <span>{short(buckets[buckets.length - 1].bucket, 16)}</span>
      </div>
      {undatedCount > 0 ? (
        <div className="hermes-lcm-dim hermes-lcm-tl-undated">
          {`${fmtInt(undatedCount)} undated messages not shown`}
        </div>
      ) : null}
    </div>
  );
}

/** Per-group compression (kept vs saved), rendered as a small inline SVG. */
export function CompressionBars(props: { groups?: any[]; onPick?: (g: any) => void }): React.ReactElement {
  const groups = props.groups || [];
  const onPick = props.onPick;
  if (!groups.length) return <EmptyState className="hermes-lcm-empty">No compression data</EmptyState>;
  const maxSrc = groups.reduce((acc, g) => Math.max(acc, Number(g.source_token_count) || 0), 0) || 1;
  return (
    <div className="hermes-lcm-comp">
      {groups.map(function (g, idx) {
        const src = Number(g.source_token_count) || 0;
        const out = Number(g.token_count) || 0;
        const totalW = Math.max(0.5, (src / maxSrc) * 100);
        const keptW = src > 0 ? (out / src) * totalW : 0;
        const sid = g.session_id != null ? g.session_id : g.key;
        const label = (typeof sid === "string" && /^\d{8}_/.test(sid))
          ? sessionLabel(sid)
          : (g.depth != null ? `node #${g.key} (D${g.depth})` : String(g.key));
        const clickable = typeof onPick === "function";
        return (
          <div
            key={String(g.key) + idx}
            className={"hermes-lcm-comp-row" + (clickable ? " hermes-lcm-clk" : "")}
            onClick={clickable ? function () { onPick!(g); } : undefined}
          >
            <div className="hermes-lcm-comp-head">
              <span className="hermes-lcm-k">{label}</span>
              <span className="hermes-lcm-v">{`${g.ratio || 0}× · ${fmtInt(src)}→${fmtInt(out)}`}</span>
            </div>
            <svg
              viewBox="0 0 100 8"
              preserveAspectRatio="none"
              width="100%"
              height={8}
              className="hermes-lcm-svgbar"
            >
              <rect x={0} y={0} width={totalW} height={8} rx={1.5} className="hermes-lcm-svg-saved" />
              <rect x={0} y={0} width={keptW} height={8} rx={1.5} className="hermes-lcm-svg-kept" />
            </svg>
          </div>
        );
      })}
    </div>
  );
}

// --- tool-result rendering. Known tools get bespoke components; any other
// JSON falls back to a clean key/value view; non-JSON to markdown. -----------

function ToolOutput(props: { data: any }): React.ReactElement {
  const d = props.data;
  const out = d.output != null
    ? d.output
    : (typeof d.result === "string" ? d.result
      : (d.result != null ? JSON.stringify(d.result, null, 2) : ""));
  const code = d.exit_code != null ? d.exit_code : d.status;
  const ok = d.exit_code === 0 || d.status === "success" || d.status === "exited" || d.success === true;
  return (
    <div className="hermes-lcm-tool">
      {(code != null || d.duration_seconds != null) ? (
        <div className="hermes-lcm-tool-meta">
          {code != null ? toolBadge((d.exit_code != null ? "exit " : "") + code, ok ? "ok" : "bad") : null}
          {d.duration_seconds != null ? <span className="hermes-lcm-dim">{d.duration_seconds + "s"}</span> : null}
        </div>
      ) : null}
      {out ? codeBlock(out) : null}
      {d.error ? <div className="hermes-lcm-tool-err">{String(d.error)}</div> : null}
      {d.timeout_note ? <div className="hermes-lcm-tool-err">{String(d.timeout_note)}</div> : null}
    </div>
  );
}

function ToolReadFile(props: { data: any }): React.ReactElement {
  const d = props.data;
  return (
    <div className="hermes-lcm-tool">
      <div className="hermes-lcm-tool-meta">
        {d.total_lines != null ? toolBadge(fmtInt(d.total_lines) + " lines") : null}
        {d.file_size != null ? toolBadge(fmtInt(d.file_size) + " B") : null}
        {d.truncated ? toolBadge("truncated", "warn") : null}
        {d.is_image ? toolBadge("image") : null}
      </div>
      {d.content ? codeBlock(d.content) : null}
      {(d.hint || d._hint) ? <div className="hermes-lcm-dim">{String(d.hint || d._hint)}</div> : null}
    </div>
  );
}

function ToolSearchFiles(props: { data: any }): React.ReactElement {
  const d = props.data;
  const matches = d.matches || [];
  return (
    <div className="hermes-lcm-tool">
      <div className="hermes-lcm-tool-meta">
        {toolBadge(fmtInt(d.total_count != null ? d.total_count : matches.length) + " matches")}
      </div>
      <div className="hermes-lcm-tool-matches">
        {matches.slice(0, 50).map(function (mm: any, i: number) {
          return (
            <div key={i} className="hermes-lcm-tool-match">
              <div className="hermes-lcm-tool-match-loc">
                <span className="hermes-lcm-tool-path">{short(String(mm.path || ""), 72)}</span>
                {mm.line != null ? <span className="hermes-lcm-dim">{":" + mm.line}</span> : null}
              </div>
              {mm.content != null
                ? <code className="hermes-lcm-tool-match-code">{short(String(mm.content), 220)}</code>
                : null}
            </div>
          );
        })}
      </div>
      {matches.length > 50 ? <div className="hermes-lcm-dim">{"+" + fmtInt(matches.length - 50) + " more"}</div> : null}
    </div>
  );
}

const TODO_ICON: Record<string, string> = { completed: "✓", in_progress: "◐", pending: "○", cancelled: "✗" };

function ToolTodo(props: { data: any }): React.ReactElement {
  const d = props.data;
  const todos = d.todos || [];
  const s = d.summary || {};
  return (
    <div className="hermes-lcm-tool">
      <div className="hermes-lcm-tool-meta">
        {s.completed != null ? toolBadge(s.completed + " done", "ok") : null}
        {s.in_progress != null ? toolBadge(s.in_progress + " active") : null}
        {s.pending != null ? toolBadge(s.pending + " todo") : null}
        {s.cancelled ? toolBadge(s.cancelled + " cancelled") : null}
      </div>
      <ul className="hermes-lcm-todo">
        {todos.map(function (t: any, i: number) {
          const st = String(t.status || "pending");
          return (
            <li key={t.id || i} className={"hermes-lcm-todo-item hermes-lcm-todo-" + st}>
              <span className="hermes-lcm-todo-ic">{TODO_ICON[st] || "•"}</span>
              <span>{String(t.content || "")}</span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function ToolPatch(props: { data: any; raw: any }): React.ReactElement {
  const d = props.data;
  const raw = props.raw;
  let diff = d && d.diff;
  if (diff == null && typeof raw === "string") {
    const mm = raw.match(/"diff"\s*:\s*"([\s\S]*?)"\s*\}?\s*$/);
    diff = mm ? mm[1].replace(/\\n/g, "\n").replace(/\\"/g, '"').replace(/\\t/g, "\t") : raw;
  }
  const lines = String(diff || "").split("\n");
  return (
    <div className="hermes-lcm-tool">
      {(d && d.success != null) ? (
        <div className="hermes-lcm-tool-meta">
          {toolBadge(d.success ? "applied" : "failed", d.success ? "ok" : "bad")}
        </div>
      ) : null}
      <pre className="hermes-lcm-md-pre hermes-lcm-diff">
        {lines.slice(0, 240).map(function (ln: string, i: number) {
          const c0 = ln.charAt(0);
          const cls = c0 === "+" ? "hermes-lcm-diff-add"
            : c0 === "-" ? "hermes-lcm-diff-del"
            : c0 === "@" ? "hermes-lcm-diff-hunk" : "";
          return <div key={i} className={cls}>{ln || " "}</div>;
        })}
      </pre>
    </div>
  );
}

function ToolSkill(props: { data: any }): React.ReactElement {
  const d = props.data;
  const tags = d.tags || [];
  return (
    <div className="hermes-lcm-tool">
      <div className="hermes-lcm-tool-meta">
        {d.name ? toolBadge(d.name) : null}
        {tags.slice(0, 6).map(function (t: any, i: number) {
          return <span key={i} className="hermes-lcm-dim">{"#" + t}</span>;
        })}
      </div>
      {d.description ? <div className="hermes-lcm-dim">{String(d.description)}</div> : null}
      {d.content ? <MarkdownText text={clampText(d.content, 4000)} /> : null}
    </div>
  );
}

function ToolGeneric(props: { data: any }): React.ReactElement {
  const d = props.data;
  if (Array.isArray(d)) return codeBlock(JSON.stringify(d, null, 2));
  return (
    <div className="hermes-lcm-kv">
      {Object.keys(d).map(function (k, i) {
        const v = d[k];
        let vn: React.ReactNode;
        if (v == null) vn = <span className="hermes-lcm-dim">null</span>;
        else if (typeof v === "object") {
          vn = (
            <pre className="hermes-lcm-md-pre">
              <code>{clampText(JSON.stringify(v, null, 2), 1500)}</code>
            </pre>
          );
        } else vn = <span>{String(v)}</span>;
        return (
          <div key={k + i} className="hermes-lcm-kv-row">
            <span className="hermes-lcm-kv-k">{k}</span>
            <span className="hermes-lcm-kv-v">{vn}</span>
          </div>
        );
      })}
    </div>
  );
}

export function ToolResult(props: { name: any; content: any }): React.ReactElement {
  const name = String(props.name || "");
  const raw = props.content;
  const parsed = parseLeadingJSON(raw);
  const data = parsed ? parsed.value : undefined;
  const note = (parsed && parsed.rest) ? parsed.rest : "";
  let body: any = null;
  try {
    if ((name === "terminal" || name === "process" || name === "execute_code" || name === "shell")
      && data && typeof data === "object") body = <ToolOutput data={data} />;
    else if (name === "read_file" && data) body = <ToolReadFile data={data} />;
    else if (name === "search_files" && data) body = <ToolSearchFiles data={data} />;
    else if (name === "todo" && data) body = <ToolTodo data={data} />;
    else if (name === "patch") body = <ToolPatch data={data} raw={raw} />;
    else if (name === "skill_view" && data) body = <ToolSkill data={data} />;
    else if (data && typeof data === "object") body = <ToolGeneric data={data} />;
  } catch (e) { body = null; }
  if (body == null) {
    return <MarkdownText className="hermes-lcm-msg-body" text={short(String(raw == null ? "" : raw), 4000)} />;
  }
  if (note) {
    return (
      <div className="hermes-lcm-tool">
        {body}
        <div className="hermes-lcm-dim hermes-lcm-tool-note">{short(note, 400)}</div>
      </div>
    );
  }
  return body;
}

// --- list rows -------------------------------------------------------------

export function SearchResultCard(props: {
  key?: React.Key;
  item: any;
  kind: string;
  query: any;
  selected?: boolean;
  resultRef?: (el: HTMLElement | null) => void;
  onFocus: () => void;
  onOpen: () => void;
}): React.ReactElement {
  const item = props.item;
  const isMessage = props.kind === "message";
  const title = isMessage
    ? short(item.session_id || "", 42)
    : short(summaryTitle(item.summary || ""), 90);
  const preview = isMessage
    ? renderSearchSnippet(item.snippet || short(item.content, 240), props.query)
    : renderSearchSnippet(item.snippet || short(stripMd(item.summary), 240), props.query);
  const keyValue = props.kind + ":" + String(isMessage ? item.store_id : item.node_id);
  return (
    <button
      type="button"
      ref={props.resultRef as any}
      className={"hermes-lcm-result hermes-lcm-result-btn" + (props.selected ? " hermes-lcm-selected" : "")}
      onClick={props.onOpen}
      onFocus={props.onFocus}
      onMouseEnter={props.onFocus}
    >
      <div className="hermes-lcm-row-meta">
        <span className="hermes-lcm-pill hermes-lcm-pill-accent">
          {isMessage ? (item.role || "message") : ("D" + item.depth)}
        </span>
        {!isMessage && item.category ? <span className="hermes-lcm-pill">{item.category}</span> : null}
        {isMessage && item.source ? <span className="hermes-lcm-pill">{item.source}</span> : null}
        {isMessage && item.tool_name ? <span className="hermes-lcm-pill">{short(item.tool_name, 24)}</span> : null}
        <span className="hermes-lcm-dim">{keyValue}</span>
        {isMessage
          ? <TimeText className="hermes-lcm-dim" epoch={item.timestamp} />
          : <TimeText className="hermes-lcm-dim" epoch={item.latest_at || item.created_at} />}
      </div>
      <div className="hermes-lcm-row-title">{title}</div>
      <div className="hermes-lcm-msg-body">{preview}</div>
      <div className="hermes-lcm-result-foot">
        <span className="hermes-lcm-dim">
          {isMessage
            ? `${fmtInt(item.token_estimate)} tok · ${sessionLabel(item.session_id)}`
            : `${fmtInt(item.source_token_count)}→${fmtInt(item.token_count)} tok · ${sessionLabel(item.session_id)}`}
        </span>
        <span className="hermes-lcm-dim">{isMessage ? "Open full message" : "Open source links"}</span>
      </div>
    </button>
  );
}

export function MessageItem(props: {
  key?: React.Key;
  m: any;
  onOpenMessage?: (m: any) => void;
  active?: boolean;
  compact?: boolean;
  previewText?: any;
  query?: any;
}): React.ReactElement {
  const m = props.m;
  const clickable = typeof props.onOpenMessage === "function";
  let body: React.ReactNode;
  if (props.previewText) {
    body = <div className="hermes-lcm-msg-body">{renderSearchSnippet(props.previewText, props.query)}</div>;
  } else if (m.role === "tool") {
    body = <ToolResult name={m.tool_name} content={m.content} />;
  } else {
    body = (
      <MarkdownText
        className="hermes-lcm-msg-body"
        text={props.compact ? short(m.content, 900) : String(m.content || "")}
      />
    );
  }
  return (
    <div
      className={"hermes-lcm-msg" + (clickable ? " hermes-lcm-clk" : "") + (props.active ? " hermes-lcm-selected" : "")}
      onClick={clickable ? function () { props.onOpenMessage!(m); } : undefined}
    >
      <div className="hermes-lcm-msg-head">
        <div className="hermes-lcm-msg-meta">
          <span className="hermes-lcm-tag">{m.role || "?"}</span>
          {m.source ? <span className="hermes-lcm-tag hermes-lcm-tag-src">{m.source}</span> : null}
          {m.tool_name ? <span className="hermes-lcm-tag">{m.tool_name}</span> : null}
          {m.pinned ? <span className="hermes-lcm-tag">pinned</span> : null}
          {m.store_id != null ? <span className="hermes-lcm-dim">{"#" + m.store_id}</span> : null}
          <TimeText className="hermes-lcm-dim" epoch={m.timestamp} />
          {m.token_estimate ? <span className="hermes-lcm-dim">{`${fmtInt(m.token_estimate)} tok`}</span> : null}
        </div>
        <div className="hermes-lcm-msg-actions">
          {clickable ? (
            <button
              type="button"
              className="hermes-lcm-btn"
              aria-label={"Open message #" + m.store_id}
              onClick={function (e) { e.stopPropagation(); props.onOpenMessage!(m); }}
            >Open</button>
          ) : null}
          <CopyButton text={m.content} label="Copy" title="Copy message content" />
        </div>
      </div>
      {body}
    </div>
  );
}

export function NodeRef(props: { key?: React.Key; n: any; onOpen: (id: any) => void; active?: boolean }): React.ReactElement {
  const n = props.n;
  const onOpen = props.onOpen;
  return (
    <button
      type="button"
      className={"hermes-lcm-noderef hermes-lcm-clk" + (props.active ? " hermes-lcm-selected" : "")}
      onClick={function () { onOpen(n.node_id); }}
    >
      <div className="hermes-lcm-msg-meta">
        <span className="hermes-lcm-tag">{`D${n.depth}`}</span>
        {n.category ? <span className="hermes-lcm-tag">{n.category}</span> : null}
        <span className="hermes-lcm-dim">{`#${n.node_id}`}</span>
        {n.source_type ? <span className="hermes-lcm-dim">{n.source_type}</span> : null}
        {(n.token_count != null) ? <span className="hermes-lcm-dim">{`${fmtInt(n.token_count)} tok`}</span> : null}
        <TimeText className="hermes-lcm-dim" epoch={n.latest_at || n.created_at} />
      </div>
      <div className="hermes-lcm-msg-body">{short(stripMd(n.summary), 320)}</div>
    </button>
  );
}

// --- detail panels ---------------------------------------------------------

export function MessageDetail(props: {
  data: any;
  onOpenNode: (id: any) => void;
  onOpenSession: (id: any, opts?: any) => void;
}): React.ReactElement {
  const d = props.data || {};
  const message = d.message;
  const session = d.session;
  if (!message) return <EmptyState className="hermes-lcm-empty">Message not found</EmptyState>;
  const sessionNodes = (session && session.summary_nodes) || [];
  // Prefer the backend's exact message→summary linkage (summary_node_ids,
  // additive field) and fall back to same-session summaries when absent.
  const linkedIds = parseJsonArray(message.summary_node_ids).map(String);
  const linkedSet: Record<string, boolean> = {};
  linkedIds.forEach(function (id) { linkedSet[id] = true; });
  const linkedNodes = linkedIds.length
    ? sessionNodes.filter(function (node: any) { return linkedSet[String(node.node_id)]; })
    : [];
  const hasExactLinks = linkedIds.length > 0;
  const relatedNodes = hasExactLinks ? linkedNodes : sessionNodes;
  const unresolvedLinkIds = hasExactLinks
    ? linkedIds.filter(function (id) {
        return !relatedNodes.some(function (node: any) { return String(node.node_id) === id; });
      })
    : [];
  return (
    <div className="hermes-lcm-detail">
      <div className="hermes-lcm-detail-meta">
        <span className="hermes-lcm-tag">{message.role || "message"}</span>
        {message.source ? <span className="hermes-lcm-tag hermes-lcm-tag-src">{message.source}</span> : null}
        <span className="hermes-lcm-tag">{`#${message.store_id}`}</span>
        <TimeText className="hermes-lcm-dim" epoch={message.timestamp} />
        {message.token_estimate ? <span className="hermes-lcm-dim">{`${fmtInt(message.token_estimate)} tok`}</span> : null}
        <button
          type="button"
          className="hermes-lcm-btn"
          onClick={function () { props.onOpenSession(message.session_id, { activeMessageId: message.store_id }); }}
        >{sessionLabel(message.session_id)}</button>
      </div>
      <div className="hermes-lcm-msg-actions">
        <CopyButton text={message.content} label="Copy message" title="Copy full message content" />
      </div>
      <div className="hermes-lcm-detail-grid">
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Session</div>
          <div className="hermes-lcm-detail-v">{short(message.session_id, 48)}</div>
        </div>
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Absolute time</div>
          <div className="hermes-lcm-detail-v">{fmtAbsoluteTime(message.timestamp) || "—"}</div>
        </div>
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Token estimate</div>
          <div className="hermes-lcm-detail-v">{fmtInt(message.token_estimate || 0)}</div>
        </div>
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">{hasExactLinks ? "Linked summaries" : "Related summaries"}</div>
          <div className="hermes-lcm-detail-v">
            {fmtInt(hasExactLinks ? linkedIds.length : relatedNodes.length)}
          </div>
        </div>
      </div>
      <h4>Content</h4>
      <MessageItem m={message} />
      <h4>{hasExactLinks
        ? `Summaries built from this message (${linkedIds.length})`
        : `Summaries in this session (${relatedNodes.length})`}</h4>
      {relatedNodes.length ? (
        <div className="hermes-lcm-stream">
          {relatedNodes.map(function (node: any) {
            return <NodeRef key={node.node_id} n={node} onOpen={props.onOpenNode} />;
          })}
        </div>
      ) : null}
      {unresolvedLinkIds.length ? (
        <div className="hermes-lcm-stream">
          {unresolvedLinkIds.map(function (id: string) {
            return (
              <button
                key={"link:" + id}
                type="button"
                className="hermes-lcm-noderef hermes-lcm-clk"
                onClick={function () { props.onOpenNode(id); }}
              >
                <div className="hermes-lcm-msg-meta">
                  <span className="hermes-lcm-tag">summary</span>
                  <span className="hermes-lcm-dim">{"#" + id}</span>
                </div>
                <div className="hermes-lcm-msg-body">Open linked summary node</div>
              </button>
            );
          })}
        </div>
      ) : null}
      {(!relatedNodes.length && !unresolvedLinkIds.length)
        ? <EmptyState className="hermes-lcm-empty">No summary nodes reference this message yet.</EmptyState>
        : null}
    </div>
  );
}

export function NodeDetail(props: {
  data: any;
  onOpenNode: (id: any) => void;
  onOpenSession: (id: any, opts?: any) => void;
  onOpenMessage: (m: any) => void;
}): React.ReactElement {
  const d = props.data || {};
  const node = d.node;
  const onOpenNode = props.onOpenNode;
  const onOpenSession = props.onOpenSession;
  const onOpenMessage = props.onOpenMessage;
  if (!node) return <EmptyState className="hermes-lcm-empty">Node not found</EmptyState>;
  const sources = d.sources || {};
  const tags = parseJsonArray(node.tags);
  const entities = parseJsonArray(node.entities);
  return (
    <div className="hermes-lcm-detail">
      <div className="hermes-lcm-detail-meta">
        <span className="hermes-lcm-tag">{`Depth ${node.depth}`}</span>
        {node.category ? <span className="hermes-lcm-tag">{node.category}</span> : null}
        <button
          type="button"
          className="hermes-lcm-btn"
          onClick={function () { onOpenSession(node.session_id); }}
        >{sessionLabel(node.session_id)}</button>
        <span className="hermes-lcm-dim">{`${fmtInt(node.source_token_count)}→${fmtInt(node.token_count)} tok`}</span>
        <TimeText className="hermes-lcm-dim" epoch={node.latest_at || node.created_at} />
      </div>
      <div className="hermes-lcm-detail-grid">
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Node id</div>
          <div className="hermes-lcm-detail-v">{String(node.node_id)}</div>
        </div>
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Source type</div>
          <div className="hermes-lcm-detail-v">{sources.type || node.source_type || "—"}</div>
        </div>
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Tags</div>
          <div className="hermes-lcm-detail-v">{tags.length ? tags.join(", ") : "—"}</div>
        </div>
        <div className="hermes-lcm-detail-cell">
          <div className="hermes-lcm-detail-k">Entities</div>
          <div className="hermes-lcm-detail-v">{entities.length ? entities.join(", ") : "—"}</div>
        </div>
      </div>
      <h4>Summary</h4>
      <MarkdownText className="hermes-lcm-summary" text={node.summary} />
      {node.expand_hint ? (
        <div className="hermes-lcm-hint">
          <strong>Expand hint: </strong>{node.expand_hint}
        </div>
      ) : null}
      <h4>{`Source links (${sources.type || "?"}, ${(sources.ids || []).length})`}</h4>
      {(() => {
        const isNodes = sources.type === "nodes";
        const items = isNodes ? (sources.nodes || []) : (sources.messages || []);
        if (!items.length) {
          return (
            <EmptyState className="hermes-lcm-empty">
              {(sources.ids || []).length
                ? "Source items are no longer in the database."
                : "This summary records no source items."}
            </EmptyState>
          );
        }
        return (
          <div className="hermes-lcm-stream">
            {items.map(function (it: any) {
              return isNodes
                ? <NodeRef key={it.node_id} n={it} onOpen={onOpenNode} />
                : <MessageItem key={it.store_id} m={it} onOpenMessage={onOpenMessage} />;
            })}
          </div>
        );
      })()}
    </div>
  );
}

export function SessionDetail(props: {
  data: any;
  onOpenNode: (id: any) => void;
  onOpenMessage: (m: any) => void;
  onLoadMore: () => void;
  loadingMore?: boolean;
  activeMessageId?: any;
}): React.ReactElement {
  const d = props.data || {};
  const onOpenNode = props.onOpenNode;
  const onOpenMessage = props.onOpenMessage;
  const c = d.counts || {};
  const [page, setPage] = useState(1);
  const sessionId = d.session_id;
  useEffect(function () {
    setPage(1);
  }, [sessionId]);
  const loadedMessages = d.messages || [];
  const shownCount = Math.min(loadedMessages.length, page * SESSION_MESSAGE_PAGE_SIZE);
  const visibleMessages = loadedMessages.slice(0, shownCount);
  return (
    <div className="hermes-lcm-detail">
      <div className="hermes-lcm-statrow">
        <Stat variant="compact" value={fmtInt(c.message_count)} label="messages" />
        <Stat variant="compact" value={fmtInt(c.summary_node_count)} label="summaries" />
        <Stat variant="compact" value={fmtInt(c.token_estimate_total)} label="msg tokens" />
        <Stat
          variant="compact"
          value={ratioStr(c.source_token_count, c.summary_token_count)}
          label="compression"
        />
      </div>
      {(d.summary_nodes && d.summary_nodes.length) ? (
        <div>
          <h4>{`Summary nodes (${d.summary_nodes.length})`}</h4>
          <div className="hermes-lcm-stream">
            {d.summary_nodes.map(function (n: any) {
              return <NodeRef key={n.node_id} n={n} onOpen={onOpenNode} />;
            })}
          </div>
        </div>
      ) : null}
      <div className="hermes-lcm-section-head">
        <h4>{`Messages (${fmtInt(c.message_count || loadedMessages.length)})`}</h4>
        <div className="hermes-lcm-dim">
          {`Showing ${fmtInt(visibleMessages.length)} of ${fmtInt(c.message_count || loadedMessages.length)}`}
        </div>
      </div>
      <div className="hermes-lcm-stream">
        {visibleMessages.map(function (m: any) {
          return (
            <MessageItem
              key={m.store_id}
              m={m}
              onOpenMessage={onOpenMessage}
              active={props.activeMessageId != null && Number(props.activeMessageId) === Number(m.store_id)}
            />
          );
        })}
      </div>
      <div className="hermes-lcm-actions">
        {shownCount < loadedMessages.length ? (
          <button
            type="button"
            className="hermes-lcm-btn"
            onClick={function () { setPage(page + 1); }}
          >{`Show next ${SESSION_MESSAGE_PAGE_SIZE}`}</button>
        ) : null}
        {(shownCount >= loadedMessages.length && d.has_more) ? (
          <button
            type="button"
            className="hermes-lcm-btn"
            onClick={props.onLoadMore}
            disabled={props.loadingMore}
          >{props.loadingMore ? "Loading more…" : `Load ${SESSION_FETCH_BATCH} more`}</button>
        ) : null}
      </div>
    </div>
  );
}

// --- drawer (session / node / message detail) ------------------------------

export function Drawer(props: {
  open: boolean;
  title?: string;
  canBack?: boolean;
  onBack: () => void;
  onClose: () => void;
  children?: React.ReactNode;
}): React.ReactElement | null {
  const panelRef = useRef<any>(null);
  const returnFocusRef = useRef<any>(null);
  const wasOpenRef = useRef<boolean>(false);
  // Restore focus to the element that opened the drawer when it closes
  // (Escape/✕/overlay), instead of dropping focus to <body>. This effect is
  // declared before the panel-focus effect so it captures the trigger
  // element before the panel steals focus.
  useEffect(function () {
    if (props.open && !wasOpenRef.current) {
      const active = typeof document !== "undefined" ? document.activeElement : null;
      returnFocusRef.current = active && active !== document.body ? active : null;
    }
    if (!props.open && wasOpenRef.current) {
      const target = returnFocusRef.current;
      returnFocusRef.current = null;
      if (
        target &&
        typeof target.focus === "function" &&
        document.contains(target)
      ) {
        try {
          target.focus();
        } catch (e) {
          /* focus restoration is best-effort */
        }
      }
    }
    wasOpenRef.current = props.open;
  }, [props.open]);
  useEffect(function () {
    if (props.open && panelRef.current && typeof panelRef.current.focus === "function") {
      panelRef.current.focus();
    }
  }, [props.open, props.title]);
  if (!props.open) return null;
  return (
    <div className="hermes-lcm-drawer-overlay" onClick={props.onClose}>
      <div
        ref={panelRef}
        className="hermes-lcm-drawer"
        role="dialog"
        aria-modal="true"
        aria-label={props.title || "Detail"}
        tabIndex={-1}
        onClick={function (e) { e.stopPropagation(); }}
      >
        <div className="hermes-lcm-drawer-head">
          {props.canBack ? (
            <button
              className="hermes-lcm-btn"
              onClick={props.onBack}
              aria-label="Back to previous detail"
            >← Back</button>
          ) : null}
          <div className="hermes-lcm-drawer-title">{props.title}</div>
          <button
            className="hermes-lcm-btn"
            onClick={props.onClose}
            aria-label="Close detail (Escape)"
          >✕</button>
        </div>
        <div className="hermes-lcm-drawer-body">{props.children}</div>
      </div>
    </div>
  );
}

export function DrawerError(props: { kind?: string; message?: any; onRetry?: () => void }): React.ReactElement {
  const label = props.kind === "node" ? "node" : (props.kind === "message" ? "message" : "session");
  return (
    <div className="hermes-lcm-derror">
      <div className="hermes-lcm-derror-title">
        {"Couldn't load this " + label}
      </div>
      <div className="hermes-lcm-derror-msg">{String(props.message || "Request failed")}</div>
      {props.onRetry ? (
        <button
          className="hermes-lcm-btn hermes-lcm-derror-retry"
          onClick={props.onRetry}
        >↻ Retry</button>
      ) : null}
    </div>
  );
}
