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

type RecentTableColumn<T> = {
  header: string;
  value: (row: T) => string | number | null | undefined;
};

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

function RecentTable<T>({
  rows,
  empty,
  columns,
  rowKey,
}: {
  rows: T[];
  empty: string;
  columns: Array<RecentTableColumn<T>>;
  rowKey: (row: T, index: number) => string;
}) {
  if (!rows.length) return <EmptyState variant="dashed">{empty}</EmptyState>;
  return (
    <div className="tss-table-scroll">
      <table className="tss-table">
        <thead>
          <tr>
            {columns.map((column) => (
              <th key={column.header}>{column.header}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.slice(0, 10).map((row, index) => (
            <tr key={rowKey(row, index)}>
              {columns.map((column) => (
                <td key={column.header}>{column.value(row) || "-"}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function EventTable({ rows }: { rows: DiagnosticsRecentEvent[] }) {
  return (
    <RecentTable
      rows={rows}
      empty="No recent events"
      rowKey={(row, index) => `${row.timestamp || 0}-${index}`}
      columns={[
        { header: "Kind", value: (row) => row.event_kind },
        { header: "Tool", value: (row) => row.tool_name },
        { header: "Hook", value: (row) => row.hook_name },
        { header: "Outcome", value: (row) => row.outcome },
      ]}
    />
  );
}

function HookTable({ rows }: { rows: DiagnosticsRecentHook[] }) {
  return (
    <RecentTable
      rows={rows}
      empty="No recent hooks"
      rowKey={(row, index) => `${row.ts_unix_ms || 0}-${index}`}
      columns={[
        { header: "Agent", value: (row) => row.agent },
        { header: "Hook", value: (row) => row.hook_name },
        { header: "Tool", value: (row) => row.tool_name },
        { header: "Prompt", value: (row) => row.prompt_category },
      ]}
    />
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
