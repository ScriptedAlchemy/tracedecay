import { useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { PayloadBoundary } from "../../ui/ReadSection.tsx";
import { relativeAge } from "../../ui/time.ts";
import { cn } from "../../ui/cn";
import {
  tallied,
  useAutomationRunArtifacts,
  useAutomationRuns,
  type RunArtifactsPayload,
  type RunRow,
} from "../../data/query/automation.ts";

/**
 * The run history the automation runtime already keeps: the newest ledger
 * records from `/api/automation/runs`, each expandable to its recorded
 * artifacts and the server-computed chain-integrity verdict.
 *
 * Everything here is a reading of the ledger. The row prints the record's own
 * status word and automatic application tallies; the artifact panel prints the handler's
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
        const reading = tallied(data.runs, data.count, "runs");
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
            {/* The route serves the tail of the ledger, newest last; the
             * history reads newest first. */}
            {[...reading.rows].reverse().map((run) => (
              <RunLine key={run.run_id} run={run} />
            ))}
            {data.count === data.limit ? (
              <p className="pt-1.5 text-3xs leading-relaxed text-text-muted">
                the newest {data.limit} runs, the request cap — older records
                remain in the ledger
              </p>
            ) : null}
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
          {run.accepted_count} applied · {run.rejected_count} quarantined
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
      <RunOutcomeSummary run={run} />
      {open ? (
        <RunArtifacts runId={run.run_id} recordedKinds={run.artifact_kinds} />
      ) : null}
    </div>
  );
}

/** Optional receipts are shown only when the daemon recorded them. Missing is
 * not interpreted as an empty effect, which keeps older ledger rows truthful. */
function RunOutcomeSummary({ run }: { run: RunRow }) {
  const details: string[] = [];
  if (run.activation_policy)
    details.push(`skill policy: ${run.activation_policy}`);
  if (run.curation_policy && typeof run.curation_policy === "object") {
    const policy = run.curation_policy as Record<string, unknown>;
    if (typeof policy["decision"] === "string")
      details.push(`curation: ${policy["decision"]}`);
    if (typeof policy["effect"] === "string")
      details.push(`effect: ${policy["effect"]}`);
  }
  if (run.validation_repairs && run.validation_repairs.length > 0) {
    details.push(`${run.validation_repairs.length} validation repair`);
  }
  if (run.created_skills && run.created_skills.length > 0) {
    details.push(`${run.created_skills.length} skill activated`);
  }
  if (run.updated_skills && run.updated_skills.length > 0) {
    details.push(`${run.updated_skills.length} skill updated`);
  }
  if (run.applied_consolidations && run.applied_consolidations.length > 0) {
    details.push(`${run.applied_consolidations.length} consolidation applied`);
  }
  const deployment = run.deployment;
  if (deployment) {
    details.push(`deployment: ${deployment.status}`);
    if (deployment.retry_required) details.push("retry required");
    if (deployment.errors.length > 0)
      details.push(`${deployment.errors.length} deployment error`);
  }
  if (details.length === 0) return null;
  return (
    <p className="pb-1.5 pl-5 text-3xs leading-relaxed text-text-secondary">
      {details.join(" · ")}
    </p>
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
        <div
          key={artifact.kind}
          className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5"
        >
          <span className="shrink-0 text-2xs text-text-primary">
            {artifact.kind}
          </span>
          {artifact.summary ? (
            <span
              className="min-w-0 flex-1 truncate text-2xs text-text-muted"
              title={artifact.summary}
            >
              {artifact.summary}
            </span>
          ) : null}
          <span
            className="tabular shrink-0 font-mono text-3xs text-text-muted"
            title={artifact.sha256}
          >
            {artifact.sha256.slice(0, 12)}
          </span>
        </div>
      ))}
    </div>
  );
}
