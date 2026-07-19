/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Top chrome of the hermes-lcm dashboard: search input, facet selects,
 * server-status pill, keyboard-shortcut hints, and the store path row.
 * Extracted 1:1 from `App.tsx` — DOM structure and class names unchanged.
 */

import React from "react";
import { short } from "./helpers";

function storageScopeLabel(scope: string | undefined): string | null {
  if (scope === "project_local") return "Project store";
  if (scope === "profile_sharded") return "User project store";
  if (scope === "global") return "Global store";
  return null;
}

interface TopBarProps {
  searchInputRef: React.RefObject<HTMLInputElement | null>;
  q: string;
  onQueryChange: (value: string) => void;
  keyboardResultCount: number;
  onSelectFirstResult: () => void;
  onClearQuery: () => void;
  role: string;
  onRoleChange: (value: string) => void;
  source: string;
  onSourceChange: (value: string) => void;
  sources: any[];
  overviewError: string;
  overviewLoading: boolean;
  chartsLoading: boolean;
  data: any;
}

export function TopBar({
  searchInputRef,
  q,
  onQueryChange,
  keyboardResultCount,
  onSelectFirstResult,
  onClearQuery,
  role,
  onRoleChange,
  source,
  onSourceChange,
  sources,
  overviewError,
  overviewLoading,
  chartsLoading,
  data,
}: TopBarProps): React.ReactElement {
  const lcmScopeLabel = data ? storageScopeLabel(data.storage_scope) : null;
  return (
    <>
      <div className="hermes-lcm-top">
        <div className="hermes-lcm-search-wrap">
          <input
            ref={searchInputRef}
            className="hermes-lcm-search"
            value={q}
            type="search"
            placeholder="Search messages and summaries"
            aria-label="Search messages and summaries"
            onChange={function (e) {
              onQueryChange(e.target.value || "");
            }}
            onKeyDown={function (e) {
              if (e.key === "ArrowDown" && keyboardResultCount) {
                e.preventDefault();
                onSelectFirstResult();
              }
            }}
          />
          {q ? (
            <button
              type="button"
              className="hermes-lcm-btn hermes-lcm-clear"
              aria-label="Clear search query"
              onClick={onClearQuery}
            >
              Clear
            </button>
          ) : null}
        </div>
        <select
          className="hermes-lcm-select"
          value={role}
          aria-label="Filter by role"
          onChange={function (e) {
            onRoleChange(e.target.value);
          }}
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
          onChange={function (e) {
            onSourceChange(e.target.value);
          }}
        >
          <option value="">All sources</option>
          {sources.map(function (s) {
            return (
              <option key={s.source} value={s.source}>
                {short(s.source, 18)}
              </option>
            );
          })}
        </select>
        <div
          className={
            "hermes-lcm-status" +
            (overviewError ? " hermes-lcm-status-err" : "")
          }
          role="status"
        >
          {overviewLoading || chartsLoading
            ? "Loading overview"
            : overviewError
              ? "Server unreachable"
              : data && data.exists
                ? "Database detected"
                : "Database missing"}
        </div>
      </div>
      <div className="hermes-lcm-shortcuts">
        <span>`/` focus search</span>
        <span>Arrow keys browse results</span>
        <span>Enter opens detail</span>
      </div>
      <div className="hermes-lcm-path">
        {data ? (
          <>
            {lcmScopeLabel ? (
              <span
                className={
                  "hermes-lcm-tag" +
                  (data.storage_scope === "global" ? "" : " hermes-lcm-tag-src")
                }
              >
                {lcmScopeLabel}
              </span>
            ) : null}
            <span>{data.path}</span>
          </>
        ) : (
          ""
        )}
      </div>
    </>
  );
}
