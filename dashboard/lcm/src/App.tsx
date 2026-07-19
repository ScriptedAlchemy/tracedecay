/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * hermes-lcm dashboard root component.
 *
 * Faithful 1:1 port of the original IIFE in `index.js`. All `hermes-lcm-*`
 * class names, DOM structure, API query shapes, pagination/dedupe behavior,
 * drawer back-stack, focus management, and reload-token refetch patterns are
 * preserved. Only surface syntax changed (`React.createElement` → JSX).
 *
 * Render sections live in sibling components (`TopBar`, `SearchShell`,
 * `StoreStatePanels`, `StatRow`, `ChartCards`, `DistributionCards`,
 * `RecentLists`, `LcmDrawer`); this file owns all state, effects, and
 * callbacks and passes props down unchanged.
 */

import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { fetchJSON } from "../../lib/sdk";
import {
  API,
  SEARCH_FETCH_LIMIT,
  SEARCH_PAGE_SIZE,
  SESSION_FETCH_BATCH,
  friendlyError,
  mergeSearchPayload,
} from "./helpers";
import { StoreHealthCard } from "./StoreHealth";
import { TopBar } from "./TopBar";
import { SearchShell } from "./SearchShell";
import { StoreStatePanels } from "./StoreStatePanels";
import { StatRow } from "./StatRow";
import { ChartCards } from "./ChartCards";
import { DistributionCards } from "./DistributionCards";
import { RecentLists } from "./RecentLists";
import { LcmDrawer } from "./LcmDrawer";

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

  useEffect(
    function () {
      const handle = setTimeout(function () {
        setDebouncedQ(String(q || "").trim());
      }, 260);
      return function () {
        clearTimeout(handle);
      };
    },
    [q],
  );

  useEffect(
    function () {
      let active = true;
      setOverviewLoading(true);
      setOverviewError("");
      fetchJSON(`${API}/overview?limit=25`)
        .then(function (json) {
          if (active) {
            setData(json);
            setOverviewError("");
          }
        })
        .catch(function (err) {
          // Failed fetch ≠ empty database: keep `data` as-is (null or stale) and
          // surface the error so the UI never renders zeros for an outage.
          if (active) setOverviewError(friendlyError(err));
        })
        .finally(function () {
          if (active) setOverviewLoading(false);
        });
      return function () {
        active = false;
      };
    },
    [reloadToken],
  );

  useEffect(
    function () {
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
        if (results[0].status === "fulfilled")
          setTimeline((results[0] as any).value);
        if (results[1].status === "fulfilled")
          setCompression((results[1] as any).value);
        const failure =
          results[0].status === "rejected"
            ? (results[0] as any).reason
            : results[1].status === "rejected"
              ? (results[1] as any).reason
              : null;
        setChartsError(failure ? friendlyError(failure) : "");
        setChartsLoading(false);
      });
      return function () {
        active = false;
      };
    },
    [reloadToken],
  );

  useEffect(
    function () {
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
      fetchJSON(`${API}/search?${params.toString()}`)
        .then(function (json) {
          if (active) setSearchData(json);
        })
        .catch(function (err) {
          // Keep error and result state mutually exclusive: a failed search must
          // not fall through to the "No matches found" empty state.
          if (active) {
            setSearchData(null);
            setSearchError(friendlyError(err));
          }
        })
        .finally(function () {
          if (active) setSearching(false);
        });
      return function () {
        active = false;
      };
    },
    [debouncedQ, role, source, searchRetryToken],
  );

  // Server-offset pagination (additive backend field `total` + `offset`):
  // pulls the next window for both result lists and appends with dedupe.
  // Responses are dropped when the query/facets changed while in flight, so
  // an old query's page can never merge into (or overwrite the totals of) a
  // newer query's results.
  const fetchMoreResults = useCallback(
    function () {
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
      fetchJSON(`${API}/search?${params.toString()}`)
        .then(function (json) {
          if (seq !== searchSeqRef.current) return;
          searchOffsetRef.current = nextOffset;
          setSearchData(function (prev) {
            return mergeSearchPayload(prev, json);
          });
        })
        .catch(function (err) {
          if (seq !== searchSeqRef.current) return;
          setSearchError(friendlyError(err));
        })
        .finally(function () {
          setLoadingMoreResults(false);
        });
    },
    [debouncedQ, role, source, searchData, loadingMoreResults],
  );

  const updateStackEntry = useCallback(function (
    matcher: (e: any) => boolean,
    updater: (e: any) => any,
  ) {
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

  const fetchNode = useCallback(
    function (id: any) {
      fetchJSON(`${API}/node/${encodeURIComponent(id)}`)
        .then(function (json) {
          updateStackEntry(
            function (entry) {
              return entry.kind === "node" && String(entry.id) === String(id);
            },
            function () {
              return {
                kind: "node",
                id: id,
                data: json,
                loading: false,
                error: "",
              };
            },
          );
        })
        .catch(function (err) {
          updateStackEntry(
            function (entry) {
              return entry.kind === "node" && String(entry.id) === String(id);
            },
            function () {
              return {
                kind: "node",
                id: id,
                data: null,
                loading: false,
                error: String((err && err.message) || err),
              };
            },
          );
        });
    },
    [updateStackEntry],
  );

  const fetchSession = useCallback(
    function (id: any, offset: any, append: boolean, activeMessageId: any) {
      const params = new URLSearchParams();
      params.set("limit", String(SESSION_FETCH_BATCH));
      params.set("offset", String(offset || 0));
      fetchJSON<any>(
        `${API}/session/${encodeURIComponent(id)}?${params.toString()}`,
      )
        .then(function (json) {
          updateStackEntry(
            function (entry) {
              return (
                entry.kind === "session" && String(entry.id) === String(id)
              );
            },
            function (entry) {
              const previous =
                append && entry.data && entry.data.messages
                  ? entry.data.messages
                  : [];
              const nextMessages = append
                ? previous.concat(json.messages || [])
                : json.messages || [];
              return {
                kind: "session",
                id: id,
                data: Object.assign({}, json, { messages: nextMessages }),
                loading: false,
                loadingMore: false,
                error: "",
                activeMessageId:
                  activeMessageId != null
                    ? activeMessageId
                    : entry.activeMessageId,
              };
            },
          );
        })
        .catch(function (err) {
          updateStackEntry(
            function (entry) {
              return (
                entry.kind === "session" && String(entry.id) === String(id)
              );
            },
            function (entry) {
              return Object.assign({}, entry, {
                loading: false,
                loadingMore: false,
                error: String((err && err.message) || err),
              });
            },
          );
        });
    },
    [updateStackEntry],
  );

  const fetchMessageContext = useCallback(
    function (message: any) {
      const params = new URLSearchParams();
      params.set("limit", "1");
      params.set("offset", "0");
      fetchJSON(
        `${API}/session/${encodeURIComponent(message.session_id)}?${params.toString()}`,
      )
        .then(function (json) {
          updateStackEntry(
            function (entry) {
              return (
                entry.kind === "message" &&
                Number(entry.id) === Number(message.store_id)
              );
            },
            function () {
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
            },
          );
        })
        .catch(function (err) {
          updateStackEntry(
            function (entry) {
              return (
                entry.kind === "message" &&
                Number(entry.id) === Number(message.store_id)
              );
            },
            function () {
              return {
                kind: "message",
                id: message.store_id,
                sessionId: message.session_id,
                loading: false,
                error: String((err && err.message) || err),
                data: { message: message, session: null },
              };
            },
          );
        });
    },
    [updateStackEntry],
  );

  const openNode = useCallback(
    function (id: any) {
      setStack(function (prev) {
        return prev.concat([
          { kind: "node", id: id, data: null, loading: true, error: "" },
        ]);
      });
      fetchNode(id);
    },
    [fetchNode],
  );

  const openSession = useCallback(
    function (id: any, opts?: any) {
      const activeMessageId =
        opts && opts.activeMessageId != null ? opts.activeMessageId : null;
      setStack(function (prev) {
        return prev.concat([
          {
            kind: "session",
            id: id,
            data: null,
            loading: true,
            loadingMore: false,
            error: "",
            activeMessageId: activeMessageId,
          },
        ]);
      });
      fetchSession(id, 0, false, activeMessageId);
    },
    [fetchSession],
  );

  const openMessage = useCallback(
    function (message: any) {
      setStack(function (prev) {
        return prev.concat([
          {
            kind: "message",
            id: message.store_id,
            sessionId: message.session_id,
            data: { message: message, session: null },
            loading: true,
            error: "",
          },
        ]);
      });
      fetchMessageContext(message);
    },
    [fetchMessageContext],
  );

  const loadMoreSession = useCallback(
    function (id: any) {
      const current = stack.length ? stack[stack.length - 1] : null;
      if (
        !current ||
        current.kind !== "session" ||
        String(current.id) !== String(id) ||
        !current.data ||
        !current.data.has_more
      ) {
        return;
      }
      const offset = (current.data.messages || []).length;
      updateStackEntry(
        function (entry) {
          return entry.kind === "session" && String(entry.id) === String(id);
        },
        function (entry) {
          return Object.assign({}, entry, { loadingMore: true, error: "" });
        },
      );
      fetchSession(id, offset, true, current.activeMessageId);
    },
    [fetchSession, stack, updateStackEntry],
  );

  const goBack = useCallback(function () {
    setStack(function (prev) {
      return prev.slice(0, -1);
    });
  }, []);
  const closeDrawer = useCallback(function () {
    setStack([]);
  }, []);

  const top = stack.length ? stack[stack.length - 1] : null;
  const overview = (data && data.overview) || {};
  const sources = overview.source_counts || [];
  const hasLcmRows = Boolean(
    Number(overview.messages_total) ||
    Number(overview.summary_nodes_total) ||
    Number(overview.sessions_total),
  );

  // The server is unreachable when the overview fetch threw and we have no
  // (stale) payload to show; this must render error UI, never zero-data UI.
  const serverUnreachable = Boolean(overviewError) && !data;
  const staleData = Boolean(overviewError) && Boolean(data);

  const matches = (searchData && searchData.matches) || {
    messages: [],
    summary_nodes: [],
  };
  const fetchedMessageCount = (matches.messages || []).length;
  const fetchedNodeCount = (matches.summary_nodes || []).length;
  // Additive backend field: true totals + offset pagination. Fall back to
  // fetched counts when the running server predates the field.
  const searchTotals = (searchData && searchData.total) || null;
  const totalMessageCount =
    searchTotals && Number(searchTotals.messages) >= 0
      ? Number(searchTotals.messages)
      : fetchedMessageCount;
  const totalNodeCount =
    searchTotals && Number(searchTotals.summary_nodes) >= 0
      ? Number(searchTotals.summary_nodes)
      : fetchedNodeCount;
  const hasMoreServerResults =
    Boolean(searchTotals) &&
    (fetchedMessageCount < totalMessageCount ||
      fetchedNodeCount < totalNodeCount);
  const messageTotalPages = Math.max(
    1,
    Math.ceil((matches.messages || []).length / SEARCH_PAGE_SIZE),
  );
  const nodeTotalPages = Math.max(
    1,
    Math.ceil((matches.summary_nodes || []).length / SEARCH_PAGE_SIZE),
  );
  const visibleMessages = (matches.messages || []).slice(
    (searchMessagePage - 1) * SEARCH_PAGE_SIZE,
    searchMessagePage * SEARCH_PAGE_SIZE,
  );
  const visibleNodes = (matches.summary_nodes || []).slice(
    (searchNodePage - 1) * SEARCH_PAGE_SIZE,
    searchNodePage * SEARCH_PAGE_SIZE,
  );

  const keyboardResults = useMemo(
    function () {
      return visibleMessages
        .map(function (item) {
          return {
            key: "message:" + item.store_id,
            open: function () {
              openMessage(item);
            },
          };
        })
        .concat(
          visibleNodes.map(function (item) {
            return {
              key: "node:" + item.node_id,
              open: function () {
                openNode(item.node_id);
              },
            };
          }),
        );
    },
    [visibleMessages, visibleNodes, openMessage, openNode],
  );

  useEffect(
    function () {
      setSelectedResultIndex(function (prev) {
        if (!keyboardResults.length) return -1;
        if (prev >= keyboardResults.length) return keyboardResults.length - 1;
        return prev;
      });
    },
    [keyboardResults.length],
  );

  const lastFocusedResultRef = useRef("");
  useEffect(
    function () {
      if (
        selectedResultIndex < 0 ||
        selectedResultIndex >= keyboardResults.length
      ) {
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
    },
    [selectedResultIndex, keyboardResults, stack],
  );

  useEffect(
    function () {
      function isTypingTarget(target: any) {
        if (!target) return false;
        const tag = target.tagName;
        return (
          tag === "INPUT" ||
          tag === "TEXTAREA" ||
          tag === "SELECT" ||
          target.isContentEditable
        );
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
        if (
          !e.metaKey &&
          !e.ctrlKey &&
          !e.altKey &&
          e.key === "/" &&
          !isTypingTarget(e.target)
        ) {
          e.preventDefault();
          if (searchInputRef.current) {
            searchInputRef.current.focus();
            if (typeof searchInputRef.current.select === "function")
              searchInputRef.current.select();
          }
          return;
        }
        if (e.key === "Escape" && top) {
          e.preventDefault();
          closeDrawer();
          return;
        }
        if (
          !keyboardResults.length ||
          e.metaKey ||
          e.ctrlKey ||
          e.altKey ||
          isTypingTarget(e.target)
        )
          return;
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          setSelectedResultIndex(function (prev) {
            if (!keyboardResults.length) return -1;
            if (prev < 0)
              return e.key === "ArrowDown" ? 0 : keyboardResults.length - 1;
            return (
              (prev +
                (e.key === "ArrowDown" ? 1 : -1) +
                keyboardResults.length) %
              keyboardResults.length
            );
          });
          return;
        }
        if (
          e.key === "Enter" &&
          selectedResultIndex >= 0 &&
          selectedResultIndex < keyboardResults.length
        ) {
          e.preventDefault();
          keyboardResults[selectedResultIndex].open();
        }
      }
      window.addEventListener("keydown", onKeyDown);
      return function () {
        window.removeEventListener("keydown", onKeyDown);
      };
    },
    [keyboardResults, selectedResultIndex, top, closeDrawer],
  );

  const searchPending = String(q || "").trim() !== debouncedQ;
  const searchActive = Boolean(
    String(q || "").trim() ||
    debouncedQ ||
    searching ||
    searchData ||
    searchError,
  );
  const totalSearchMatches = totalMessageCount + totalNodeCount;

  return (
    <div className="hermes-lcm" ref={rootRef}>
      <TopBar
        searchInputRef={searchInputRef}
        q={q}
        onQueryChange={setQ}
        keyboardResultCount={keyboardResults.length}
        onSelectFirstResult={function () {
          setSelectedResultIndex(0);
        }}
        onClearQuery={function () {
          setQ("");
          setSearchData(null);
          setSearchError("");
        }}
        role={role}
        onRoleChange={setRole}
        source={source}
        onSourceChange={setSource}
        sources={sources}
        overviewError={overviewError}
        overviewLoading={overviewLoading}
        chartsLoading={chartsLoading}
        data={data}
      />

      <SearchShell
        searchActive={searchActive}
        debouncedQ={debouncedQ}
        searchPending={searchPending}
        searching={searching}
        searchData={searchData}
        searchError={searchError}
        totalSearchMatches={totalSearchMatches}
        totalMessageCount={totalMessageCount}
        totalNodeCount={totalNodeCount}
        fetchedMessageCount={fetchedMessageCount}
        fetchedNodeCount={fetchedNodeCount}
        visibleMessages={visibleMessages}
        visibleNodes={visibleNodes}
        selectedResultIndex={selectedResultIndex}
        onSelectResult={setSelectedResultIndex}
        resultRefs={resultRefs}
        onOpenMessage={openMessage}
        onOpenNode={openNode}
        searchMessagePage={searchMessagePage}
        searchNodePage={searchNodePage}
        messageTotalPages={messageTotalPages}
        nodeTotalPages={nodeTotalPages}
        onMessagePageChange={setSearchMessagePage}
        onNodePageChange={setSearchNodePage}
        hasMoreServerResults={hasMoreServerResults}
        loadingMoreResults={loadingMoreResults}
        onFetchMore={fetchMoreResults}
        onRetrySearch={function () {
          setSearchRetryToken(function (n) {
            return n + 1;
          });
        }}
      />

      <StoreStatePanels
        serverUnreachable={serverUnreachable}
        staleData={staleData}
        overviewError={overviewError}
        data={data}
        hasLcmRows={hasLcmRows}
        onRetry={function () {
          setReloadToken(function (n) {
            return n + 1;
          });
        }}
      />

      <StatRow data={data} overviewLoading={overviewLoading} />

      {serverUnreachable ? null : (
        <ChartCards
          chartsError={chartsError}
          timeline={timeline}
          compression={compression}
          chartsLoading={chartsLoading}
          onRetry={function () {
            setReloadToken(function (n) {
              return n + 1;
            });
          }}
          onOpenSession={openSession}
        />
      )}

      {serverUnreachable ? null : (
        <DistributionCards
          sources={sources}
          roleCounts={overview.role_counts || []}
          depthCounts={overview.depth_counts || []}
          onSourceChange={setSource}
          onRoleChange={setRole}
        />
      )}

      {serverUnreachable ? null : (
        <RecentLists
          data={data}
          onOpenSession={openSession}
          onOpenNode={openNode}
        />
      )}

      {!serverUnreachable && data?.exists && (
        <div className="hermes-lcm-grid">
          <StoreHealthCard />
        </div>
      )}

      <LcmDrawer
        top={top}
        canBack={stack.length > 1}
        onBack={goBack}
        onClose={closeDrawer}
        updateStackEntry={updateStackEntry}
        fetchNode={fetchNode}
        fetchMessageContext={fetchMessageContext}
        fetchSession={fetchSession}
        onOpenNode={openNode}
        onOpenSession={openSession}
        onOpenMessage={openMessage}
        onLoadMoreSession={loadMoreSession}
      />
    </div>
  );
}

export default App;
