import React from "react";
import { Card, CardContent, CardHeader, CardTitle } from "../../lib/sdk";
import { BarList, EmptyState, Stat } from "../../lib/primitives";
import { fmtTokens } from "./logic";
import type {
  DiagnosticsCountRow,
  DiagnosticsRecentEvent,
  DiagnosticsRecentHook,
  DiagnosticsResponse,
} from "./types";

function fmtRatio(value: number | undefined): string {
  return value == null ? "0.00" : value.toFixed(2);
}

function rowLabel(row: DiagnosticsCountRow, key: string): string {
  return String(row[key] || "(none)");
}

function countRows(
  rows: DiagnosticsCountRow[],
  key: string,
): Array<{ label: string; value: number }> {
  return rows
    .slice(0, 12)
    .map((row) => ({ label: rowLabel(row, key), value: Number(row.count) || 0 }));
}

function EventTable({ rows }: { rows: DiagnosticsRecentEvent[] }) {
  if (!rows.length) return <EmptyState variant="dashed">No recent events</EmptyState>;
  return (
    <div className="tss-table-scroll">
      <table className="tss-table">
        <thead>
          <tr>
            <th>Kind</th>
            <th>Tool</th>
            <th>Hook</th>
            <th>Outcome</th>
          </tr>
        </thead>
        <tbody>
          {rows.slice(0, 10).map((row, index) => (
            <tr key={`${row.timestamp || 0}-${index}`}>
              <td>{row.event_kind || "-"}</td>
              <td>{row.tool_name || "-"}</td>
              <td>{row.hook_name || "-"}</td>
              <td>{row.outcome || "-"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function HookTable({ rows }: { rows: DiagnosticsRecentHook[] }) {
  if (!rows.length) return <EmptyState variant="dashed">No recent hooks</EmptyState>;
  return (
    <div className="tss-table-scroll">
      <table className="tss-table">
        <thead>
          <tr>
            <th>Agent</th>
            <th>Hook</th>
            <th>Tool</th>
            <th>Prompt</th>
          </tr>
        </thead>
        <tbody>
          {rows.slice(0, 10).map((row, index) => (
            <tr key={`${row.ts_unix_ms || 0}-${index}`}>
              <td>{row.agent || "-"}</td>
              <td>{row.hook_name || "-"}</td>
              <td>{row.tool_name || "-"}</td>
              <td>{row.prompt_category || "-"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export default function DiagnosticsPanel({ data }: { data: DiagnosticsResponse | null }) {
  if (!data) return <EmptyState variant="dashed">Loading diagnostics...</EmptyState>;

  return (
    <div className="tss-grid">
      <div className="tss-stat-row">
        <Stat label="messages" value={fmtTokens(data.message_count)} />
        <Stat label="events" value={fmtTokens(data.event_count)} />
        <Stat label="MCP tools" value={fmtTokens(data.mcp_tool_call_count)} />
        <Stat label="TraceDecay calls" value={fmtTokens(data.tracedecay_call_count)} />
        <Stat label="hooks" value={fmtTokens(data.hook_call_count)} />
      </div>

      <div className="tss-stat-row">
        <Stat
          label="tools / message"
          value={fmtRatio(data.ratios?.tool_calls_per_message)}
        />
        <Stat
          label="MCP / message"
          value={fmtRatio(data.ratios?.mcp_tool_calls_per_message)}
        />
        <Stat
          label="hooks / message"
          value={fmtRatio(data.ratios?.hook_calls_per_message)}
        />
        <Stat label="events / hour" value={fmtRatio(data.events_per_hour)} />
      </div>

      <div className="tss-card-grid">
        <Card>
          <CardHeader>
            <CardTitle>Tool Categories</CardTitle>
          </CardHeader>
          <CardContent>
            <BarList rows={countRows(data.by_tool_category, "tool_category")} keyName="label" />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>MCP Tools</CardTitle>
          </CardHeader>
          <CardContent>
            <BarList rows={countRows(data.by_mcp_tool, "tool_name")} keyName="label" />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Hooks</CardTitle>
          </CardHeader>
          <CardContent>
            <BarList rows={countRows(data.by_hook, "hook_name")} keyName="label" />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Prompt Categories</CardTitle>
          </CardHeader>
          <CardContent>
            <BarList
              rows={countRows(data.by_prompt_category || [], "prompt_category")}
              keyName="label"
            />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Outcomes</CardTitle>
          </CardHeader>
          <CardContent>
            <BarList rows={countRows(data.by_outcome, "outcome")} keyName="label" />
          </CardContent>
        </Card>
      </div>

      <div className="tss-card-grid">
        <Card>
          <CardHeader>
            <CardTitle>Recent Events</CardTitle>
          </CardHeader>
          <CardContent>
            <EventTable rows={data.recent_events || []} />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Recent Hooks</CardTitle>
          </CardHeader>
          <CardContent>
            <HookTable rows={data.recent_hooks || []} />
          </CardContent>
        </Card>
      </div>

      <div className="tss-meta-strip">
        <span className="tss-meta-item">source: {data.source}</span>
      </div>
    </div>
  );
}
