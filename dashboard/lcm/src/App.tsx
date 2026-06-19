/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * hermes-lcm dashboard root component.
 *
 * Faithful 1:1 port of the original IIFE in `index.js`. All `hermes-lcm-*`
 * class names, DOM structure, API query shapes, pagination/dedupe behavior,
 * drawer back-stack, focus management, and reload-token refetch patterns are
 * preserved. Only surface syntax changed (`React.createElement` → JSX).
 */

import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { fetchJSON } from "../../lib/sdk";
import {
  API,
  SEARCH_FETCH_LIMIT,
  SEARCH_PAGE_SIZE,
  SESSION_FETCH_BATCH,
  fmtInt,
  friendlyError,
  mergeSearchPayload,
  sessionLabel,
  sessionTail,
  short,
  stripMd,
  summaryTitle,
} from "./helpers";
import {
  BarList,
  CompressionBars,
  Drawer,
  DrawerError,
  MessageDetail,
  NodeDetail,
  Pager,
  SearchResultCard,
  SessionDetail,
  SkeletonLines,
  Stat,
  TimelineChart,
  TimeText,
  toolBadge,
} from "./components";
import { EmptyState, ErrorPanel } from "../../lib/primitives";

function App(): React.ReactElement {
  const [q, setQ] = useState("");
  const [debouncedQ, setDebouncedQ] = useState("");
  const [role, setRole] = useState("");
  const [source, setSource] = useState("");
  const [data, setData] = useState<any>(null);
  const [overviewLoading, setOverviewLoading] = useState(false);
  const [chartsLoading, setChartsLoading] = useState(false);
  const [overviewError, setOverviewError] = useState("");
  const [reloadToken, setReloadToken] = useState(0);

  const [searchData, setSearchData] = useState<any>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState("");
  const [searchRetryToken, setSearchRetryToken] = useState(0);
  const [loadingMoreResults, setLoadingMoreResults] = useState(false);
  const [searchMessagePage, setSearchMessagePage] = useState(1);
  const [searchNodePage, setSearchNodePage] = useState(1);
  const [selectedResultIndex, setSelectedResultIndex] = useState(-1);

  const [timeline, setTimeline] = useState<any>(null);
  const [compression, setCompression] = useState<any>(null);
  const [chartsError, setChartsError] = useState("");

  const [stack, setStack] = useState<any[]>([]);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const resultRefs = useRef<Record<string, HTMLElement>>({});
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
    fetchJSON(`${API}/overview?limit=25`).then(function (json) {
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
      fetchJSON(`${API}/timeline?bucket=day&limit=400`),
      fetchJSON(`${API}/compression?by=session&limit=12`),
    ]).then(function (results) {
      if (!active) return;
      // A rejected chart fetch leaves the previous value (or null) in place
      // instead of substituting empty datasets that read as "no data".
      if (results[0].status === "fulfilled") setTimeline((results[0] as any).value);
      if (results[1].status === "fulfilled") setCompression((results[1] as any).value);
      const failure = results[0].status === "rejected"
        ? (results[0] as any).reason
        : (results[1].status === "rejected" ? (results[1] as any).reason : null);
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
    fetchJSON(`${API}/search?${params.toString()}`).then(function (json) {
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
    fetchJSON(`${API}/search?${params.toString()}`).then(function (json) {
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

  const updateStackEntry = useCallback(function (matcher: (e: any) => boolean, updater: (e: any) => any) {
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

  const fetchNode = useCallback(function (id: any) {
    fetchJSON(`${API}/node/${encodeURIComponent(id)}`).then(function (json) {
      updateStackEntry(function (entry) {
        return entry.kind === "node" && String(entry.id) === String(id);
      }, function () {
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

  const fetchSession = useCallback(function (id: any, offset: any, append: boolean, activeMessageId: any) {
    const params = new URLSearchParams();
    params.set("limit", String(SESSION_FETCH_BATCH));
    params.set("offset", String(offset || 0));
    fetchJSON(`${API}/session/${encodeURIComponent(id)}?${params.toString()}`).then(function (json) {
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

  const fetchMessageContext = useCallback(function (message: any) {
    const params = new URLSearchParams();
    params.set("limit", "1");
    params.set("offset", "0");
    fetchJSON(`${API}/session/${encodeURIComponent(message.session_id)}?${params.toString()}`).then(function (json) {
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

  const openNode = useCallback(function (id: any) {
    setStack(function (prev) {
      return prev.concat([{ kind: "node", id: id, data: null, loading: true, error: "" }]);
    });
    fetchNode(id);
  }, [fetchNode]);

  const openSession = useCallback(function (id: any, opts?: any) {
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

  const openMessage = useCallback(function (message: any) {
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

  const loadMoreSession = useCallback(function (id: any) {
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
    function isTypingTarget(target: any) {
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
    function onKeyDown(e: any) {
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
  let drawerBody: React.ReactNode = null;
  if (top) {
    if (top.loading) {
      drawerTitle = top.kind === "node"
        ? `Node #${top.id}`
        : (top.kind === "message" ? `Message #${top.id}` : `Session ${short(top.id, 40)}`);
      drawerBody = <EmptyState className="hermes-lcm-empty">Loading…</EmptyState>;
    } else if (top.error) {
      drawerTitle = top.kind === "node"
        ? `Node #${top.id}`
        : (top.kind === "message" ? `Message #${top.id}` : `Session ${short(top.id, 40)}`);
      const current = top;
      drawerBody = (
        <DrawerError
          kind={current.kind}
          message={current.error}
          onRetry={function () {
            updateStackEntry(function (entry) {
              return entry === current;
            }, function (entry) {
              return Object.assign({}, entry, { loading: true, error: "" });
            });
            if (current.kind === "node") fetchNode(current.id);
            else if (current.kind === "message") fetchMessageContext(current.data && current.data.message);
            else fetchSession(current.id, 0, false, current.activeMessageId);
          }}
        />
      );
    } else if (top.kind === "node") {
      drawerTitle = `Node #${top.id}`;
      drawerBody = (
        <NodeDetail
          data={top.data}
          onOpenNode={openNode}
          onOpenSession={openSession}
          onOpenMessage={openMessage}
        />
      );
    } else if (top.kind === "message") {
      drawerTitle = `Message #${top.id}`;
      drawerBody = (
        <MessageDetail
          data={top.data}
          onOpenNode={openNode}
          onOpenSession={openSession}
        />
      );
    } else {
      drawerTitle = `Session ${short(top.id, 40)}`;
      drawerBody = (
        <SessionDetail
          data={top.data}
          onOpenNode={openNode}
          onOpenMessage={openMessage}
          onLoadMore={function () { loadMoreSession(top.id); }}
          loadingMore={!!top.loadingMore}
          activeMessageId={top.activeMessageId}
        />
      );
    }
  }

  // Search results render directly under the toolbar (see placement below)
  // so typing a query gives immediate visible feedback instead of appending
  // results below the overview cards, off-screen.
  const searchShell = searchActive ? (
    <div className="hermes-lcm-card hermes-lcm-wide hermes-lcm-search-shell">
      <div className="hermes-lcm-search-head">
        <div>
          <h3>Search</h3>
          <div className="hermes-lcm-search-subtitle" role="status">
            {searchPending
              ? "Waiting for typing to pause…"
              : (searching
                  ? "Searching messages and summary nodes…"
                  : (debouncedQ && searchData
                      ? `${fmtInt(totalSearchMatches)} matches for "${short(debouncedQ, 36)}".`
                      : "Use / to focus and arrows to move through the current page."))}
          </div>
        </div>
        <div className="hermes-lcm-badge-row">
          {debouncedQ ? toolBadge(`"${short(debouncedQ, 36)}"`) : null}
          {searchData && searchData.engine === "fts" ? toolBadge("FTS ranked", "ok") : null}
          {searchData && searchData.engine === "like" ? toolBadge("LIKE fallback", "warn") : null}
          {(!searchPending && !searching && debouncedQ && searchData) ? toolBadge(fmtInt(totalSearchMatches) + " hits") : null}
        </div>
      </div>
      {searchError ? (
        <div className="hermes-lcm-error" role="alert">
          <div>
            <strong>Search failed. </strong>
            {searchError + " — results below may be incomplete; this is not an empty result."}
          </div>
          <button
            type="button"
            className="hermes-lcm-btn"
            onClick={function () { setSearchRetryToken(function (n) { return n + 1; }); }}
          >Retry search</button>
        </div>
      ) : null}
      {(!searchPending && searching && !searchData) ? (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card"><SkeletonLines count={5} widths={["95%", "90%", "88%", "92%", "70%"]} /></div>
          <div className="hermes-lcm-card"><SkeletonLines count={4} widths={["92%", "84%", "88%", "68%"]} /></div>
        </div>
      ) : null}
      {(!searchPending && !searching && debouncedQ && !searchError && totalSearchMatches === 0) ? (
        <EmptyState className="hermes-lcm-empty">
          <strong>No matches found.</strong>
          {" Try removing a facet or a punctuation-heavy query so the backend can stay on the ranked FTS path."}
        </EmptyState>
      ) : null}
      {totalSearchMatches > 0 ? (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card">
            <div className="hermes-lcm-section-head">
              <h3>{totalMessageCount > fetchedMessageCount
                ? `Matching Messages (${fmtInt(fetchedMessageCount)} of ${fmtInt(totalMessageCount)})`
                : `Matching Messages (${fmtInt(fetchedMessageCount)})`}</h3>
              <div className="hermes-lcm-dim">Click for full content and session context</div>
            </div>
            <div className="hermes-lcm-results">
              {visibleMessages.length
                ? visibleMessages.map(function (m, idx) {
                    const resultKey = "message:" + m.store_id;
                    const selected = selectedResultIndex === idx;
                    return (
                      <SearchResultCard
                        key={resultKey}
                        resultRef={function (el) {
                          if (el) resultRefs.current[resultKey] = el;
                          else delete resultRefs.current[resultKey];
                        }}
                        kind="message"
                        item={m}
                        query={debouncedQ}
                        selected={selected}
                        onFocus={function () { setSelectedResultIndex(idx); }}
                        onOpen={function () { openMessage(m); }}
                      />
                    );
                  })
                : <EmptyState className="hermes-lcm-empty">No matching messages on this page.</EmptyState>}
            </div>
            <Pager
              page={searchMessagePage}
              totalPages={messageTotalPages}
              onChange={setSearchMessagePage}
            />
          </div>
          <div className="hermes-lcm-card">
            <div className="hermes-lcm-section-head">
              <h3>{totalNodeCount > fetchedNodeCount
                ? `Matching Summaries (${fmtInt(fetchedNodeCount)} of ${fmtInt(totalNodeCount)})`
                : `Matching Summaries (${fmtInt(fetchedNodeCount)})`}</h3>
              <div className="hermes-lcm-dim">Open a node to follow its source links</div>
            </div>
            <div className="hermes-lcm-results">
              {visibleNodes.length
                ? visibleNodes.map(function (n, idx) {
                    const absoluteIndex = visibleMessages.length + idx;
                    const resultKey = "node:" + n.node_id;
                    const selected = selectedResultIndex === absoluteIndex;
                    return (
                      <SearchResultCard
                        key={resultKey}
                        resultRef={function (el) {
                          if (el) resultRefs.current[resultKey] = el;
                          else delete resultRefs.current[resultKey];
                        }}
                        kind="node"
                        item={n}
                        query={debouncedQ}
                        selected={selected}
                        onFocus={function () { setSelectedResultIndex(absoluteIndex); }}
                        onOpen={function () { openNode(n.node_id); }}
                      />
                    );
                  })
                : <EmptyState className="hermes-lcm-empty">No matching summaries on this page.</EmptyState>}
            </div>
            <Pager
              page={searchNodePage}
              totalPages={nodeTotalPages}
              onChange={setSearchNodePage}
            />
          </div>
        </div>
      ) : null}
      {hasMoreServerResults ? (
        <div className="hermes-lcm-actions hermes-lcm-fetch-more">
          <button
            type="button"
            className="hermes-lcm-btn"
            disabled={loadingMoreResults}
            onClick={fetchMoreResults}
          >{loadingMoreResults
            ? "Fetching more results…"
            : `Fetch next ${fmtInt(SEARCH_FETCH_LIMIT)} from server`}</button>
          <span className="hermes-lcm-dim">
            {`${fmtInt(fetchedMessageCount + fetchedNodeCount)} of ${fmtInt(totalSearchMatches)} loaded`}
          </span>
        </div>
      ) : null}
    </div>
  ) : null;

  return (
    <div className="hermes-lcm" ref={rootRef}>
      <div className="hermes-lcm-top">
        <div className="hermes-lcm-search-wrap">
          <input
            ref={searchInputRef}
            className="hermes-lcm-search"
            value={q}
            type="search"
            placeholder="Search messages and summaries"
            aria-label="Search messages and summaries"
            onChange={function (e) { setQ(e.target.value || ""); }}
            onKeyDown={function (e) {
              if (e.key === "ArrowDown" && keyboardResults.length) {
                e.preventDefault();
                setSelectedResultIndex(0);
              }
            }}
          />
          {q ? (
            <button
              type="button"
              className="hermes-lcm-btn hermes-lcm-clear"
              aria-label="Clear search query"
              onClick={function () { setQ(""); setSearchData(null); setSearchError(""); }}
            >Clear</button>
          ) : null}
        </div>
        <select
          className="hermes-lcm-select"
          value={role}
          aria-label="Filter by role"
          onChange={function (e) { setRole(e.target.value); }}
        >
          <option value="">All roles</option>
          <option value="user">user</option>
          <option value="assistant">assistant</option>
          <option value="tool">tool</option>
          <option value="system">system</option>
        </select>
        <select
          className="hermes-lcm-select"
          value={source}
          aria-label="Filter by source"
          onChange={function (e) { setSource(e.target.value); }}
        >
          <option value="">All sources</option>
          {sources.map(function (s) {
            return <option key={s.source} value={s.source}>{short(s.source, 18)}</option>;
          })}
        </select>
        <div
          className={"hermes-lcm-status" + (overviewError ? " hermes-lcm-status-err" : "")}
          role="status"
        >
          {(overviewLoading || chartsLoading) ? "Loading overview"
            : overviewError ? "Server unreachable"
            : ((data && data.exists) ? "Database detected" : "Database missing")}
        </div>
      </div>
      <div className="hermes-lcm-shortcuts">
        <span>`/` focus search</span>
        <span>Arrow keys browse results</span>
        <span>Enter opens detail</span>
      </div>
      {/* Which session store is being served (scope tag + database path). */}
      <div className="hermes-lcm-path">
        {data ? (
          <>
            {data.storage_scope === "project_local"
              ? <span className="hermes-lcm-tag hermes-lcm-tag-src">Project store</span>
              : (data.storage_scope === "global"
                  ? <span className="hermes-lcm-tag">Global store</span>
                  : null)}
            <span>{data.path}</span>
          </>
        ) : ""}
      </div>

      {searchShell}

      {/* Unreachable server: a distinguishable error hero with retry — never
          the zeroed stats / "No data" cards that imply an empty database. */}
      {serverUnreachable ? (
        <div className="hermes-lcm-empty-panel hermes-lcm-offline" role="alert">
          <div className="hermes-lcm-empty-orb hermes-lcm-offline-orb" aria-hidden="true" />
          <div className="hermes-lcm-empty-copy">
            <div className="hermes-lcm-empty-kicker">Connection problem</div>
            <h2>Can't reach the tracedecay server</h2>
            <p>The LCM overview request failed, so no counts or timelines can be shown. Your data is not gone — the dashboard just can't talk to the server right now.</p>
            <div className="hermes-lcm-offline-actions">
              <button
                type="button"
                className="hermes-lcm-btn"
                onClick={function () { setReloadToken(function (n) { return n + 1; }); }}
              >↻ Retry now</button>
              <span className="hermes-lcm-dim">{overviewError}</span>
            </div>
          </div>
        </div>
      ) : null}

      {staleData ? (
        <ErrorPanel
          error={`Refresh failed (${overviewError}) — showing previously loaded data.`}
          onRetry={function () { setReloadToken(function (n) { return n + 1; }); }}
          className="hermes-lcm-error"
        />
      ) : null}
      {data && data.error ? <ErrorPanel error={data.error} className="hermes-lcm-error" /> : null}

      {data && !data.exists ? (
        <div className="hermes-lcm-empty-panel">
          <div className="hermes-lcm-empty-orb" aria-hidden="true" />
          <div className="hermes-lcm-empty-copy">
            <div className="hermes-lcm-empty-kicker">Lossless Context Store</div>
            <h2>{data.storage_scope === "project_local"
              ? "Project session store not found"
              : "Global LCM database not found"}</h2>
            <p>The dashboard can render once the session store exists. Until then, the search, timeline, and detail views remain unavailable.</p>
          </div>
        </div>
      ) : null}

      {data && data.exists && !hasLcmRows ? (
        <div className="hermes-lcm-empty-panel">
          <div className="hermes-lcm-empty-orb" aria-hidden="true" />
          <div className="hermes-lcm-empty-copy">
            <div className="hermes-lcm-empty-kicker">Lossless Context Store</div>
            <h2>No LCM sessions indexed yet</h2>
            <p>{data.storage_scope === "project_local"
              ? "This project's session store (.tracedecay/sessions.db) exists but holds no messages yet. Cursor sessions are ingested by its end-of-turn hook; Claude/Codex/Vibe/Cline transcripts are swept automatically when the MCP server or this dashboard starts. Run an agent turn in this project and refresh."
              : "The global database exists, but it does not contain raw messages or summary nodes. Once sessions are ingested, this page will fill with timelines, compression ratios, searchable messages, and summary-node drilldowns."}</p>
          </div>
        </div>
      ) : null}

      {/* Stats render only from a successful overview payload; zeros are then
          genuinely "empty database", never a masked fetch failure. */}
      {data ? (
        <div className="hermes-lcm-statrow">
          <Stat value={fmtInt(overview.messages_total)} label="messages" />
          <Stat value={fmtInt(overview.sessions_total)} label="sessions" />
          <Stat value={fmtInt(overview.summary_nodes_total)} label="summary nodes" />
          <Stat value={(comp.ratio ? comp.ratio + "×" : "—")} label="compression" />
          <Stat value={`${fmtInt(comp.source_token_count)}→${fmtInt(comp.token_count)}`} label="tokens kept" />
        </div>
      ) : (overviewLoading ? (
        <div className="hermes-lcm-statrow">
          <div className="hermes-lcm-stat hermes-lcm-skeleton"><SkeletonLines count={2} widths={["55%", "35%"]} /></div>
          <div className="hermes-lcm-stat hermes-lcm-skeleton"><SkeletonLines count={2} widths={["45%", "30%"]} /></div>
          <div className="hermes-lcm-stat hermes-lcm-skeleton"><SkeletonLines count={2} widths={["62%", "38%"]} /></div>
        </div>
      ) : null)}

      {serverUnreachable ? null : (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card hermes-lcm-wide">
            <h3>Message Timeline (per day · dots = summaries)</h3>
            {chartsError && !timeline
              ? (
                <ErrorPanel
                  error={chartsError}
                  onRetry={function () { setReloadToken(function (n) { return n + 1; }); }}
                  className="hermes-lcm-error"
                />
              )
              : (chartsLoading && !timeline)
                ? <SkeletonLines count={5} widths={["100%", "95%", "90%", "92%", "88%"]} />
                : (
                  <TimelineChart
                    buckets={(timeline && timeline.buckets) || []}
                    nodeBuckets={(timeline && timeline.node_buckets) || []}
                    undatedCount={(timeline && timeline.undated && timeline.undated.count) || 0}
                  />
                )}
          </div>
          <div className="hermes-lcm-card hermes-lcm-wide">
            <h3>Compression by Session (kept vs saved)</h3>
            {chartsError && !compression
              ? (
                <ErrorPanel
                  error={chartsError}
                  onRetry={function () { setReloadToken(function (n) { return n + 1; }); }}
                  className="hermes-lcm-error"
                />
              )
              : (chartsLoading && !compression)
                ? <SkeletonLines count={4} widths={["98%", "90%", "84%", "88%"]} />
                : (
                  <CompressionBars
                    groups={(compression && compression.groups) || []}
                    onPick={function (g) { openSession(g.session_id != null ? g.session_id : g.key); }}
                  />
                )}
          </div>
        </div>
      )}

      {serverUnreachable ? null : (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card">
            <h3>By Source</h3>
            <BarList
              rows={sources}
              keyName="source"
              onPick={function (v) { setSource(v === "(none)" ? "unknown" : v); }}
            />
          </div>
          <div className="hermes-lcm-card">
            <h3>By Role</h3>
            <BarList rows={overview.role_counts || []} keyName="role" onPick={function (v) { setRole(v); }} />
          </div>
          <div className="hermes-lcm-card">
            <h3>Summary Depth</h3>
            <BarList rows={overview.depth_counts || []} keyName="depth" />
          </div>
        </div>
      )}

      {serverUnreachable ? null : (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card">
            <h3>Recent Sessions</h3>
            <div className="hermes-lcm-rows">
              {((data && data.latest_sessions) || []).length
                ? ((data && data.latest_sessions) || []).map(function (s, idx) {
                    const tail = sessionTail(s.session_id);
                    return (
                      <button
                        key={s.session_id + ":" + idx}
                        type="button"
                        className="hermes-lcm-row"
                        onClick={function () { openSession(s.session_id); }}
                      >
                        <div className="hermes-lcm-row-main">
                          <span className="hermes-lcm-row-title">{sessionLabel(s.session_id)}</span>
                          {tail ? <span className="hermes-lcm-row-id">{tail}</span> : null}
                        </div>
                        <div className="hermes-lcm-row-meta">
                          <span className="hermes-lcm-pill">{fmtInt(s.message_count) + " msgs"}</span>
                          <TimeText className="hermes-lcm-dim" epoch={s.last_timestamp} />
                        </div>
                      </button>
                    );
                  })
                : (data
                    ? <EmptyState className="hermes-lcm-empty">No sessions</EmptyState>
                    : <SkeletonLines count={3} widths={["92%", "84%", "76%"]} />)}
            </div>
          </div>
          <div className="hermes-lcm-card">
            <h3>Latest Summaries</h3>
            <div className="hermes-lcm-rows">
              {((data && data.latest_summary_nodes) || []).length
                ? ((data && data.latest_summary_nodes) || []).map(function (n) {
                    const title = summaryTitle(n.summary);
                    const preview = stripMd(n.summary);
                    return (
                      <button
                        key={n.node_id}
                        type="button"
                        className="hermes-lcm-row"
                        onClick={function () { openNode(n.node_id); }}
                      >
                        <div className="hermes-lcm-row-meta">
                          <span className="hermes-lcm-pill hermes-lcm-pill-accent">{"D" + n.depth}</span>
                          {n.category ? <span className="hermes-lcm-pill">{n.category}</span> : null}
                          <span className="hermes-lcm-dim">{sessionLabel(n.session_id)}</span>
                          {n.token_count != null ? <span className="hermes-lcm-dim">{fmtInt(n.token_count) + " tok"}</span> : null}
                        </div>
                        <div className="hermes-lcm-row-title">{short(title, 80)}</div>
                        <div className="hermes-lcm-row-sub">{short(preview, 150)}</div>
                      </button>
                    );
                  })
                : (data
                    ? <EmptyState className="hermes-lcm-empty">No summaries</EmptyState>
                    : <SkeletonLines count={3} widths={["90%", "82%", "74%"]} />)}
            </div>
          </div>
        </div>
      )}

      <Drawer
        open={!!top}
        title={drawerTitle}
        canBack={stack.length > 1}
        onBack={goBack}
        onClose={closeDrawer}
      >
        {drawerBody}
      </Drawer>
    </div>
  );
}

export default App;
