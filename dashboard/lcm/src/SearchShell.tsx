/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Search results shell of the hermes-lcm dashboard: status subtitle, engine
 * badges, error banner with retry, skeletons, the message/summary result
 * columns with pagers, and the server-offset "fetch more" row.
 * Extracted 1:1 from `App.tsx` — DOM structure and class names unchanged.
 */

import React from "react";
import { SEARCH_FETCH_LIMIT, fmtInt, short } from "./helpers";
import { Pager, SearchResultCard, toolBadge } from "./components";
import { EmptyState, SkeletonLines } from "../../lib/primitives";

interface SearchShellProps {
  searchActive: boolean;
  debouncedQ: string;
  searchPending: boolean;
  searching: boolean;
  searchData: any;
  searchError: string;
  totalSearchMatches: number;
  totalMessageCount: number;
  totalNodeCount: number;
  fetchedMessageCount: number;
  fetchedNodeCount: number;
  visibleMessages: any[];
  visibleNodes: any[];
  selectedResultIndex: number;
  onSelectResult: (index: number) => void;
  resultRefs: React.MutableRefObject<Record<string, HTMLElement>>;
  onOpenMessage: (message: any) => void;
  onOpenNode: (id: any) => void;
  searchMessagePage: number;
  searchNodePage: number;
  messageTotalPages: number;
  nodeTotalPages: number;
  onMessagePageChange: (page: number) => void;
  onNodePageChange: (page: number) => void;
  hasMoreServerResults: boolean;
  loadingMoreResults: boolean;
  onFetchMore: () => void;
  onRetrySearch: () => void;
}

// Search results render directly under the toolbar (see placement in App)
// so typing a query gives immediate visible feedback instead of appending
// results below the overview cards, off-screen.
export function SearchShell({
  searchActive,
  debouncedQ,
  searchPending,
  searching,
  searchData,
  searchError,
  totalSearchMatches,
  totalMessageCount,
  totalNodeCount,
  fetchedMessageCount,
  fetchedNodeCount,
  visibleMessages,
  visibleNodes,
  selectedResultIndex,
  onSelectResult,
  resultRefs,
  onOpenMessage,
  onOpenNode,
  searchMessagePage,
  searchNodePage,
  messageTotalPages,
  nodeTotalPages,
  onMessagePageChange,
  onNodePageChange,
  hasMoreServerResults,
  loadingMoreResults,
  onFetchMore,
  onRetrySearch,
}: SearchShellProps): React.ReactElement | null {
  if (!searchActive) return null;
  return (
    <div className="hermes-lcm-card hermes-lcm-wide hermes-lcm-search-shell">
      <div className="hermes-lcm-search-head">
        <div>
          <h3>Search</h3>
          <div className="hermes-lcm-search-subtitle" role="status">
            {searchPending
              ? "Waiting for typing to pause…"
              : searching
                ? "Searching messages and summary nodes…"
                : debouncedQ && searchData
                  ? `${fmtInt(totalSearchMatches)} matches for "${short(debouncedQ, 36)}".`
                  : "Use / to focus and arrows to move through the current page."}
          </div>
        </div>
        <div className="hermes-lcm-badge-row">
          {debouncedQ ? toolBadge(`"${short(debouncedQ, 36)}"`) : null}
          {searchData && searchData.engine === "fts"
            ? toolBadge("FTS ranked", "ok")
            : null}
          {searchData && searchData.engine === "like"
            ? toolBadge("LIKE fallback", "warn")
            : null}
          {!searchPending && !searching && debouncedQ && searchData
            ? toolBadge(fmtInt(totalSearchMatches) + " hits")
            : null}
        </div>
      </div>
      {searchError ? (
        <div className="hermes-lcm-error" role="alert">
          <div>
            <strong>Search failed. </strong>
            {searchError +
              " — results below may be incomplete; this is not an empty result."}
          </div>
          <button
            type="button"
            className="hermes-lcm-btn"
            onClick={onRetrySearch}
          >
            Retry search
          </button>
        </div>
      ) : null}
      {!searchPending && searching && !searchData ? (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card">
            <SkeletonLines
              count={5}
              widths={["95%", "90%", "88%", "92%", "70%"]}
            />
          </div>
          <div className="hermes-lcm-card">
            <SkeletonLines count={4} widths={["92%", "84%", "88%", "68%"]} />
          </div>
        </div>
      ) : null}
      {!searchPending &&
      !searching &&
      debouncedQ &&
      !searchError &&
      totalSearchMatches === 0 ? (
        <EmptyState className="hermes-lcm-empty">
          <strong>No matches found.</strong>
          {
            " Try removing a facet or a punctuation-heavy query so the backend can stay on the ranked FTS path."
          }
        </EmptyState>
      ) : null}
      {totalSearchMatches > 0 ? (
        <div className="hermes-lcm-grid">
          <div className="hermes-lcm-card">
            <div className="hermes-lcm-section-head">
              <h3>
                {totalMessageCount > fetchedMessageCount
                  ? `Matching Messages (${fmtInt(fetchedMessageCount)} of ${fmtInt(totalMessageCount)})`
                  : `Matching Messages (${fmtInt(fetchedMessageCount)})`}
              </h3>
              <div className="hermes-lcm-dim">
                Click for full content and session context
              </div>
            </div>
            <div className="hermes-lcm-results">
              {visibleMessages.length ? (
                visibleMessages.map(function (m, idx) {
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
                      onFocus={function () {
                        onSelectResult(idx);
                      }}
                      onOpen={function () {
                        onOpenMessage(m);
                      }}
                    />
                  );
                })
              ) : (
                <EmptyState className="hermes-lcm-empty">
                  No matching messages on this page.
                </EmptyState>
              )}
            </div>
            <Pager
              page={searchMessagePage}
              totalPages={messageTotalPages}
              onChange={onMessagePageChange}
            />
          </div>
          <div className="hermes-lcm-card">
            <div className="hermes-lcm-section-head">
              <h3>
                {totalNodeCount > fetchedNodeCount
                  ? `Matching Summaries (${fmtInt(fetchedNodeCount)} of ${fmtInt(totalNodeCount)})`
                  : `Matching Summaries (${fmtInt(fetchedNodeCount)})`}
              </h3>
              <div className="hermes-lcm-dim">
                Open a node to follow its source links
              </div>
            </div>
            <div className="hermes-lcm-results">
              {visibleNodes.length ? (
                visibleNodes.map(function (n, idx) {
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
                      onFocus={function () {
                        onSelectResult(absoluteIndex);
                      }}
                      onOpen={function () {
                        onOpenNode(n.node_id);
                      }}
                    />
                  );
                })
              ) : (
                <EmptyState className="hermes-lcm-empty">
                  No matching summaries on this page.
                </EmptyState>
              )}
            </div>
            <Pager
              page={searchNodePage}
              totalPages={nodeTotalPages}
              onChange={onNodePageChange}
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
            onClick={onFetchMore}
          >
            {loadingMoreResults
              ? "Fetching more results…"
              : `Fetch next ${fmtInt(SEARCH_FETCH_LIMIT)} from server`}
          </button>
          <span className="hermes-lcm-dim">
            {`${fmtInt(fetchedMessageCount + fetchedNodeCount)} of ${fmtInt(totalSearchMatches)} loaded`}
          </span>
        </div>
      ) : null}
    </div>
  );
}
