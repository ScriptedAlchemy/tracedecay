/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Store-state panels of the hermes-lcm dashboard: the offline hero shown when
 * the server is unreachable, the stale-data / payload-error banners, and the
 * missing-store / empty-store empty panels. Extracted 1:1 from `App.tsx` —
 * DOM structure and class names unchanged.
 */

import React from "react";
import { ErrorPanel } from "../../lib/primitives";

function missingStoreTitle(scope: string | undefined): string {
  if (scope === "global") return "Global LCM database not found";
  return "Project session store not found";
}

function emptyStoreCopy(scope: string | undefined): string {
  if (scope === "project_local") {
    return "This project's active session store exists but holds no messages yet. Cursor sessions are ingested by its end-of-turn hook; Claude/Codex/Vibe/Cline transcripts are swept automatically when the MCP server or this dashboard starts. Run an agent turn in this project and refresh.";
  }
  if (scope === "profile_sharded") {
    return "This project's user-level session store exists but holds no messages yet. Cursor sessions are ingested by its end-of-turn hook; Claude/Codex/Vibe/Cline transcripts are swept automatically when the MCP server or this dashboard starts. Run an agent turn in this project and refresh.";
  }
  return "The global database exists, but it does not contain raw messages or summary nodes. Once sessions are ingested, this page will fill with timelines, compression ratios, searchable messages, and summary-node drilldowns.";
}

interface StoreStatePanelsProps {
  serverUnreachable: boolean;
  staleData: boolean;
  overviewError: string;
  data: any;
  hasLcmRows: boolean;
  onRetry: () => void;
}

export function StoreStatePanels({
  serverUnreachable,
  staleData,
  overviewError,
  data,
  hasLcmRows,
  onRetry,
}: StoreStatePanelsProps): React.ReactElement {
  return (
    <>
      {/* Unreachable server: a distinguishable error hero with retry — never
          the zeroed stats / "No data" cards that imply an empty database. */}
      {serverUnreachable ? (
        <div className="hermes-lcm-empty-panel hermes-lcm-offline" role="alert">
          <div
            className="hermes-lcm-empty-orb hermes-lcm-offline-orb"
            aria-hidden="true"
          />
          <div className="hermes-lcm-empty-copy">
            <div className="hermes-lcm-empty-kicker">Connection problem</div>
            <h2>Can't reach the tracedecay server</h2>
            <p>
              The LCM overview request failed, so no counts or timelines can be
              shown. Your data is not gone — the dashboard just can't talk to
              the server right now.
            </p>
            <div className="hermes-lcm-offline-actions">
              <button
                type="button"
                className="hermes-lcm-btn"
                onClick={onRetry}
              >
                ↻ Retry now
              </button>
              <span className="hermes-lcm-dim">{overviewError}</span>
            </div>
          </div>
        </div>
      ) : null}

      {staleData ? (
        <ErrorPanel
          error={`Refresh failed (${overviewError}) — showing previously loaded data.`}
          onRetry={onRetry}
          className="hermes-lcm-error"
        />
      ) : null}
      {data && data.error ? (
        <ErrorPanel error={data.error} className="hermes-lcm-error" />
      ) : null}

      {data && !data.exists ? (
        <div className="hermes-lcm-empty-panel">
          <div className="hermes-lcm-empty-orb" aria-hidden="true" />
          <div className="hermes-lcm-empty-copy">
            <div className="hermes-lcm-empty-kicker">
              Lossless Context Store
            </div>
            <h2>{missingStoreTitle(data.storage_scope)}</h2>
            <p>
              The dashboard can render once the session store exists. Until
              then, the search, timeline, and detail views remain unavailable.
            </p>
          </div>
        </div>
      ) : null}

      {data && data.exists && !hasLcmRows ? (
        <div className="hermes-lcm-empty-panel">
          <div className="hermes-lcm-empty-orb" aria-hidden="true" />
          <div className="hermes-lcm-empty-copy">
            <div className="hermes-lcm-empty-kicker">
              Lossless Context Store
            </div>
            <h2>No LCM sessions indexed yet</h2>
            <p>{emptyStoreCopy(data.storage_scope)}</p>
          </div>
        </div>
      ) : null}
    </>
  );
}
