import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, cn, timeAgo } from "../../lib/sdk";
import { EmptyState, ErrorPanel, Stat } from "../../lib/primitives";
import { fmt } from "../../lib/format";
import { api } from "./api";
import type {
  CodeDiagnostic,
  DiagnosticsSnapshot,
  EngineState,
  EngineStatus,
  IdleBackfillMode,
} from "./types";

const STATE_LABELS: Record<EngineState, string> = {
  unavailable: "Unavailable",
  disabled: "Disabled",
  inactive: "Inactive",
  starting: "Starting",
  indexing: "Indexing",
  ready: "Ready",
  refreshing: "Refreshing",
  crashed: "Crashed",
};

function copyCommand(command: string) {
  navigator.clipboard?.writeText(command).catch(() => undefined);
}

export default function CodeDiagnostics() {
  const [snapshot, setSnapshot] = useState<DiagnosticsSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setSnapshot(await api.overview());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const patch = useCallback(async (body: Parameters<typeof api.patchSettings>[0]) => {
    setBusy("settings");
    setError("");
    try {
      setSnapshot(await api.patchSettings(body));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy("");
    }
  }, []);

  const refresh = useCallback(async (language?: string) => {
    setBusy(language || "all");
    setError("");
    try {
      setSnapshot(language ? await api.refreshLanguage(language) : await api.refreshAll());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy("");
    }
  }, []);

  const engines = snapshot?.engines ?? [];
  const diagnosticsByFile = useMemo(
    () => groupDiagnostics(snapshot?.diagnostics ?? []),
    [snapshot?.diagnostics],
  );

  if (loading && !snapshot) {
    return <EmptyState>Loading code diagnostics...</EmptyState>;
  }

  return (
    <div className="tdcd-root">
      {error && <ErrorPanel error={error} onRetry={load} />}

      <section className="tdcd-toolbar">
        <div className="tdcd-summary">
          <Stat variant="compact" label="Errors" value={fmt(snapshot?.summary.total_errors)} />
          <Stat variant="compact" label="Warnings" value={fmt(snapshot?.summary.total_warnings)} />
          <Stat variant="compact" label="Pending" value={fmt(snapshot?.summary.pending_refreshes)} />
          <Stat variant="compact" label="Engines" value={fmt(engines.length)} />
        </div>
        <div className="tdcd-controls">
          <label className="tdcd-select-label">
            Backfill
            <select
              value={snapshot?.settings.idle_backfill || "idle"}
              onChange={(event) =>
                patch({ idle_backfill: event.currentTarget.value as IdleBackfillMode })
              }
              disabled={busy === "settings"}
            >
              <option value="idle">Idle</option>
              <option value="off">Off</option>
            </select>
          </label>
          <Button onClick={() => refresh()} disabled={Boolean(busy)}>
            Refresh all
          </Button>
        </div>
      </section>

      <section className="tdcd-layout">
        <Card className="tdcd-engines-card">
          <CardHeader>
            <CardTitle>Engines</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="tdcd-engine-table">
              {engines.map((engine) => (
                <React.Fragment key={engine.language}>
                  <EngineRow
                    engine={engine}
                    busy={busy === engine.language || busy === "settings"}
                    onToggle={(enabled) =>
                      patch({ languages: { [engine.language]: { enabled } } })
                    }
                    onConfigure={(commandOverride) =>
                      patch({
                        languages: {
                          [engine.language]: {
                            command_override: commandOverride,
                          },
                        },
                      })
                    }
                    onRefresh={() => refresh(engine.language)}
                  />
                </React.Fragment>
              ))}
            </div>
          </CardContent>
        </Card>

        <Card className="tdcd-diagnostics-card">
          <CardHeader>
            <CardTitle>Diagnostics</CardTitle>
          </CardHeader>
          <CardContent>
            {diagnosticsByFile.length === 0 ? (
              <EmptyState>No cached diagnostics.</EmptyState>
            ) : (
              <div className="tdcd-file-list">
                {diagnosticsByFile.map(([file, rows]) => (
                  <section key={file} className="tdcd-file-group">
                    <header>
                      <code>{file}</code>
                      <Badge>{rows.length}</Badge>
                    </header>
                    {rows.map((diagnostic, index) => (
                      <React.Fragment key={`${diagnosticKey(diagnostic)}-${index}`}>
                        <DiagnosticRow diagnostic={diagnostic} />
                      </React.Fragment>
                    ))}
                  </section>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </section>
    </div>
  );
}

function EngineRow({
  engine,
  busy,
  onToggle,
  onConfigure,
  onRefresh,
}: {
  engine: EngineStatus;
  busy: boolean;
  onToggle: (enabled: boolean) => void;
  onConfigure: (commandOverride: string | null) => void;
  onRefresh: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [commandDraft, setCommandDraft] = useState(engine.command);
  const hasOverride = engine.command !== engine.default_command;

  useEffect(() => {
    setCommandDraft(engine.command);
  }, [engine.command]);

  return (
    <div className="tdcd-engine-row">
      <label className="tdcd-toggle">
        <input
          type="checkbox"
          checked={engine.enabled}
          disabled={busy}
          onChange={(event) => onToggle(event.currentTarget.checked)}
        />
        <span>{engine.language}</span>
      </label>
      <code>{engine.command}</code>
      <Badge className={cn("tdcd-state", `tdcd-state-${engine.state}`)}>
        {STATE_LABELS[engine.state]}
      </Badge>
      <span className="tdcd-engine-time">
        {engine.last_diagnostic_update ? timeAgo(engine.last_diagnostic_update) : "Never"}
      </span>
      <Button size="sm" onClick={() => setExpanded((value) => !value)} disabled={busy}>
        {engine.state === "unavailable" ? "Setup" : "Configure"}
      </Button>
      <Button size="sm" onClick={onRefresh} disabled={busy || !engine.enabled}>
        Refresh
      </Button>
      {engine.last_error && <p className="tdcd-engine-error">{engine.last_error}</p>}
      {expanded && (
        <div className="tdcd-engine-setup">
          <div className="tdcd-engine-setup-copy">
            <span>Default</span>
            <code>{engine.default_command}</code>
            {engine.args.length > 0 && <code>{engine.args.join(" ")}</code>}
          </div>
          {engine.install_options.length > 0 && (
            <div className="tdcd-install-options">
              {engine.install_options.map((option) => (
                <div key={`${engine.language}-${option.label}`} className="tdcd-install-option">
                  <span>{option.label}</span>
                  <div className="tdcd-install-command">
                    <code>{option.command}</code>
                    <Button size="sm" onClick={() => copyCommand(option.command)}>
                      Copy
                    </Button>
                  </div>
                  {option.notes && <small>{option.notes}</small>}
                </div>
              ))}
            </div>
          )}
          <label className="tdcd-command-override">
            Command
            <input
              value={commandDraft}
              disabled={busy}
              placeholder={engine.default_command}
              onChange={(event) => setCommandDraft(event.currentTarget.value)}
            />
          </label>
          <div className="tdcd-engine-setup-actions">
            <Button
              size="sm"
              disabled={busy}
              onClick={() => onConfigure(commandDraft.trim() || null)}
            >
              Save
            </Button>
            {hasOverride && (
              <Button
                size="sm"
                disabled={busy}
                onClick={() => {
                  setCommandDraft(engine.default_command);
                  onConfigure(null);
                }}
              >
                Reset
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function DiagnosticRow({ diagnostic }: { diagnostic: CodeDiagnostic }) {
  return (
    <article className={cn("tdcd-diagnostic", `tdcd-diagnostic-${diagnostic.severity}`)}>
      <div className="tdcd-diagnostic-meta">
        <Badge>{diagnostic.severity}</Badge>
        <span>
          {diagnostic.line_start}
          {diagnostic.line_end !== diagnostic.line_start ? `-${diagnostic.line_end}` : ""}
        </span>
        {diagnostic.code && <code>{diagnostic.code}</code>}
        <span>{diagnostic.source}</span>
      </div>
      <p>{diagnostic.message}</p>
      {diagnostic.enclosing_node && <small>{diagnostic.enclosing_node}</small>}
    </article>
  );
}

function groupDiagnostics(rows: CodeDiagnostic[]): Array<[string, CodeDiagnostic[]]> {
  const groups = new Map<string, CodeDiagnostic[]>();
  for (const row of rows) {
    const group = groups.get(row.file) ?? [];
    group.push(row);
    groups.set(row.file, group);
  }
  return [...groups.entries()].sort((a, b) => a[0].localeCompare(b[0]));
}

function diagnosticKey(diagnostic: CodeDiagnostic): string {
  return [
    diagnostic.file,
    diagnostic.line_start,
    diagnostic.line_end,
    diagnostic.character_start ?? "",
    diagnostic.character_end ?? "",
    diagnostic.source,
    diagnostic.code ?? "",
    diagnostic.message,
  ].join(":");
}
