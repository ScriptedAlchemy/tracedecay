(function () {
  "use strict";

  const SDK = window.__HERMES_PLUGIN_SDK__;
  if (!SDK) return;

  const { React } = SDK;
  const { useEffect, useMemo, useState, useCallback, useRef } = SDK.hooks;
  const h = React.createElement;
  const isoTimeAgo = (SDK.utils && SDK.utils.isoTimeAgo) || null;

  const API = "/api/plugins/hermes-lcm";

  function short(s, n) {
    const text = String(s || "");
    return text.length > n ? text.slice(0, n - 1) + "…" : text;
  }

  function fmtInt(n) {
    const v = Number(n) || 0;
    return v.toLocaleString();
  }

  function fmtTime(epoch) {
    const v = Number(epoch);
    if (!v) return "";
    try {
      const d = new Date(v * 1000);
      if (isoTimeAgo) return isoTimeAgo(d.toISOString());
      return d.toLocaleString();
    } catch (e) {
      return String(epoch);
    }
  }

  function fmtAbsoluteTime(epoch) {
    const v = Number(epoch);
    if (!v) return "";
    try {
      return new Date(v * 1000).toLocaleString();
    } catch (e) {
      return String(epoch);
    }
  }

  function escapeRegExp(s) {
    return String(s || "").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }

  function queryTerms(query) {
    return String(query || "")
      .trim()
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 8);
  }

  function renderHighlightedText(text, query) {
    const raw = String(text || "");
    const terms = queryTerms(query);
    if (!terms.length) return raw;
    const re = new RegExp("(" + terms.map(escapeRegExp).join("|") + ")", "ig");
    const parts = raw.split(re);
    return parts.map(function (part, idx) {
      if (!part) return null;
      return terms.some(function (term) { return part.toLowerCase() === term.toLowerCase(); })
        ? h("mark", { key: "hl" + idx, className: "hermes-lcm-mark" }, part)
        : part;
    });
  }

  function renderSearchSnippet(text, query) {
    const raw = String(text || "");
    if (/\[[^\]]+\]/.test(raw)) return renderSnippet(raw);
    return renderHighlightedText(raw, query);
  }

  function parseJsonArray(value) {
    if (Array.isArray(value)) return value;
    if (typeof value !== "string" || !value.trim()) return [];
    try {
      const parsed = JSON.parse(value);
      return Array.isArray(parsed) ? parsed : [];
    } catch (e) {
      return [];
    }
  }

  function copyTextValue(text) {
    const value = String(text == null ? "" : text);
    if (!value) return Promise.resolve(false);
    if (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(value).then(function () { return true; }).catch(function () {
        return false;
      });
    }
    return new Promise(function (resolve) {
      try {
        const ta = document.createElement("textarea");
        ta.value = value;
        ta.setAttribute("readonly", "readonly");
        ta.style.position = "absolute";
        ta.style.left = "-9999px";
        document.body.appendChild(ta);
        ta.select();
        const ok = document.execCommand("copy");
        document.body.removeChild(ta);
        resolve(ok);
      } catch (e) {
        resolve(false);
      }
    });
  }

  // Humanize fetch failures: the shell SDK's fetchJSON throws (TypeError
  // "Failed to fetch") when the server is unreachable. Failed fetches must
  // render error UI, never zero-data UI, so this string is shown prominently.
  function friendlyError(err) {
    const raw = String((err && err.message) || err || "Request failed");
    if (/failed to fetch|networkerror|load failed|network request/i.test(raw)) {
      return "Can't reach the tokensave server";
    }
    return raw;
  }

  // Append rows from a paginated follow-up fetch, de-duplicated by id, so
  // server-offset pagination can never double-render a row.
  function mergeRows(prevRows, nextRows, idKey) {
    const seen = {};
    const merged = (prevRows || []).slice();
    merged.forEach(function (row) { seen[String(row[idKey])] = true; });
    (nextRows || []).forEach(function (row) {
      const key = String(row[idKey]);
      if (!seen[key]) {
        seen[key] = true;
        merged.push(row);
      }
    });
    return merged;
  }

  // Merge a follow-up search page into the previous payload: scalar fields
  // (totals, engine, …) come from the newest response, match rows append with
  // id-dedupe. Pure so the pagination merge stays unit-testable.
  function mergeSearchPayload(prev, json) {
    if (!prev) return json;
    const prevMatches = prev.matches || {};
    const nextMatches = (json && json.matches) || {};
    return Object.assign({}, prev, json, {
      matches: {
        messages: mergeRows(prevMatches.messages, nextMatches.messages, "store_id"),
        summary_nodes: mergeRows(prevMatches.summary_nodes, nextMatches.summary_nodes, "node_id"),
      },
    });
  }

  // Flatten markdown to readable plain text (for compact list previews/titles).
  function stripMd(s) {
    return String(s == null ? "" : s)
      .replace(/```[\s\S]*?```/g, " ")
      .replace(/`([^`]+)`/g, "$1")
      .replace(/\*\*([^*]+)\*\*/g, "$1")
      .replace(/\*([^*]+)\*/g, "$1")
      .replace(/^#{1,6}\s+/gm, "")
      .replace(/^\s*>\s?/gm, "")
      .replace(/^\s*[-*+]\s+/gm, "")
      .replace(/\[([^\]]+)\]\([^)\s]+\)/g, "$1")
      .replace(/\s+/g, " ")
      .trim();
  }

  // Derive a short title from a summary: first heading, else first bold run,
  // else the first sentence/line of the flattened text.
  function summaryTitle(s) {
    const txt = String(s == null ? "" : s);
    const hd = txt.match(/^\s*#{1,6}\s+(.+?)\s*$/m);
    if (hd) return stripMd(hd[1]);
    const bold = txt.match(/\*\*([^*]+)\*\*/);
    if (bold) return stripMd(bold[1]);
    const flat = stripMd(txt);
    const dot = flat.search(/[.!?](\s|$)/);
    return dot > 12 && dot < 90 ? flat.slice(0, dot + 1) : flat;
  }

  // Pretty session label from an id like "20260529_011608_ab12cd".
  function sessionLabel(id) {
    const txt = String(id == null ? "" : id);
    const m = txt.match(/^(\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})/);
    if (m) return `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]}`;
    return short(txt, 36);
  }

  function sessionTail(id) {
    const txt = String(id == null ? "" : id);
    const m = txt.match(/_([0-9a-f]{4,})$/i);
    return m ? m[1] : "";
  }

  // --- snippet rendering: backend wraps highlights in [ ... ] ----------------
  function renderSnippet(text) {
    const raw = String(text || "");
    const parts = [];
    const re = /\[([^\]]*)\]/g;
    let last = 0;
    let m;
    let i = 0;
    while ((m = re.exec(raw)) !== null) {
      if (m.index > last) parts.push(raw.slice(last, m.index));
      parts.push(h("mark", { key: "mk" + i++, className: "hermes-lcm-mark" }, m[1]));
      last = re.lastIndex;
    }
    if (last < raw.length) parts.push(raw.slice(last));
    return parts.length ? parts : raw;
  }

  // --- minimal self-contained markdown -> React. XSS-safe: builds elements,
  // never uses innerHTML. Underscores are left literal so snake_case and paths
  // (kanban_block, auto_model_routing) are not mangled into emphasis. ---------
  function mdInlineNodes(text, kp) {
    const nodes = [];
    const codeRe = /`([^`]+)`/g;
    let last = 0, m, i = 0;
    while ((m = codeRe.exec(text)) !== null) {
      if (m.index > last) mdEmphasis(text.slice(last, m.index), nodes, kp + "t" + i);
      nodes.push(h("code", { key: kp + "c" + i, className: "hermes-lcm-md-code" }, m[1]));
      last = codeRe.lastIndex; i++;
    }
    if (last < text.length) mdEmphasis(text.slice(last), nodes, kp + "t" + i);
    return nodes;
  }

  function mdEmphasis(str, nodes, kp) {
    const re = /(\*\*)([\s\S]+?)\*\*|(\*)([^*\n]+?)\*|\[([^\]]+)\]\(([^)\s]+)\)/;
    let rest = str, i = 0, m;
    while ((m = re.exec(rest)) !== null) {
      if (m.index > 0) nodes.push(rest.slice(0, m.index));
      if (m[1]) nodes.push(h("strong", { key: kp + "b" + i }, mdInlineNodes(m[2], kp + "b" + i + "-")));
      else if (m[3]) nodes.push(h("em", { key: kp + "e" + i }, mdInlineNodes(m[4], kp + "e" + i + "-")));
      else nodes.push(h("a", {
        key: kp + "a" + i, href: m[6], target: "_blank", rel: "noopener noreferrer",
        className: "hermes-lcm-md-link",
      }, m[5]));
      rest = rest.slice(m.index + m[0].length); i++;
    }
    if (rest) nodes.push(rest);
  }

  function mdBuildList(items, kp) {
    const base = items[0].indent;
    const ordered = items[0].ordered;
    const children = [];
    let i = 0, li = 0;
    while (i < items.length) {
      if (items[i].indent > base) {
        const start = i;
        while (i < items.length && items[i].indent > base) i++;
        const nested = mdBuildList(items.slice(start), kp + "n" + li);
        if (children.length) {
          const prev = children[children.length - 1];
          children[children.length - 1] = h("li", { key: prev.key },
            [].concat(prev.props.children, nested));
        } else {
          children.push(h("li", { key: kp + "li" + li++ }, nested));
        }
        continue;
      }
      children.push(h("li", { key: kp + "li" + li++ }, mdInlineNodes(items[i].text, kp + "x" + li)));
      i++;
    }
    return h(ordered ? "ol" : "ul", { key: kp, className: "hermes-lcm-md-list" }, children);
  }

  function mdToReact(src) {
    const lines = String(src == null ? "" : src).replace(/\r\n?/g, "\n").split("\n");
    const blocks = [];
    let i = 0, key = 0;
    while (i < lines.length) {
      const line = lines[i];
      if (/^\s*```/.test(line)) {
        const buf = [];
        i++;
        while (i < lines.length && !/^\s*```/.test(lines[i])) { buf.push(lines[i]); i++; }
        i++;
        blocks.push(h("pre", { key: "p" + key++, className: "hermes-lcm-md-pre" },
          h("code", null, buf.join("\n"))));
        continue;
      }
      if (/^\s*$/.test(line)) { i++; continue; }
      const hd = line.match(/^(#{1,6})\s+(.*)$/);
      if (hd) {
        blocks.push(h("div", {
          key: "p" + key++,
          className: "hermes-lcm-md-h hermes-lcm-md-h" + hd[1].length,
        }, mdInlineNodes(hd[2], "h" + key)));
        i++; continue;
      }
      if (/^\s*>\s?/.test(line)) {
        const buf = [];
        while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
          buf.push(lines[i].replace(/^\s*>\s?/, "")); i++;
        }
        blocks.push(h("blockquote", { key: "p" + key++, className: "hermes-lcm-md-quote" },
          mdInlineNodes(buf.join(" "), "q" + key)));
        continue;
      }
      if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
        const items = [];
        while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
          const mm = lines[i].match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/);
          items.push({ indent: mm[1].length, ordered: /\d/.test(mm[2]), text: mm[3] });
          i++;
        }
        blocks.push(mdBuildList(items, "l" + key++));
        continue;
      }
      const buf = [];
      while (i < lines.length && !/^\s*$/.test(lines[i])
        && !/^\s*```/.test(lines[i])
        && !/^(#{1,6})\s+/.test(lines[i])
        && !/^\s*>\s?/.test(lines[i])
        && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
        buf.push(lines[i]); i++;
      }
      const kids = [];
      buf.forEach(function (ln, idx) {
        if (idx) kids.push(h("br", { key: "br" + idx }));
        const sub = mdInlineNodes(ln, "p" + key + "-" + idx);
        for (let s = 0; s < sub.length; s++) kids.push(sub[s]);
      });
      blocks.push(h("p", { key: "p" + key++, className: "hermes-lcm-md-p" }, kids));
    }
    return blocks;
  }

  function MarkdownText(props) {
    const text = String(props.text == null ? "" : props.text);
    let nodes;
    try { nodes = mdToReact(text); } catch (e) { nodes = [text]; }
    return h("div", {
      className: "hermes-lcm-md" + (props.className ? " " + props.className : ""),
    }, nodes);
  }

  function BarList(props) {
    const rows = props.rows || [];
    const keyName = props.keyName;
    const onPick = props.onPick;
    const total = rows.reduce((acc, row) => acc + (Number(row.count) || 0), 0) || 1;
    if (!rows.length) return h("div", { className: "hermes-lcm-empty" }, "No data");
    return h("div", { className: "hermes-lcm-bars" }, rows.map(function (row, idx) {
      const label = String(row[keyName] == null ? "(none)" : row[keyName]);
      const count = Number(row.count) || 0;
      const pct = Math.max(2, Math.round((count / total) * 100));
      const clickable = typeof onPick === "function";
      return h("div", {
        key: label + ":" + idx,
        className: "hermes-lcm-bar-row" + (clickable ? " hermes-lcm-clk" : ""),
        onClick: clickable ? function () { onPick(label); } : undefined,
      }, [
        h("div", { className: "hermes-lcm-bar-head" }, [
          h("span", { className: "hermes-lcm-k" }, label),
          h("span", { className: "hermes-lcm-v" }, fmtInt(count)),
        ]),
        h("div", { className: "hermes-lcm-bar-track" }, [
          h("div", { className: "hermes-lcm-bar-fill", style: { width: pct + "%" } }),
        ]),
      ]);
    }));
  }

  // --- inline SVG: message-volume timeline ----------------------------------
  // Responsive CSS bar chart (no SVG stretching, so bars stay crisp and the
  // summary markers render as true round dots regardless of bucket count).
  function TimelineChart(props) {
    // Buckets cover only messages with real timestamps; the server reports
    // messages without one separately so they surface as an honest note
    // instead of a fake single bar.
    const buckets = (props.buckets || []).filter(function (b) { return b && b.bucket != null; });
    const nodeBuckets = props.nodeBuckets || [];
    const undatedCount = Number(props.undatedCount) || 0;
    if (!buckets.length) {
      return h("div", { className: "hermes-lcm-empty" }, undatedCount > 0
        ? `No dated messages yet — ${fmtInt(undatedCount)} stored messages have no timestamp`
        : "No timeline data");
    }
    const maxCount = buckets.reduce((acc, b) => Math.max(acc, Number(b.count) || 0), 0) || 1;
    const nodeByBucket = {};
    nodeBuckets.forEach(function (nb) { nodeByBucket[nb.bucket] = Number(nb.count) || 0; });

    const cols = buckets.map(function (b, i) {
      const count = Number(b.count) || 0;
      const pct = count > 0 ? Math.max(3, Math.round((count / maxCount) * 100)) : 0;
      const nodes = nodeByBucket[b.bucket] || 0;
      const tip = `${b.bucket}: ${fmtInt(count)} messages`
        + (nodes ? ` · ${fmtInt(nodes)} summaries` : "");
      return h("div", { key: b.bucket + i, className: "hermes-lcm-tl-col", title: tip }, [
        h("div", { className: "hermes-lcm-tl-dot" + (nodes ? " hermes-lcm-tl-dot-on" : "") }),
        h("div", { className: "hermes-lcm-tl-bar", style: { height: pct + "%" } }),
      ]);
    });

    return h("div", { className: "hermes-lcm-tl" }, [
      h("div", { className: "hermes-lcm-tl-bars" }, cols),
      h("div", { className: "hermes-lcm-svg-axis" }, [
        h("span", null, short(buckets[0].bucket, 16)),
        h("span", null, short(buckets[buckets.length - 1].bucket, 16)),
      ]),
      undatedCount > 0 ? h("div", { className: "hermes-lcm-dim hermes-lcm-tl-undated" },
        `${fmtInt(undatedCount)} undated messages not shown`) : null,
    ]);
  }

  // --- inline SVG: per-group compression (kept vs saved) --------------------
  function CompressionBars(props) {
    const groups = props.groups || [];
    const onPick = props.onPick;
    if (!groups.length) return h("div", { className: "hermes-lcm-empty" }, "No compression data");
    const maxSrc = groups.reduce((acc, g) => Math.max(acc, Number(g.source_token_count) || 0), 0) || 1;
    return h("div", { className: "hermes-lcm-comp" }, groups.map(function (g, idx) {
      const src = Number(g.source_token_count) || 0;
      const out = Number(g.token_count) || 0;
      const totalW = Math.max(0.5, (src / maxSrc) * 100);
      const keptW = src > 0 ? (out / src) * totalW : 0;
      const sid = g.session_id != null ? g.session_id : g.key;
      const label = (typeof sid === "string" && /^\d{8}_/.test(sid))
        ? sessionLabel(sid)
        : (g.depth != null ? `node #${g.key} (D${g.depth})` : String(g.key));
      const clickable = typeof onPick === "function";
      return h("div", {
        key: String(g.key) + idx,
        className: "hermes-lcm-comp-row" + (clickable ? " hermes-lcm-clk" : ""),
        onClick: clickable ? function () { onPick(g); } : undefined,
      }, [
        h("div", { className: "hermes-lcm-comp-head" }, [
          h("span", { className: "hermes-lcm-k" }, label),
          h("span", { className: "hermes-lcm-v" }, `${g.ratio || 0}× · ${fmtInt(src)}→${fmtInt(out)}`),
        ]),
        h("svg", {
          viewBox: "0 0 100 8", preserveAspectRatio: "none",
          width: "100%", height: 8, className: "hermes-lcm-svgbar",
        }, [
          h("rect", { x: 0, y: 0, width: totalW, height: 8, rx: 1.5, className: "hermes-lcm-svg-saved" }),
          h("rect", { x: 0, y: 0, width: keptW, height: 8, rx: 1.5, className: "hermes-lcm-svg-kept" }),
        ]),
      ]);
    }));
  }

  function Stat(props) {
    return h("div", { className: "hermes-lcm-stat" }, [
      h("div", { className: "hermes-lcm-stat-v" }, props.value),
      h("div", { className: "hermes-lcm-stat-k" }, props.label),
    ]);
  }

  // --- pretty tool-result rendering. Known tools get bespoke components; any
  // other JSON falls back to a clean key/value view; non-JSON to markdown. ----
  // Tool results are often a JSON value followed by a trailing human note
  // (e.g. `{...}\n[use offset=120 to see more]`), so strict JSON.parse fails.
  // Extract the leading {...}/[...] by brace-matching, keep the rest as a note.
  function parseLeadingJSON(s) {
    if (typeof s !== "string") return null;
    let i = 0;
    while (i < s.length && /\s/.test(s[i])) i++;
    const open = s[i];
    if (open !== "{" && open !== "[") {
      try { return { value: JSON.parse(s), rest: "" }; } catch (e) { return null; }
    }
    const close = open === "{" ? "}" : "]";
    let depth = 0, inStr = false, esc = false, end = -1;
    for (let j = i; j < s.length; j++) {
      const ch = s[j];
      if (inStr) {
        if (esc) esc = false;
        else if (ch === "\\") esc = true;
        else if (ch === '"') inStr = false;
        continue;
      }
      if (ch === '"') inStr = true;
      else if (ch === open) depth++;
      else if (ch === close) { depth--; if (depth === 0) { end = j + 1; break; } }
    }
    if (end === -1) return null;
    try { return { value: JSON.parse(s.slice(i, end)), rest: s.slice(end).trim() }; }
    catch (e) { return null; }
  }

  function clampText(s, n) {
    const t = String(s == null ? "" : s);
    return t.length > n ? t.slice(0, n) + "\n…(" + fmtInt(t.length - n) + " more chars)" : t;
  }

  function codeBlock(text) {
    return h("pre", { className: "hermes-lcm-md-pre" }, h("code", null, clampText(text, 4000)));
  }

  function toolBadge(label, kind) {
    return h("span", { className: "hermes-lcm-tag" + (kind ? " hermes-lcm-tag-" + kind : "") }, label);
  }

  function ToolOutput(d) {
    const out = d.output != null
      ? d.output
      : (typeof d.result === "string" ? d.result
        : (d.result != null ? JSON.stringify(d.result, null, 2) : ""));
    const code = d.exit_code != null ? d.exit_code : d.status;
    const ok = d.exit_code === 0 || d.status === "success" || d.status === "exited" || d.success === true;
    return h("div", { className: "hermes-lcm-tool" }, [
      (code != null || d.duration_seconds != null) ? h("div", { className: "hermes-lcm-tool-meta" }, [
        code != null ? toolBadge((d.exit_code != null ? "exit " : "") + code, ok ? "ok" : "bad") : null,
        d.duration_seconds != null ? h("span", { className: "hermes-lcm-dim" }, d.duration_seconds + "s") : null,
      ]) : null,
      out ? codeBlock(out) : null,
      d.error ? h("div", { className: "hermes-lcm-tool-err" }, String(d.error)) : null,
      d.timeout_note ? h("div", { className: "hermes-lcm-tool-err" }, String(d.timeout_note)) : null,
    ]);
  }

  function ToolReadFile(d) {
    return h("div", { className: "hermes-lcm-tool" }, [
      h("div", { className: "hermes-lcm-tool-meta" }, [
        d.total_lines != null ? toolBadge(fmtInt(d.total_lines) + " lines") : null,
        d.file_size != null ? toolBadge(fmtInt(d.file_size) + " B") : null,
        d.truncated ? toolBadge("truncated", "warn") : null,
        d.is_image ? toolBadge("image") : null,
      ]),
      d.content ? codeBlock(d.content) : null,
      (d.hint || d._hint) ? h("div", { className: "hermes-lcm-dim" }, String(d.hint || d._hint)) : null,
    ]);
  }

  function ToolSearchFiles(d) {
    const matches = d.matches || [];
    return h("div", { className: "hermes-lcm-tool" }, [
      h("div", { className: "hermes-lcm-tool-meta" }, [
        toolBadge(fmtInt(d.total_count != null ? d.total_count : matches.length) + " matches"),
      ]),
      h("div", { className: "hermes-lcm-tool-matches" }, matches.slice(0, 50).map(function (mm, i) {
        return h("div", { key: i, className: "hermes-lcm-tool-match" }, [
          h("div", { className: "hermes-lcm-tool-match-loc" }, [
            h("span", { className: "hermes-lcm-tool-path" }, short(String(mm.path || ""), 72)),
            mm.line != null ? h("span", { className: "hermes-lcm-dim" }, ":" + mm.line) : null,
          ]),
          mm.content != null
            ? h("code", { className: "hermes-lcm-tool-match-code" }, short(String(mm.content), 220))
            : null,
        ]);
      })),
      matches.length > 50 ? h("div", { className: "hermes-lcm-dim" }, "+" + fmtInt(matches.length - 50) + " more") : null,
    ]);
  }

  const TODO_ICON = { completed: "✓", in_progress: "◐", pending: "○", cancelled: "✗" };
  function ToolTodo(d) {
    const todos = d.todos || [];
    const s = d.summary || {};
    return h("div", { className: "hermes-lcm-tool" }, [
      h("div", { className: "hermes-lcm-tool-meta" }, [
        s.completed != null ? toolBadge(s.completed + " done", "ok") : null,
        s.in_progress != null ? toolBadge(s.in_progress + " active") : null,
        s.pending != null ? toolBadge(s.pending + " todo") : null,
        s.cancelled ? toolBadge(s.cancelled + " cancelled") : null,
      ]),
      h("ul", { className: "hermes-lcm-todo" }, todos.map(function (t, i) {
        const st = String(t.status || "pending");
        return h("li", { key: t.id || i, className: "hermes-lcm-todo-item hermes-lcm-todo-" + st }, [
          h("span", { className: "hermes-lcm-todo-ic" }, TODO_ICON[st] || "•"),
          h("span", null, String(t.content || "")),
        ]);
      })),
    ]);
  }

  function ToolPatch(d, raw) {
    let diff = d && d.diff;
    if (diff == null && typeof raw === "string") {
      const mm = raw.match(/"diff"\s*:\s*"([\s\S]*?)"\s*\}?\s*$/);
      diff = mm ? mm[1].replace(/\\n/g, "\n").replace(/\\"/g, '"').replace(/\\t/g, "\t") : raw;
    }
    const lines = String(diff || "").split("\n");
    return h("div", { className: "hermes-lcm-tool" }, [
      (d && d.success != null) ? h("div", { className: "hermes-lcm-tool-meta" }, [
        toolBadge(d.success ? "applied" : "failed", d.success ? "ok" : "bad"),
      ]) : null,
      h("pre", { className: "hermes-lcm-md-pre hermes-lcm-diff" }, lines.slice(0, 240).map(function (ln, i) {
        const c0 = ln.charAt(0);
        const cls = c0 === "+" ? "hermes-lcm-diff-add"
          : c0 === "-" ? "hermes-lcm-diff-del"
          : c0 === "@" ? "hermes-lcm-diff-hunk" : "";
        return h("div", { key: i, className: cls }, ln || " ");
      })),
    ]);
  }

  function ToolSkill(d) {
    const tags = d.tags || [];
    return h("div", { className: "hermes-lcm-tool" }, [
      h("div", { className: "hermes-lcm-tool-meta" }, [
        d.name ? toolBadge(d.name) : null,
        tags.slice(0, 6).map(function (t, i) { return h("span", { key: i, className: "hermes-lcm-dim" }, "#" + t); }),
      ]),
      d.description ? h("div", { className: "hermes-lcm-dim" }, String(d.description)) : null,
      d.content ? h(MarkdownText, { text: clampText(d.content, 4000) }) : null,
    ]);
  }

  function ToolGeneric(d) {
    if (Array.isArray(d)) return codeBlock(JSON.stringify(d, null, 2));
    return h("div", { className: "hermes-lcm-kv" }, Object.keys(d).map(function (k, i) {
      const v = d[k];
      let vn;
      if (v == null) vn = h("span", { className: "hermes-lcm-dim" }, "null");
      else if (typeof v === "object") {
        vn = h("pre", { className: "hermes-lcm-md-pre" },
          h("code", null, clampText(JSON.stringify(v, null, 2), 1500)));
      } else vn = h("span", null, String(v));
      return h("div", { key: k + i, className: "hermes-lcm-kv-row" }, [
        h("span", { className: "hermes-lcm-kv-k" }, k),
        h("span", { className: "hermes-lcm-kv-v" }, vn),
      ]);
    }));
  }

  function ToolResult(props) {
    const name = String(props.name || "");
    const raw = props.content;
    const parsed = parseLeadingJSON(raw);
    const data = parsed ? parsed.value : undefined;
    const note = (parsed && parsed.rest) ? parsed.rest : "";
    let body = null;
    try {
      if ((name === "terminal" || name === "process" || name === "execute_code" || name === "shell")
        && data && typeof data === "object") body = ToolOutput(data);
      else if (name === "read_file" && data) body = ToolReadFile(data);
      else if (name === "search_files" && data) body = ToolSearchFiles(data);
      else if (name === "todo" && data) body = ToolTodo(data);
      else if (name === "patch") body = ToolPatch(data, raw);
      else if (name === "skill_view" && data) body = ToolSkill(data);
      else if (data && typeof data === "object") body = ToolGeneric(data);
    } catch (e) { body = null; }
    if (body == null) {
      return h(MarkdownText, { className: "hermes-lcm-msg-body", text: short(String(raw == null ? "" : raw), 4000) });
    }
    if (note) {
      return h("div", { className: "hermes-lcm-tool" }, [
        body,
        h("div", { className: "hermes-lcm-dim hermes-lcm-tool-note" }, short(note, 400)),
      ]);
    }
    return body;
  }

  // --- detail drawer (session / node / message) -----------------------------
  const SEARCH_FETCH_LIMIT = 120;
  const SEARCH_PAGE_SIZE = 6;
  const SESSION_FETCH_BATCH = 60;
  const SESSION_MESSAGE_PAGE_SIZE = 8;

  function TimeText(props) {
    if (!props.epoch) return h("span", { className: props.className || "hermes-lcm-dim" }, "—");
    const absolute = fmtAbsoluteTime(props.epoch);
    let dateTime = "";
    try {
      dateTime = new Date(Number(props.epoch) * 1000).toISOString();
    } catch (e) {}
    return h("time", {
      className: props.className || "hermes-lcm-dim",
      title: absolute,
      dateTime: dateTime || undefined,
    }, fmtTime(props.epoch));
  }

  function CopyButton(props) {
    const [status, setStatus] = useState("idle");
    const onCopy = useCallback(function (e) {
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
    return h("button", {
      type: "button",
      className: "hermes-lcm-btn hermes-lcm-copy" + (status === "copied" ? " is-copied" : ""),
      onClick: onCopy,
      title: props.title || "Copy to clipboard",
    }, label);
  }

  function SkeletonLines(props) {
    const count = props.count || 3;
    const lines = [];
    for (let i = 0; i < count; i++) {
      lines.push(h("div", {
        key: "sk" + i,
        className: "hermes-lcm-skel-line",
        style: props.widths && props.widths[i] ? { width: props.widths[i] } : undefined,
      }));
    }
    return h("div", { className: "hermes-lcm-skeleton-block" }, lines);
  }

  function Pager(props) {
    if (!props.totalPages || props.totalPages <= 1) return null;
    return h("div", { className: "hermes-lcm-pager" }, [
      h("button", {
        type: "button",
        className: "hermes-lcm-btn",
        disabled: props.page <= 1,
        onClick: function () { props.onChange(props.page - 1); },
      }, "Prev"),
      h("span", { className: "hermes-lcm-pager-status" }, `Page ${props.page} / ${props.totalPages}`),
      h("button", {
        type: "button",
        className: "hermes-lcm-btn",
        disabled: props.page >= props.totalPages,
        onClick: function () { props.onChange(props.page + 1); },
      }, "Next"),
    ]);
  }

  function SearchResultCard(props) {
    const item = props.item;
    const isMessage = props.kind === "message";
    const title = isMessage
      ? short(item.session_id || "", 42)
      : short(summaryTitle(item.summary || ""), 90);
    const preview = isMessage
      ? renderSearchSnippet(item.snippet || short(item.content, 240), props.query)
      : renderSearchSnippet(item.snippet || short(stripMd(item.summary), 240), props.query);
    const keyValue = props.kind + ":" + String(isMessage ? item.store_id : item.node_id);
    return h("button", {
      type: "button",
      ref: props.resultRef,
      className: "hermes-lcm-result hermes-lcm-result-btn" + (props.selected ? " hermes-lcm-selected" : ""),
      onClick: props.onOpen,
      onFocus: props.onFocus,
      onMouseEnter: props.onFocus,
    }, [
      h("div", { className: "hermes-lcm-row-meta" }, [
        h("span", { className: "hermes-lcm-pill hermes-lcm-pill-accent" },
          isMessage ? (item.role || "message") : ("D" + item.depth)),
        !isMessage && item.category ? h("span", { className: "hermes-lcm-pill" }, item.category) : null,
        isMessage && item.source ? h("span", { className: "hermes-lcm-pill" }, item.source) : null,
        isMessage && item.tool_name ? h("span", { className: "hermes-lcm-pill" }, short(item.tool_name, 24)) : null,
        h("span", { className: "hermes-lcm-dim" }, keyValue),
        isMessage
          ? h(TimeText, { className: "hermes-lcm-dim", epoch: item.timestamp })
          : h(TimeText, { className: "hermes-lcm-dim", epoch: item.latest_at || item.created_at }),
      ]),
      h("div", { className: "hermes-lcm-row-title" }, title),
      h("div", { className: "hermes-lcm-msg-body" }, preview),
      h("div", { className: "hermes-lcm-result-foot" }, [
        h("span", { className: "hermes-lcm-dim" },
          isMessage
            ? `${fmtInt(item.token_estimate)} tok · ${sessionLabel(item.session_id)}`
            : `${fmtInt(item.source_token_count)}→${fmtInt(item.token_count)} tok · ${sessionLabel(item.session_id)}`),
        h("span", { className: "hermes-lcm-dim" }, isMessage ? "Open full message" : "Open source links"),
      ]),
    ]);
  }

  function MessageItem(props) {
    const m = props.m;
    const clickable = typeof props.onOpenMessage === "function";
    let body;
    if (props.previewText) {
      body = h("div", { className: "hermes-lcm-msg-body" }, renderSearchSnippet(props.previewText, props.query));
    } else if (m.role === "tool") {
      body = h(ToolResult, { name: m.tool_name, content: m.content });
    } else {
      body = h(MarkdownText, {
        className: "hermes-lcm-msg-body",
        text: props.compact ? short(m.content, 900) : String(m.content || ""),
      });
    }
    return h("div", {
      className: "hermes-lcm-msg" + (clickable ? " hermes-lcm-clk" : "") + (props.active ? " hermes-lcm-selected" : ""),
      onClick: clickable ? function () { props.onOpenMessage(m); } : undefined,
    }, [
      h("div", { className: "hermes-lcm-msg-head" }, [
        h("div", { className: "hermes-lcm-msg-meta" }, [
          h("span", { className: "hermes-lcm-tag" }, m.role || "?"),
          m.source ? h("span", { className: "hermes-lcm-tag hermes-lcm-tag-src" }, m.source) : null,
          m.tool_name ? h("span", { className: "hermes-lcm-tag" }, m.tool_name) : null,
          m.pinned ? h("span", { className: "hermes-lcm-tag" }, "pinned") : null,
          m.store_id != null ? h("span", { className: "hermes-lcm-dim" }, "#" + m.store_id) : null,
          h(TimeText, { className: "hermes-lcm-dim", epoch: m.timestamp }),
          m.token_estimate ? h("span", { className: "hermes-lcm-dim" }, `${fmtInt(m.token_estimate)} tok`) : null,
        ]),
        h("div", { className: "hermes-lcm-msg-actions" }, [
          clickable ? h("button", {
            type: "button",
            className: "hermes-lcm-btn",
            "aria-label": "Open message #" + m.store_id,
            onClick: function (e) { e.stopPropagation(); props.onOpenMessage(m); },
          }, "Open") : null,
          h(CopyButton, { text: m.content, label: "Copy", title: "Copy message content" }),
        ]),
      ]),
      body,
    ]);
  }

  function NodeRef(props) {
    const n = props.n;
    const onOpen = props.onOpen;
    return h("button", {
      type: "button",
      className: "hermes-lcm-noderef hermes-lcm-clk" + (props.active ? " hermes-lcm-selected" : ""),
      onClick: function () { onOpen(n.node_id); },
    }, [
      h("div", { className: "hermes-lcm-msg-meta" }, [
        h("span", { className: "hermes-lcm-tag" }, `D${n.depth}`),
        n.category ? h("span", { className: "hermes-lcm-tag" }, n.category) : null,
        h("span", { className: "hermes-lcm-dim" }, `#${n.node_id}`),
        n.source_type ? h("span", { className: "hermes-lcm-dim" }, n.source_type) : null,
        (n.token_count != null) ? h("span", { className: "hermes-lcm-dim" }, `${fmtInt(n.token_count)} tok`) : null,
        h(TimeText, { className: "hermes-lcm-dim", epoch: n.latest_at || n.created_at }),
      ]),
      h("div", { className: "hermes-lcm-msg-body" }, short(stripMd(n.summary), 320)),
    ]);
  }

  function MessageDetail(props) {
    const d = props.data || {};
    const message = d.message;
    const session = d.session;
    if (!message) return h("div", { className: "hermes-lcm-empty" }, "Message not found");
    const sessionNodes = (session && session.summary_nodes) || [];
    // Prefer the backend's exact message→summary linkage (summary_node_ids,
    // additive field) and fall back to same-session summaries when absent.
    const linkedIds = parseJsonArray(message.summary_node_ids).map(String);
    const linkedSet = {};
    linkedIds.forEach(function (id) { linkedSet[id] = true; });
    const linkedNodes = linkedIds.length
      ? sessionNodes.filter(function (node) { return linkedSet[String(node.node_id)]; })
      : [];
    const hasExactLinks = linkedIds.length > 0;
    const relatedNodes = hasExactLinks ? linkedNodes : sessionNodes;
    const unresolvedLinkIds = hasExactLinks
      ? linkedIds.filter(function (id) {
          return !relatedNodes.some(function (node) { return String(node.node_id) === id; });
        })
      : [];
    return h("div", { className: "hermes-lcm-detail" }, [
      h("div", { className: "hermes-lcm-detail-meta" }, [
        h("span", { className: "hermes-lcm-tag" }, message.role || "message"),
        message.source ? h("span", { className: "hermes-lcm-tag hermes-lcm-tag-src" }, message.source) : null,
        h("span", { className: "hermes-lcm-tag" }, `#${message.store_id}`),
        h(TimeText, { className: "hermes-lcm-dim", epoch: message.timestamp }),
        message.token_estimate ? h("span", { className: "hermes-lcm-dim" }, `${fmtInt(message.token_estimate)} tok`) : null,
        h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          onClick: function () { props.onOpenSession(message.session_id, { activeMessageId: message.store_id }); },
        }, sessionLabel(message.session_id)),
      ]),
      h("div", { className: "hermes-lcm-msg-actions" }, [
        h(CopyButton, { text: message.content, label: "Copy message", title: "Copy full message content" }),
      ]),
      h("div", { className: "hermes-lcm-detail-grid" }, [
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Session"),
          h("div", { className: "hermes-lcm-detail-v" }, short(message.session_id, 48)),
        ]),
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Absolute time"),
          h("div", { className: "hermes-lcm-detail-v" }, fmtAbsoluteTime(message.timestamp) || "—"),
        ]),
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Token estimate"),
          h("div", { className: "hermes-lcm-detail-v" }, fmtInt(message.token_estimate || 0)),
        ]),
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, hasExactLinks ? "Linked summaries" : "Related summaries"),
          h("div", { className: "hermes-lcm-detail-v" },
            fmtInt(hasExactLinks ? linkedIds.length : relatedNodes.length)),
        ]),
      ]),
      h("h4", null, "Content"),
      h(MessageItem, { m: message }),
      h("h4", null, hasExactLinks
        ? `Summaries built from this message (${linkedIds.length})`
        : `Summaries in this session (${relatedNodes.length})`),
      relatedNodes.length
        ? h("div", { className: "hermes-lcm-stream" }, relatedNodes.map(function (node) {
            return h(NodeRef, {
              key: node.node_id,
              n: node,
              onOpen: props.onOpenNode,
            });
          }))
        : null,
      unresolvedLinkIds.length ? h("div", { className: "hermes-lcm-stream" }, unresolvedLinkIds.map(function (id) {
        return h("button", {
          key: "link:" + id,
          type: "button",
          className: "hermes-lcm-noderef hermes-lcm-clk",
          onClick: function () { props.onOpenNode(id); },
        }, [
          h("div", { className: "hermes-lcm-msg-meta" }, [
            h("span", { className: "hermes-lcm-tag" }, "summary"),
            h("span", { className: "hermes-lcm-dim" }, "#" + id),
          ]),
          h("div", { className: "hermes-lcm-msg-body" }, "Open linked summary node"),
        ]);
      })) : null,
      (!relatedNodes.length && !unresolvedLinkIds.length)
        ? h("div", { className: "hermes-lcm-empty" },
            "No summary nodes reference this message yet.")
        : null,
    ]);
  }

  function NodeDetail(props) {
    const d = props.data || {};
    const node = d.node;
    const onOpenNode = props.onOpenNode;
    const onOpenSession = props.onOpenSession;
    const onOpenMessage = props.onOpenMessage;
    if (!node) return h("div", { className: "hermes-lcm-empty" }, "Node not found");
    const sources = d.sources || {};
    const tags = parseJsonArray(node.tags);
    const entities = parseJsonArray(node.entities);
    return h("div", { className: "hermes-lcm-detail" }, [
      h("div", { className: "hermes-lcm-detail-meta" }, [
        h("span", { className: "hermes-lcm-tag" }, `Depth ${node.depth}`),
        node.category ? h("span", { className: "hermes-lcm-tag" }, node.category) : null,
        h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          onClick: function () { onOpenSession(node.session_id); },
        }, sessionLabel(node.session_id)),
        h("span", { className: "hermes-lcm-dim" }, `${fmtInt(node.source_token_count)}→${fmtInt(node.token_count)} tok`),
        h(TimeText, { className: "hermes-lcm-dim", epoch: node.latest_at || node.created_at }),
      ]),
      h("div", { className: "hermes-lcm-detail-grid" }, [
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Node id"),
          h("div", { className: "hermes-lcm-detail-v" }, String(node.node_id)),
        ]),
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Source type"),
          h("div", { className: "hermes-lcm-detail-v" }, sources.type || node.source_type || "—"),
        ]),
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Tags"),
          h("div", { className: "hermes-lcm-detail-v" }, tags.length ? tags.join(", ") : "—"),
        ]),
        h("div", { className: "hermes-lcm-detail-cell" }, [
          h("div", { className: "hermes-lcm-detail-k" }, "Entities"),
          h("div", { className: "hermes-lcm-detail-v" }, entities.length ? entities.join(", ") : "—"),
        ]),
      ]),
      h("h4", null, "Summary"),
      h(MarkdownText, { className: "hermes-lcm-summary", text: node.summary }),
      node.expand_hint ? h("div", { className: "hermes-lcm-hint" }, [
        h("strong", null, "Expand hint: "), node.expand_hint,
      ]) : null,
      h("h4", null, `Source links (${sources.type || "?"}, ${(sources.ids || []).length})`),
      (function () {
        const isNodes = sources.type === "nodes";
        const items = isNodes ? (sources.nodes || []) : (sources.messages || []);
        if (!items.length) {
          return h("div", { className: "hermes-lcm-empty" },
            (sources.ids || []).length
              ? "Source items are no longer in the database."
              : "This summary records no source items.");
        }
        return h("div", { className: "hermes-lcm-stream" }, items.map(function (it) {
          return isNodes
            ? h(NodeRef, { key: it.node_id, n: it, onOpen: onOpenNode })
            : h(MessageItem, { key: it.store_id, m: it, onOpenMessage: onOpenMessage });
        }));
      })(),
    ]);
  }

  function SessionDetail(props) {
    const d = props.data || {};
    const onOpenNode = props.onOpenNode;
    const onOpenMessage = props.onOpenMessage;
    const c = d.counts || {};
    const [page, setPage] = useState(1);
    useEffect(function () {
      setPage(1);
    }, [d.session_id]);
    const loadedMessages = d.messages || [];
    const shownCount = Math.min(loadedMessages.length, page * SESSION_MESSAGE_PAGE_SIZE);
    const visibleMessages = loadedMessages.slice(0, shownCount);
    return h("div", { className: "hermes-lcm-detail" }, [
      h("div", { className: "hermes-lcm-statrow" }, [
        h(Stat, { value: fmtInt(c.message_count), label: "messages" }),
        h(Stat, { value: fmtInt(c.summary_node_count), label: "summaries" }),
        h(Stat, { value: fmtInt(c.token_estimate_total), label: "msg tokens" }),
        h(Stat, {
          value: ratioStr(c.source_token_count, c.summary_token_count),
          label: "compression",
        }),
      ]),
      (d.summary_nodes && d.summary_nodes.length) ? h("div", null, [
        h("h4", null, `Summary nodes (${d.summary_nodes.length})`),
        h("div", { className: "hermes-lcm-stream" }, d.summary_nodes.map(function (n) {
          return h(NodeRef, { key: n.node_id, n: n, onOpen: onOpenNode });
        })),
      ]) : null,
      h("div", { className: "hermes-lcm-section-head" }, [
        h("h4", null, `Messages (${fmtInt(c.message_count || loadedMessages.length)})`),
        h("div", { className: "hermes-lcm-dim" },
          `Showing ${fmtInt(visibleMessages.length)} of ${fmtInt(c.message_count || loadedMessages.length)}`),
      ]),
      h("div", { className: "hermes-lcm-stream" }, visibleMessages.map(function (m) {
        return h(MessageItem, {
          key: m.store_id,
          m: m,
          onOpenMessage: onOpenMessage,
          active: props.activeMessageId != null && Number(props.activeMessageId) === Number(m.store_id),
        });
      })),
      h("div", { className: "hermes-lcm-actions" }, [
        shownCount < loadedMessages.length ? h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          onClick: function () { setPage(page + 1); },
        }, `Show next ${SESSION_MESSAGE_PAGE_SIZE}`) : null,
        shownCount >= loadedMessages.length && d.has_more ? h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          onClick: props.onLoadMore,
          disabled: props.loadingMore,
        }, props.loadingMore ? "Loading more…" : `Load ${SESSION_FETCH_BATCH} more`) : null,
      ]),
    ]);
  }

  function ratioStr(src, out) {
    const s = Number(src) || 0;
    const o = Number(out) || 0;
    if (!o) return "—";
    return (Math.round((s / o) * 10) / 10) + "×";
  }

  function Drawer(props) {
    const panelRef = useRef(null);
    const returnFocusRef = useRef(null);
    const wasOpenRef = useRef(false);
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
    return h("div", { className: "hermes-lcm-drawer-overlay", onClick: props.onClose }, [
      h("div", {
        ref: panelRef,
        className: "hermes-lcm-drawer",
        role: "dialog",
        "aria-modal": "true",
        "aria-label": props.title || "Detail",
        tabIndex: -1,
        onClick: function (e) { e.stopPropagation(); },
      }, [
        h("div", { className: "hermes-lcm-drawer-head" }, [
          props.canBack ? h("button", {
            className: "hermes-lcm-btn", onClick: props.onBack, "aria-label": "Back to previous detail",
          }, "← Back") : null,
          h("div", { className: "hermes-lcm-drawer-title" }, props.title),
          h("button", {
            className: "hermes-lcm-btn", onClick: props.onClose, "aria-label": "Close detail (Escape)",
          }, "✕"),
        ]),
        h("div", { className: "hermes-lcm-drawer-body" }, props.children),
      ]),
    ]);
  }

  function DrawerError(props) {
    const label = props.kind === "node" ? "node" : (props.kind === "message" ? "message" : "session");
    return h("div", { className: "hermes-lcm-derror" }, [
      h("div", { className: "hermes-lcm-derror-title" },
        "Couldn't load this " + label),
      h("div", { className: "hermes-lcm-derror-msg" }, String(props.message || "Request failed")),
      props.onRetry ? h("button", {
        className: "hermes-lcm-btn hermes-lcm-derror-retry",
        onClick: props.onRetry,
      }, "↻ Retry") : null,
    ]);
  }

  function App() {
    const [q, setQ] = useState("");
    const [debouncedQ, setDebouncedQ] = useState("");
    const [role, setRole] = useState("");
    const [source, setSource] = useState("");
    const [data, setData] = useState(null);
    const [overviewLoading, setOverviewLoading] = useState(false);
    const [chartsLoading, setChartsLoading] = useState(false);
    const [overviewError, setOverviewError] = useState("");
    const [reloadToken, setReloadToken] = useState(0);

    const [searchData, setSearchData] = useState(null);
    const [searching, setSearching] = useState(false);
    const [searchError, setSearchError] = useState("");
    const [searchRetryToken, setSearchRetryToken] = useState(0);
    const [loadingMoreResults, setLoadingMoreResults] = useState(false);
    const [searchMessagePage, setSearchMessagePage] = useState(1);
    const [searchNodePage, setSearchNodePage] = useState(1);
    const [selectedResultIndex, setSelectedResultIndex] = useState(-1);

    const [timeline, setTimeline] = useState(null);
    const [compression, setCompression] = useState(null);
    const [chartsError, setChartsError] = useState("");

    const [stack, setStack] = useState([]);
    const rootRef = useRef(null);
    const searchInputRef = useRef(null);
    const resultRefs = useRef({});
    const searchOffsetRef = useRef(0);
    // Bumped whenever the search inputs change; in-flight pagination fetches
    // from an older query compare against it and drop their stale responses.
    const searchSeqRef = useRef(0);

    useEffect(function () {
      const handle = setTimeout(function () {
        setDebouncedQ(String(q || "").trim());
      }, 260);
      return function () { clearTimeout(handle); };
    }, [q]);

    useEffect(function () {
      let active = true;
      setOverviewLoading(true);
      setOverviewError("");
      SDK.fetchJSON(`${API}/overview?limit=25`).then(function (json) {
        if (active) {
          setData(json);
          setOverviewError("");
        }
      }).catch(function (err) {
        // Failed fetch ≠ empty database: keep `data` as-is (null or stale) and
        // surface the error so the UI never renders zeros for an outage.
        if (active) setOverviewError(friendlyError(err));
      }).finally(function () {
        if (active) setOverviewLoading(false);
      });
      return function () { active = false; };
    }, [reloadToken]);

    useEffect(function () {
      let active = true;
      setChartsLoading(true);
      setChartsError("");
      Promise.allSettled([
        SDK.fetchJSON(`${API}/timeline?bucket=day&limit=400`),
        SDK.fetchJSON(`${API}/compression?by=session&limit=12`),
      ]).then(function (results) {
        if (!active) return;
        // A rejected chart fetch leaves the previous value (or null) in place
        // instead of substituting empty datasets that read as "no data".
        if (results[0].status === "fulfilled") setTimeline(results[0].value);
        if (results[1].status === "fulfilled") setCompression(results[1].value);
        const failure = results[0].status === "rejected"
          ? results[0].reason
          : (results[1].status === "rejected" ? results[1].reason : null);
        setChartsError(failure ? friendlyError(failure) : "");
        setChartsLoading(false);
      });
      return function () { active = false; };
    }, [reloadToken]);

    useEffect(function () {
      searchSeqRef.current += 1;
      setSearchMessagePage(1);
      setSearchNodePage(1);
      setSelectedResultIndex(-1);
      if (!debouncedQ) {
        setSearchData(null);
        setSearchError("");
        return;
      }
      let active = true;
      setSearching(true);
      setSearchError("");
      searchOffsetRef.current = 0;
      const params = new URLSearchParams();
      params.set("q", debouncedQ);
      params.set("limit", String(SEARCH_FETCH_LIMIT));
      if (role) params.set("role", role);
      if (source) params.set("source", source);
      SDK.fetchJSON(`${API}/search?${params.toString()}`).then(function (json) {
        if (active) setSearchData(json);
      }).catch(function (err) {
        // Keep error and result state mutually exclusive: a failed search must
        // not fall through to the "No matches found" empty state.
        if (active) {
          setSearchData(null);
          setSearchError(friendlyError(err));
        }
      }).finally(function () {
        if (active) setSearching(false);
      });
      return function () { active = false; };
    }, [debouncedQ, role, source, searchRetryToken]);

    // Server-offset pagination (additive backend field `total` + `offset`):
    // pulls the next window for both result lists and appends with dedupe.
    // Responses are dropped when the query/facets changed while in flight, so
    // an old query's page can never merge into (or overwrite the totals of) a
    // newer query's results.
    const fetchMoreResults = useCallback(function () {
      if (!debouncedQ || !searchData || loadingMoreResults) return;
      const seq = searchSeqRef.current;
      const nextOffset = searchOffsetRef.current + SEARCH_FETCH_LIMIT;
      setLoadingMoreResults(true);
      const params = new URLSearchParams();
      params.set("q", debouncedQ);
      params.set("limit", String(SEARCH_FETCH_LIMIT));
      params.set("offset", String(nextOffset));
      if (role) params.set("role", role);
      if (source) params.set("source", source);
      SDK.fetchJSON(`${API}/search?${params.toString()}`).then(function (json) {
        if (seq !== searchSeqRef.current) return;
        searchOffsetRef.current = nextOffset;
        setSearchData(function (prev) { return mergeSearchPayload(prev, json); });
      }).catch(function (err) {
        if (seq !== searchSeqRef.current) return;
        setSearchError(friendlyError(err));
      }).finally(function () {
        setLoadingMoreResults(false);
      });
    }, [debouncedQ, role, source, searchData, loadingMoreResults]);

    const updateStackEntry = useCallback(function (matcher, updater) {
      setStack(function (prev) {
        const next = prev.slice();
        for (let i = next.length - 1; i >= 0; i--) {
          if (matcher(next[i])) {
            next[i] = updater(next[i]);
            break;
          }
        }
        return next;
      });
    }, []);

    const fetchNode = useCallback(function (id) {
      SDK.fetchJSON(`${API}/node/${encodeURIComponent(id)}`).then(function (json) {
        updateStackEntry(function (entry) {
          return entry.kind === "node" && String(entry.id) === String(id);
        }, function (entry) {
          return {
            kind: "node",
            id: id,
            data: json,
            loading: false,
            error: "",
          };
        });
      }).catch(function (err) {
        updateStackEntry(function (entry) {
          return entry.kind === "node" && String(entry.id) === String(id);
        }, function () {
          return {
            kind: "node",
            id: id,
            data: null,
            loading: false,
            error: String((err && err.message) || err),
          };
        });
      });
    }, [updateStackEntry]);

    const fetchSession = useCallback(function (id, offset, append, activeMessageId) {
      const params = new URLSearchParams();
      params.set("limit", String(SESSION_FETCH_BATCH));
      params.set("offset", String(offset || 0));
      SDK.fetchJSON(`${API}/session/${encodeURIComponent(id)}?${params.toString()}`).then(function (json) {
        updateStackEntry(function (entry) {
          return entry.kind === "session" && String(entry.id) === String(id);
        }, function (entry) {
          const previous = (append && entry.data && entry.data.messages) ? entry.data.messages : [];
          const nextMessages = append ? previous.concat(json.messages || []) : (json.messages || []);
          return {
            kind: "session",
            id: id,
            data: Object.assign({}, json, { messages: nextMessages }),
            loading: false,
            loadingMore: false,
            error: "",
            activeMessageId: activeMessageId != null ? activeMessageId : entry.activeMessageId,
          };
        });
      }).catch(function (err) {
        updateStackEntry(function (entry) {
          return entry.kind === "session" && String(entry.id) === String(id);
        }, function (entry) {
          return Object.assign({}, entry, {
            loading: false,
            loadingMore: false,
            error: String((err && err.message) || err),
          });
        });
      });
    }, [updateStackEntry]);

    const fetchMessageContext = useCallback(function (message) {
      const params = new URLSearchParams();
      params.set("limit", "1");
      params.set("offset", "0");
      SDK.fetchJSON(`${API}/session/${encodeURIComponent(message.session_id)}?${params.toString()}`).then(function (json) {
        updateStackEntry(function (entry) {
          return entry.kind === "message" && Number(entry.id) === Number(message.store_id);
        }, function () {
          return {
            kind: "message",
            id: message.store_id,
            sessionId: message.session_id,
            loading: false,
            error: "",
            data: {
              message: message,
              session: json,
            },
          };
        });
      }).catch(function (err) {
        updateStackEntry(function (entry) {
          return entry.kind === "message" && Number(entry.id) === Number(message.store_id);
        }, function () {
          return {
            kind: "message",
            id: message.store_id,
            sessionId: message.session_id,
            loading: false,
            error: String((err && err.message) || err),
            data: { message: message, session: null },
          };
        });
      });
    }, [updateStackEntry]);

    const openNode = useCallback(function (id) {
      setStack(function (prev) {
        return prev.concat([{ kind: "node", id: id, data: null, loading: true, error: "" }]);
      });
      fetchNode(id);
    }, [fetchNode]);

    const openSession = useCallback(function (id, opts) {
      const activeMessageId = opts && opts.activeMessageId != null ? opts.activeMessageId : null;
      setStack(function (prev) {
        return prev.concat([{
          kind: "session",
          id: id,
          data: null,
          loading: true,
          loadingMore: false,
          error: "",
          activeMessageId: activeMessageId,
        }]);
      });
      fetchSession(id, 0, false, activeMessageId);
    }, [fetchSession]);

    const openMessage = useCallback(function (message) {
      setStack(function (prev) {
        return prev.concat([{
          kind: "message",
          id: message.store_id,
          sessionId: message.session_id,
          data: { message: message, session: null },
          loading: true,
          error: "",
        }]);
      });
      fetchMessageContext(message);
    }, [fetchMessageContext]);

    const loadMoreSession = useCallback(function (id) {
      const current = stack.length ? stack[stack.length - 1] : null;
      if (!current || current.kind !== "session" || String(current.id) !== String(id) || !current.data || !current.data.has_more) {
        return;
      }
      const offset = (current.data.messages || []).length;
      updateStackEntry(function (entry) {
        return entry.kind === "session" && String(entry.id) === String(id);
      }, function (entry) {
        return Object.assign({}, entry, { loadingMore: true, error: "" });
      });
      fetchSession(id, offset, true, current.activeMessageId);
    }, [fetchSession, stack, updateStackEntry]);

    const goBack = useCallback(function () {
      setStack(function (prev) { return prev.slice(0, -1); });
    }, []);
    const closeDrawer = useCallback(function () {
      setStack([]);
    }, []);

    const top = stack.length ? stack[stack.length - 1] : null;
    const overview = (data && data.overview) || {};
    const comp = overview.compression || {};
    const sources = overview.source_counts || [];
    const hasLcmRows = Boolean(
      Number(overview.messages_total) ||
      Number(overview.summary_nodes_total) ||
      Number(overview.sessions_total)
    );

    // The server is unreachable when the overview fetch threw and we have no
    // (stale) payload to show; this must render error UI, never zero-data UI.
    const serverUnreachable = Boolean(overviewError) && !data;
    const staleData = Boolean(overviewError) && Boolean(data);

    const matches = (searchData && searchData.matches) || { messages: [], summary_nodes: [] };
    const fetchedMessageCount = (matches.messages || []).length;
    const fetchedNodeCount = (matches.summary_nodes || []).length;
    // Additive backend field: true totals + offset pagination. Fall back to
    // fetched counts when the running server predates the field.
    const searchTotals = (searchData && searchData.total) || null;
    const totalMessageCount = (searchTotals && Number(searchTotals.messages) >= 0)
      ? Number(searchTotals.messages)
      : fetchedMessageCount;
    const totalNodeCount = (searchTotals && Number(searchTotals.summary_nodes) >= 0)
      ? Number(searchTotals.summary_nodes)
      : fetchedNodeCount;
    const hasMoreServerResults = Boolean(searchTotals)
      && (fetchedMessageCount < totalMessageCount || fetchedNodeCount < totalNodeCount);
    const messageTotalPages = Math.max(1, Math.ceil((matches.messages || []).length / SEARCH_PAGE_SIZE));
    const nodeTotalPages = Math.max(1, Math.ceil((matches.summary_nodes || []).length / SEARCH_PAGE_SIZE));
    const visibleMessages = (matches.messages || []).slice(
      (searchMessagePage - 1) * SEARCH_PAGE_SIZE,
      searchMessagePage * SEARCH_PAGE_SIZE
    );
    const visibleNodes = (matches.summary_nodes || []).slice(
      (searchNodePage - 1) * SEARCH_PAGE_SIZE,
      searchNodePage * SEARCH_PAGE_SIZE
    );

    const keyboardResults = useMemo(function () {
      return visibleMessages.map(function (item) {
        return {
          key: "message:" + item.store_id,
          open: function () { openMessage(item); },
        };
      }).concat(visibleNodes.map(function (item) {
        return {
          key: "node:" + item.node_id,
          open: function () { openNode(item.node_id); },
        };
      }));
    }, [visibleMessages, visibleNodes, openMessage, openNode]);

    useEffect(function () {
      setSelectedResultIndex(function (prev) {
        if (!keyboardResults.length) return -1;
        if (prev >= keyboardResults.length) return keyboardResults.length - 1;
        return prev;
      });
    }, [keyboardResults.length]);

    const lastFocusedResultRef = useRef("");
    useEffect(function () {
      if (selectedResultIndex < 0 || selectedResultIndex >= keyboardResults.length) {
        lastFocusedResultRef.current = "";
        return;
      }
      const key = keyboardResults[selectedResultIndex].key;
      // Only move focus when the selection actually changes, and never while
      // a detail drawer is open — the drawer owns focus then.
      if (key === lastFocusedResultRef.current || stack.length) return;
      const el = resultRefs.current[key];
      if (!el) return;
      lastFocusedResultRef.current = key;
      try {
        if (typeof el.focus === "function") el.focus({ preventScroll: true });
      } catch (e) {
        if (typeof el.focus === "function") el.focus();
      }
      if (typeof el.scrollIntoView === "function") {
        el.scrollIntoView({ block: "nearest" });
      }
    }, [selectedResultIndex, keyboardResults, stack]);

    useEffect(function () {
      function isTypingTarget(target) {
        if (!target) return false;
        const tag = target.tagName;
        return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
      }
      // Keep-mounted hosts (the standalone shell) hide inactive tab panels
      // with `display: none` instead of unmounting them; a hidden panel must
      // not react to keystrokes meant for the visible tab.
      function isPanelHidden() {
        const el = rootRef.current;
        return !!el && el.offsetParent === null;
      }
      function onKeyDown(e) {
        if (e.defaultPrevented) return;
        if (isPanelHidden()) return;
        if (!e.metaKey && !e.ctrlKey && !e.altKey && e.key === "/" && !isTypingTarget(e.target)) {
          e.preventDefault();
          if (searchInputRef.current) {
            searchInputRef.current.focus();
            if (typeof searchInputRef.current.select === "function") searchInputRef.current.select();
          }
          return;
        }
        if (e.key === "Escape" && top) {
          e.preventDefault();
          closeDrawer();
          return;
        }
        if (!keyboardResults.length || e.metaKey || e.ctrlKey || e.altKey || isTypingTarget(e.target)) return;
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          setSelectedResultIndex(function (prev) {
            if (!keyboardResults.length) return -1;
            if (prev < 0) return e.key === "ArrowDown" ? 0 : keyboardResults.length - 1;
            return (prev + (e.key === "ArrowDown" ? 1 : -1) + keyboardResults.length) % keyboardResults.length;
          });
          return;
        }
        if (e.key === "Enter" && selectedResultIndex >= 0 && selectedResultIndex < keyboardResults.length) {
          e.preventDefault();
          keyboardResults[selectedResultIndex].open();
        }
      }
      window.addEventListener("keydown", onKeyDown);
      return function () { window.removeEventListener("keydown", onKeyDown); };
    }, [keyboardResults, selectedResultIndex, top, closeDrawer]);

    const searchPending = String(q || "").trim() !== debouncedQ;
    const searchActive = Boolean(String(q || "").trim() || debouncedQ || searching || searchData || searchError);
    const totalSearchMatches = totalMessageCount + totalNodeCount;

    let drawerTitle = "";
    let drawerBody = null;
    if (top) {
      if (top.loading) {
        drawerTitle = top.kind === "node"
          ? `Node #${top.id}`
          : (top.kind === "message" ? `Message #${top.id}` : `Session ${short(top.id, 40)}`);
        drawerBody = h("div", { className: "hermes-lcm-empty" }, "Loading…");
      } else if (top.error) {
        drawerTitle = top.kind === "node"
          ? `Node #${top.id}`
          : (top.kind === "message" ? `Message #${top.id}` : `Session ${short(top.id, 40)}`);
        const current = top;
        drawerBody = h(DrawerError, {
          kind: current.kind,
          message: current.error,
          onRetry: function () {
            updateStackEntry(function (entry) {
              return entry === current;
            }, function (entry) {
              return Object.assign({}, entry, { loading: true, error: "" });
            });
            if (current.kind === "node") fetchNode(current.id);
            else if (current.kind === "message") fetchMessageContext(current.data && current.data.message);
            else fetchSession(current.id, 0, false, current.activeMessageId);
          },
        });
      } else if (top.kind === "node") {
        drawerTitle = `Node #${top.id}`;
        drawerBody = h(NodeDetail, {
          data: top.data,
          onOpenNode: openNode,
          onOpenSession: openSession,
          onOpenMessage: openMessage,
        });
      } else if (top.kind === "message") {
        drawerTitle = `Message #${top.id}`;
        drawerBody = h(MessageDetail, {
          data: top.data,
          onOpenNode: openNode,
          onOpenSession: openSession,
        });
      } else {
        drawerTitle = `Session ${short(top.id, 40)}`;
        drawerBody = h(SessionDetail, {
          data: top.data,
          onOpenNode: openNode,
          onOpenMessage: openMessage,
          onLoadMore: function () { loadMoreSession(top.id); },
          loadingMore: !!top.loadingMore,
          activeMessageId: top.activeMessageId,
        });
      }
    }

    // Search results render directly under the toolbar (see placement below)
    // so typing a query gives immediate visible feedback instead of appending
    // results below the overview cards, off-screen.
    const searchShell = searchActive ? h("div", { className: "hermes-lcm-card hermes-lcm-wide hermes-lcm-search-shell" }, [
      h("div", { className: "hermes-lcm-search-head" }, [
        h("div", null, [
          h("h3", null, "Search"),
          h("div", { className: "hermes-lcm-search-subtitle", role: "status" },
            searchPending
              ? "Waiting for typing to pause…"
              : (searching
                  ? "Searching messages and summary nodes…"
                  : (debouncedQ && searchData
                      ? `${fmtInt(totalSearchMatches)} matches for "${short(debouncedQ, 36)}".`
                      : "Use / to focus and arrows to move through the current page."))),
        ]),
        h("div", { className: "hermes-lcm-badge-row" }, [
          debouncedQ ? toolBadge(`"${short(debouncedQ, 36)}"`) : null,
          searchData && searchData.engine === "fts" ? toolBadge("FTS ranked", "ok") : null,
          searchData && searchData.engine === "like" ? toolBadge("LIKE fallback", "warn") : null,
          (!searchPending && !searching && debouncedQ && searchData) ? toolBadge(fmtInt(totalSearchMatches) + " hits") : null,
        ]),
      ]),
      searchError ? h("div", { className: "hermes-lcm-error", role: "alert" }, [
        h("div", null, [
          h("strong", null, "Search failed. "),
          searchError + " — results below may be incomplete; this is not an empty result.",
        ]),
        h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          onClick: function () { setSearchRetryToken(function (n) { return n + 1; }); },
        }, "Retry search"),
      ]) : null,
      (!searchPending && searching && !searchData) ? h("div", { className: "hermes-lcm-grid" }, [
        h("div", { className: "hermes-lcm-card" }, h(SkeletonLines, { count: 5, widths: ["95%", "90%", "88%", "92%", "70%"] })),
        h("div", { className: "hermes-lcm-card" }, h(SkeletonLines, { count: 4, widths: ["92%", "84%", "88%", "68%"] })),
      ]) : null,
      (!searchPending && !searching && debouncedQ && !searchError && totalSearchMatches === 0) ? h("div", { className: "hermes-lcm-empty" }, [
        h("strong", null, "No matches found."),
        " Try removing a facet or a punctuation-heavy query so the backend can stay on the ranked FTS path.",
      ]) : null,
      totalSearchMatches > 0 ? h("div", { className: "hermes-lcm-grid" }, [
        h("div", { className: "hermes-lcm-card" }, [
          h("div", { className: "hermes-lcm-section-head" }, [
            h("h3", null, totalMessageCount > fetchedMessageCount
              ? `Matching Messages (${fmtInt(fetchedMessageCount)} of ${fmtInt(totalMessageCount)})`
              : `Matching Messages (${fmtInt(fetchedMessageCount)})`),
            h("div", { className: "hermes-lcm-dim" }, "Click for full content and session context"),
          ]),
          h("div", { className: "hermes-lcm-results" },
            visibleMessages.length
              ? visibleMessages.map(function (m, idx) {
                  const resultKey = "message:" + m.store_id;
                  const selected = selectedResultIndex === idx;
                  return h(SearchResultCard, {
                    key: resultKey,
                    resultRef: function (el) {
                      if (el) resultRefs.current[resultKey] = el;
                      else delete resultRefs.current[resultKey];
                    },
                    kind: "message",
                    item: m,
                    query: debouncedQ,
                    selected: selected,
                    onFocus: function () { setSelectedResultIndex(idx); },
                    onOpen: function () { openMessage(m); },
                  });
                })
              : h("div", { className: "hermes-lcm-empty" }, "No matching messages on this page.")
          ),
          h(Pager, {
            page: searchMessagePage,
            totalPages: messageTotalPages,
            onChange: setSearchMessagePage,
          }),
        ]),
        h("div", { className: "hermes-lcm-card" }, [
          h("div", { className: "hermes-lcm-section-head" }, [
            h("h3", null, totalNodeCount > fetchedNodeCount
              ? `Matching Summaries (${fmtInt(fetchedNodeCount)} of ${fmtInt(totalNodeCount)})`
              : `Matching Summaries (${fmtInt(fetchedNodeCount)})`),
            h("div", { className: "hermes-lcm-dim" }, "Open a node to follow its source links"),
          ]),
          h("div", { className: "hermes-lcm-results" },
            visibleNodes.length
              ? visibleNodes.map(function (n, idx) {
                  const absoluteIndex = visibleMessages.length + idx;
                  const resultKey = "node:" + n.node_id;
                  const selected = selectedResultIndex === absoluteIndex;
                  return h(SearchResultCard, {
                    key: resultKey,
                    resultRef: function (el) {
                      if (el) resultRefs.current[resultKey] = el;
                      else delete resultRefs.current[resultKey];
                    },
                    kind: "node",
                    item: n,
                    query: debouncedQ,
                    selected: selected,
                    onFocus: function () { setSelectedResultIndex(absoluteIndex); },
                    onOpen: function () { openNode(n.node_id); },
                  });
                })
              : h("div", { className: "hermes-lcm-empty" }, "No matching summaries on this page.")
          ),
          h(Pager, {
            page: searchNodePage,
            totalPages: nodeTotalPages,
            onChange: setSearchNodePage,
          }),
        ]),
      ]) : null,
      hasMoreServerResults ? h("div", { className: "hermes-lcm-actions hermes-lcm-fetch-more" }, [
        h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          disabled: loadingMoreResults,
          onClick: fetchMoreResults,
        }, loadingMoreResults
          ? "Fetching more results…"
          : `Fetch next ${fmtInt(SEARCH_FETCH_LIMIT)} from server`),
        h("span", { className: "hermes-lcm-dim" },
          `${fmtInt(fetchedMessageCount + fetchedNodeCount)} of ${fmtInt(totalSearchMatches)} loaded`),
      ]) : null,
    ]) : null;

    return h("div", { className: "hermes-lcm", ref: rootRef }, [
      h("div", { className: "hermes-lcm-top" }, [
        h("div", { className: "hermes-lcm-search-wrap" }, [
          h("input", {
            ref: searchInputRef,
            className: "hermes-lcm-search",
            value: q,
            type: "search",
            placeholder: "Search messages and summaries",
            "aria-label": "Search messages and summaries",
            onChange: function (e) { setQ(e.target.value || ""); },
            onKeyDown: function (e) {
              if (e.key === "ArrowDown" && keyboardResults.length) {
                e.preventDefault();
                setSelectedResultIndex(0);
              }
            },
          }),
          q ? h("button", {
            type: "button",
            className: "hermes-lcm-btn hermes-lcm-clear",
            "aria-label": "Clear search query",
            onClick: function () { setQ(""); setSearchData(null); setSearchError(""); },
          }, "Clear") : null,
        ]),
        h("select", {
          className: "hermes-lcm-select", value: role,
          "aria-label": "Filter by role",
          onChange: function (e) { setRole(e.target.value); },
        }, [
          h("option", { key: "all", value: "" }, "All roles"),
          h("option", { key: "user", value: "user" }, "user"),
          h("option", { key: "assistant", value: "assistant" }, "assistant"),
          h("option", { key: "tool", value: "tool" }, "tool"),
          h("option", { key: "system", value: "system" }, "system"),
        ]),
        h("select", {
          className: "hermes-lcm-select", value: source,
          "aria-label": "Filter by source",
          onChange: function (e) { setSource(e.target.value); },
        }, [h("option", { key: "all", value: "" }, "All sources")].concat(
          sources.map(function (s) {
            return h("option", { key: s.source, value: s.source }, short(s.source, 18));
          })
        )),
        h("div", {
          className: "hermes-lcm-status" + (overviewError ? " hermes-lcm-status-err" : ""),
          role: "status",
        },
          (overviewLoading || chartsLoading) ? "Loading overview"
            : overviewError ? "Server unreachable"
            : ((data && data.exists) ? "Database detected" : "Database missing")
        ),
      ]),
      h("div", { className: "hermes-lcm-shortcuts" }, [
        h("span", null, "`/` focus search"),
        h("span", null, "Arrow keys browse results"),
        h("span", null, "Enter opens detail"),
      ]),
      // Which session store is being served (additive `storage_scope` field;
      // older servers omit it and only the path renders).
      h("div", { className: "hermes-lcm-path" }, data ? [
        data.storage_scope === "project_local"
          ? h("span", { key: "scope", className: "hermes-lcm-tag hermes-lcm-tag-src" }, "Project store")
          : (data.storage_scope === "global"
              ? h("span", { key: "scope", className: "hermes-lcm-tag" }, "Global store")
              : null),
        h("span", { key: "path" }, data.path),
      ] : ""),

      searchShell,

      // Unreachable server: a distinguishable error hero with retry — never
      // the zeroed stats / "No data" cards that imply an empty database.
      serverUnreachable ? h("div", {
        className: "hermes-lcm-empty-panel hermes-lcm-offline",
        role: "alert",
      }, [
        h("div", { className: "hermes-lcm-empty-orb hermes-lcm-offline-orb", "aria-hidden": "true" }),
        h("div", { className: "hermes-lcm-empty-copy" }, [
          h("div", { className: "hermes-lcm-empty-kicker" }, "Connection problem"),
          h("h2", null, "Can't reach the tokensave server"),
          h("p", null, "The LCM overview request failed, so no counts or timelines can be shown. Your data is not gone — the dashboard just can't talk to the server right now."),
          h("div", { className: "hermes-lcm-offline-actions" }, [
            h("button", {
              type: "button",
              className: "hermes-lcm-btn",
              onClick: function () { setReloadToken(function (n) { return n + 1; }); },
            }, "↻ Retry now"),
            h("span", { className: "hermes-lcm-dim" }, overviewError),
          ]),
        ]),
      ]) : null,

      staleData ? h("div", { className: "hermes-lcm-error", role: "alert" }, [
        h("div", null, `Refresh failed (${overviewError}) — showing previously loaded data.`),
        h("button", {
          type: "button",
          className: "hermes-lcm-btn",
          onClick: function () { setReloadToken(function (n) { return n + 1; }); },
        }, "Retry"),
      ]) : null,
      data && data.error ? h("div", { className: "hermes-lcm-error", role: "alert" }, data.error) : null,

      data && !data.exists ? h("div", { className: "hermes-lcm-empty-panel" }, [
        h("div", { className: "hermes-lcm-empty-orb", "aria-hidden": "true" }),
        h("div", { className: "hermes-lcm-empty-copy" }, [
          h("div", { className: "hermes-lcm-empty-kicker" }, "Lossless Context Store"),
          h("h2", null, data.storage_scope === "project_local"
            ? "Project session store not found"
            : "Global LCM database not found"),
          h("p", null, "The dashboard can render once the session store exists. Until then, the search, timeline, and detail views remain unavailable."),
        ]),
      ]) : null,

      data && data.exists && !hasLcmRows ? h("div", { className: "hermes-lcm-empty-panel" }, [
        h("div", { className: "hermes-lcm-empty-orb", "aria-hidden": "true" }),
        h("div", { className: "hermes-lcm-empty-copy" }, [
          h("div", { className: "hermes-lcm-empty-kicker" }, "Lossless Context Store"),
          h("h2", null, "No LCM sessions indexed yet"),
          h("p", null, data.storage_scope === "project_local"
            ? "This project's session store (.tokensave/sessions.db) exists but holds no messages yet. Cursor sessions are ingested by its end-of-turn hook; Claude/Codex/Vibe/Cline transcripts are swept automatically when the MCP server or this dashboard starts. Run an agent turn in this project and refresh."
            : "The global database exists, but it does not contain raw messages or summary nodes. Once sessions are ingested, this page will fill with timelines, compression ratios, searchable messages, and summary-node drilldowns."),
        ]),
      ]) : null,

      // Stats render only from a successful overview payload; zeros are then
      // genuinely "empty database", never a masked fetch failure.
      data ? h("div", { className: "hermes-lcm-statrow" }, [
        h(Stat, { value: fmtInt(overview.messages_total), label: "messages" }),
        h(Stat, { value: fmtInt(overview.sessions_total), label: "sessions" }),
        h(Stat, { value: fmtInt(overview.summary_nodes_total), label: "summary nodes" }),
        h(Stat, { value: (comp.ratio ? comp.ratio + "×" : "—"), label: "compression" }),
        h(Stat, { value: `${fmtInt(comp.source_token_count)}→${fmtInt(comp.token_count)}`, label: "tokens kept" }),
      ]) : (overviewLoading ? h("div", { className: "hermes-lcm-statrow" }, [
        h("div", { className: "hermes-lcm-stat hermes-lcm-skeleton" }, h(SkeletonLines, { count: 2, widths: ["55%", "35%"] })),
        h("div", { className: "hermes-lcm-stat hermes-lcm-skeleton" }, h(SkeletonLines, { count: 2, widths: ["45%", "30%"] })),
        h("div", { className: "hermes-lcm-stat hermes-lcm-skeleton" }, h(SkeletonLines, { count: 2, widths: ["62%", "38%"] })),
      ]) : null),

      serverUnreachable ? null : h("div", { className: "hermes-lcm-grid" }, [
        h("div", { className: "hermes-lcm-card hermes-lcm-wide" }, [
          h("h3", null, "Message Timeline (per day · dots = summaries)"),
          chartsError && !timeline
            ? h("div", { className: "hermes-lcm-error", role: "alert" }, [
                h("div", null, chartsError),
                h("button", {
                  type: "button",
                  className: "hermes-lcm-btn",
                  onClick: function () { setReloadToken(function (n) { return n + 1; }); },
                }, "Retry"),
              ])
            : (chartsLoading && !timeline)
              ? h(SkeletonLines, { count: 5, widths: ["100%", "95%", "90%", "92%", "88%"] })
              : h(TimelineChart, {
                  buckets: (timeline && timeline.buckets) || [],
                  nodeBuckets: (timeline && timeline.node_buckets) || [],
                  undatedCount: (timeline && timeline.undated && timeline.undated.count) || 0,
                }),
        ]),
        h("div", { className: "hermes-lcm-card hermes-lcm-wide" }, [
          h("h3", null, "Compression by Session (kept vs saved)"),
          chartsError && !compression
            ? h("div", { className: "hermes-lcm-error", role: "alert" }, [
                h("div", null, chartsError),
                h("button", {
                  type: "button",
                  className: "hermes-lcm-btn",
                  onClick: function () { setReloadToken(function (n) { return n + 1; }); },
                }, "Retry"),
              ])
            : (chartsLoading && !compression)
              ? h(SkeletonLines, { count: 4, widths: ["98%", "90%", "84%", "88%"] })
              : h(CompressionBars, {
                  groups: (compression && compression.groups) || [],
                  onPick: function (g) { openSession(g.session_id != null ? g.session_id : g.key); },
                }),
        ]),
      ]),

      serverUnreachable ? null : h("div", { className: "hermes-lcm-grid" }, [
        h("div", { className: "hermes-lcm-card" }, [
          h("h3", null, "By Source"),
          h(BarList, {
            rows: sources,
            keyName: "source",
            onPick: function (v) { setSource(v === "(none)" ? "unknown" : v); },
          }),
        ]),
        h("div", { className: "hermes-lcm-card" }, [
          h("h3", null, "By Role"),
          h(BarList, { rows: overview.role_counts || [], keyName: "role", onPick: function (v) { setRole(v); } }),
        ]),
        h("div", { className: "hermes-lcm-card" }, [
          h("h3", null, "Summary Depth"),
          h(BarList, { rows: overview.depth_counts || [], keyName: "depth" }),
        ]),
      ]),

      serverUnreachable ? null : h("div", { className: "hermes-lcm-grid" }, [
        h("div", { className: "hermes-lcm-card" }, [
          h("h3", null, "Recent Sessions"),
          h("div", { className: "hermes-lcm-rows" },
            ((data && data.latest_sessions) || []).length
              ? ((data && data.latest_sessions) || []).map(function (s, idx) {
                  const tail = sessionTail(s.session_id);
                  return h("button", {
                    key: s.session_id + ":" + idx,
                    type: "button",
                    className: "hermes-lcm-row",
                    onClick: function () { openSession(s.session_id); },
                  }, [
                    h("div", { className: "hermes-lcm-row-main" }, [
                      h("span", { className: "hermes-lcm-row-title" }, sessionLabel(s.session_id)),
                      tail ? h("span", { className: "hermes-lcm-row-id" }, tail) : null,
                    ]),
                    h("div", { className: "hermes-lcm-row-meta" }, [
                      h("span", { className: "hermes-lcm-pill" }, fmtInt(s.message_count) + " msgs"),
                      h(TimeText, { className: "hermes-lcm-dim", epoch: s.last_timestamp }),
                    ]),
                  ]);
                })
              : (data
                  ? h("div", { className: "hermes-lcm-empty" }, "No sessions")
                  : h(SkeletonLines, { count: 3, widths: ["92%", "84%", "76%"] }))
          ),
        ]),
        h("div", { className: "hermes-lcm-card" }, [
          h("h3", null, "Latest Summaries"),
          h("div", { className: "hermes-lcm-rows" },
            ((data && data.latest_summary_nodes) || []).length
              ? ((data && data.latest_summary_nodes) || []).map(function (n) {
                  const title = summaryTitle(n.summary);
                  const preview = stripMd(n.summary);
                  return h("button", {
                    key: n.node_id,
                    type: "button",
                    className: "hermes-lcm-row",
                    onClick: function () { openNode(n.node_id); },
                  }, [
                    h("div", { className: "hermes-lcm-row-meta" }, [
                      h("span", { className: "hermes-lcm-pill hermes-lcm-pill-accent" }, "D" + n.depth),
                      n.category ? h("span", { className: "hermes-lcm-pill" }, n.category) : null,
                      h("span", { className: "hermes-lcm-dim" }, sessionLabel(n.session_id)),
                      n.token_count != null ? h("span", { className: "hermes-lcm-dim" }, fmtInt(n.token_count) + " tok") : null,
                    ]),
                    h("div", { className: "hermes-lcm-row-title" }, short(title, 80)),
                    h("div", { className: "hermes-lcm-row-sub" }, short(preview, 150)),
                  ]);
                })
              : (data
                  ? h("div", { className: "hermes-lcm-empty" }, "No summaries")
                  : h(SkeletonLines, { count: 3, widths: ["90%", "82%", "74%"] }))
          ),
        ]),
      ]),

      h(Drawer, {
        open: !!top,
        title: drawerTitle,
        canBack: stack.length > 1,
        onBack: goBack,
        onClose: closeDrawer,
      }, drawerBody),
    ]);
  }

  if (window.__HERMES_PLUGINS__ && typeof window.__HERMES_PLUGINS__.register === "function") {
    window.__HERMES_PLUGINS__.register("hermes-lcm", App);
  }
})();
