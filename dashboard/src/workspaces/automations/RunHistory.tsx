import { useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { PayloadBoundary } from "../../ui/ReadSection.tsx";
import { relativeAge } from "../../ui/time.ts";
import { cn } from "../../ui/cn";
import {
  automationRunsReading,
  useAutomationRunArtifactPayload,
  useAutomationRunArtifacts,
  useAutomationRuns,
  type RunArtifactRow,
  type RunArtifactsPayload,
  type RunRow,
} from "../../data/query/automation.ts";

/**
 * The run history the automation runtime already keeps: the newest ledger
 * records from `/api/automation/runs`, each expandable to its recorded
 * artifacts and the server-computed chain-integrity verdict.
 *
 * Everything here is a reading of the ledger. The row prints the record's own
 * status word and canonical acceptance tallies; the artifact panel prints the handler's
 * `integrity_status` rather than deciding integrity in the browser — the
 * publication chain lives beside the ledger on disk, and only the daemon can
 * compare them.
 */
export function RunHistory() {
  const runs = useAutomationRuns();
  return (
    <PayloadBoundary
      title="Run history"
      pending={runs.isPending}
      result={runs.data}
    >
      {(data) => {
        const reading = automationRunsReading(data);
        if (reading.rows.length === 0) {
          // The ledger route answers an absent ledger file with an empty list,
          // which is the truthful reading: no run has ever been recorded here.
          return reading.complete ? (
            <p className="text-2xs text-text-muted">
              no automation runs are recorded in this project&apos;s ledger
            </p>
          ) : (
            <p
              role="status"
              className="text-2xs leading-relaxed text-text-secondary"
            >
              Showing a partial list: {reading.reason}.
            </p>
          );
        }
        return (
          <div className="flex flex-col">
            {reading.complete ? null : (
              <p
                role="status"
                className="pb-1.5 text-2xs leading-relaxed text-text-secondary"
              >
                Showing a partial list: {reading.reason}.
              </p>
            )}
            {/* The route already projects its bounded ledger tail newest
             * first, so preserve that canonical order. */}
            {reading.rows.map((run) => (
              <RunLine key={run.run_id} run={run} />
            ))}
          </div>
        );
      }}
    </PayloadBoundary>
  );
}

/** One run: a disclosure row whose panel holds the artifact reading. The
 * artifact request is issued only when the row first opens. */
function RunLine({ run }: { run: RunRow }) {
  const [open, setOpen] = useState(false);
  const started = Number(run.started_at);
  const age = Number.isFinite(started)
    ? relativeAge(started, Math.floor(Date.now() / 1000))
    : null;
  return (
    <div className="border-b border-edge-subtle last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        className="flex min-h-[var(--touch-target-min)] w-full flex-wrap items-center gap-x-2 gap-y-0.5 py-1.5 text-left hover:bg-surface-1"
      >
        {open ? (
          <ChevronDown
            aria-hidden
            size={12}
            className="shrink-0 text-text-muted"
          />
        ) : (
          <ChevronRight
            aria-hidden
            size={12}
            className="shrink-0 text-text-muted"
          />
        )}
        <span className="min-w-0 flex-1 truncate text-xs">{run.task}</span>
        <span
          className={cn(
            "shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs",
            run.status === "failed" ? "text-state-error" : "text-text-muted",
          )}
        >
          {run.status}
        </span>
        <span className="tabular shrink-0 text-2xs text-text-muted">
          {run.accepted_count} accepted · {run.rejected_count} rejected
        </span>
        {/* The record's timestamp verbatim when it does not parse as epoch
         * seconds: a raw string is a truthful oddity, a blank is a lie. */}
        <span className="tabular shrink-0 text-2xs text-text-muted">
          {age ?? run.started_at}
        </span>
      </button>
      {run.error ? (
        <p className="pb-1.5 pl-5 text-2xs leading-relaxed text-state-error">
          {run.error}
        </p>
      ) : null}
      {open ? (
        <RunArtifacts runId={run.run_id} recordedKinds={run.artifact_kinds} />
      ) : null}
    </div>
  );
}

function RunArtifacts({
  runId,
  recordedKinds,
}: {
  runId: string;
  recordedKinds: readonly string[];
}) {
  // Nothing is fetched for a run whose ledger entry recorded no artifacts:
  // the list is already known to be empty from the row itself.
  const artifacts = useAutomationRunArtifacts(runId, recordedKinds.length > 0);
  return (
    <div className="mb-1.5 ml-5 border-l border-edge-subtle pl-2.5">
      {recordedKinds.length === 0 ? (
        <p className="py-1 text-2xs text-text-muted">
          this run recorded no artifacts in its ledger entry
        </p>
      ) : (
        <PayloadBoundary
          title="Artifacts"
          pending={artifacts.isPending}
          result={artifacts.data}
        >
          {(data) => <ArtifactList data={data} />}
        </PayloadBoundary>
      )}
    </div>
  );
}

function ArtifactList({ data }: { data: RunArtifactsPayload }) {
  const chain = data.artifact_chain;
  const missing = chain.expected_kinds.filter(
    (kind) => !chain.present_kinds.includes(kind),
  );
  return (
    <div className="flex flex-col gap-1 py-1">
      {/* The daemon's own verdict on whether the ledger's artifact list still
       * matches the published chain. Its words, not a green summary. */}
      <p
        className={cn(
          "text-2xs leading-relaxed",
          chain.integrity_status === "verified"
            ? "text-text-secondary"
            : "text-state-error",
        )}
      >
        chain integrity: {chain.integrity_status}
        {missing.length > 0 ? ` · not recorded: ${missing.join(", ")}` : ""}
      </p>
      {data.artifacts.map((artifact) => (
        <ArtifactLine key={artifact.kind} runId={data.run_id} artifact={artifact} />
      ))}
    </div>
  );
}

function ArtifactLine({
  runId,
  artifact,
}: {
  runId: string;
  artifact: RunArtifactRow;
}) {
  const [open, setOpen] = useState(false);
  const payload = useAutomationRunArtifactPayload(runId, artifact.kind, open);
  return (
    <div className="flex flex-col gap-1 border-l border-edge-subtle pl-2">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-left"
      >
        <span className="shrink-0 text-2xs text-text-primary">
          {open ? "Hide" : "Inspect"} {artifact.kind.replaceAll("_", " ")}
        </span>
        {artifact.summary ? (
          <span className="min-w-0 flex-1 truncate text-2xs text-text-muted" title={artifact.summary}>
            {artifact.summary}
          </span>
        ) : null}
        <span className="tabular shrink-0 font-mono text-3xs text-text-muted" title={artifact.sha256}>
          {artifact.sha256.slice(0, 12)}
        </span>
      </button>
      {open ? (
        <PayloadBoundary title={`${artifact.kind} artifact`} pending={payload.isPending} result={payload.data}>
          {(data) =>
            data.run_id === runId &&
            data.artifact.kind === artifact.kind &&
            data.artifact.path === artifact.path &&
            data.artifact.sha256 === artifact.sha256 ? (
              <pre
                aria-label={`${artifact.kind} artifact payload`}
                className="max-h-64 overflow-auto whitespace-pre-wrap break-words border border-edge-subtle bg-surface-1 p-2 font-mono text-3xs text-text-secondary"
              >
                {JSON.stringify(data.payload, null, 2)}
              </pre>
            ) : (
              <p role="status" className="text-2xs text-state-error">
                the artifact payload does not belong to this run and kind
              </p>
            )
          }
        </PayloadBoundary>
      ) : null}
    </div>
  );
}
