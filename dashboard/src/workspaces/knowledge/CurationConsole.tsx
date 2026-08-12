/** Agent-managed curation control plus automation-owned run observability. */
import {
  useAutomaticCurator,
  useAutomationOutcomes,
  type AutomaticCuratorRun,
  type AutomaticCuratorResult,
  type AutomationOutcomesPayload,
} from "../../data/query/automation.ts";
import { PayloadBoundary } from "../../ui/ReadSection.tsx";
import { Panel, Readout } from "../../ui/instrument.tsx";
import { RunHistory } from "../automations/RunHistory.tsx";

export function CurationConsole() {
  const curator = useAutomaticCurator();
  const outcomes = useAutomationOutcomes();

  return (
    <div
      role="region"
      aria-label="Curation console"
      tabIndex={0}
      className="flex h-full flex-col gap-3 overflow-auto p-3"
    >
      <Panel legend="Automatic memory curator" elevation="well">
        <AutomaticCuratorControl
          result={curator.data}
          pending={curator.isPending}
          writability={curator.writability}
          run={() => curator.mutate()}
        />
      </Panel>
      <Panel legend="Automatic run history" elevation="well">
        <RunHistory />
      </Panel>
      <Panel legend="Post-activation outcomes" elevation="well">
        <PayloadBoundary
          title="Post-activation outcomes"
          pending={outcomes.isPending}
          result={outcomes.data}
        >
          {(data) => <OutcomesBody data={data} />}
        </PayloadBoundary>
      </Panel>
    </div>
  );
}

function AutomaticCuratorControl({
  result,
  pending,
  writability,
  run,
}: {
  result: AutomaticCuratorResult | undefined;
  pending: boolean;
  writability: ReturnType<typeof useAutomaticCurator>["writability"];
  run: () => void;
}) {
  const unavailableReason =
    writability.state === "writable" ? null : writability.reason;
  return (
    <div className="flex flex-col gap-2">
      <p className="text-2xs leading-relaxed text-text-muted">
        Start one agent-managed review against the active project. Policy owns
        the review limit and confidence threshold; this control does not approve
        or apply individual facts.
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          disabled={pending || unavailableReason !== null}
          onClick={run}
          className="border border-edge-strong bg-surface-2 px-2.5 py-1.5 text-2xs font-medium text-text-secondary disabled:cursor-not-allowed disabled:opacity-50"
        >
          {pending ? "Running automatic curator…" : "Run automatic curator now"}
        </button>
        {writability.state === "writable" ? (
          <span className="text-3xs text-text-muted">
            target: {writability.target}
          </span>
        ) : null}
      </div>
      {unavailableReason ? (
        <p role="status" className="text-2xs leading-relaxed text-state-locked">
          {unavailableReason}
        </p>
      ) : null}
      {result ? <AutomaticCuratorSettlement result={result} /> : null}
    </div>
  );
}

function AutomaticCuratorSettlement({
  result,
}: {
  result: AutomaticCuratorResult;
}) {
  switch (result.outcome) {
    case "ok":
      return (
        <div role="status" className="flex flex-col gap-1 text-2xs leading-relaxed text-state-ready">
          <p>
            automatic curator run {result.run.run_id} settled {result.run.terminal.status}
            {result.run.committed_receipts.length > 0
              ? ` · ${result.run.committed_receipts.length.toLocaleString()} committed receipt`
              : " · no committed effects"}
          </p>
          <CommittedCurationEffects run={result.run} />
        </div>
      );
    case "partial_effect": {
      const receipt = result.problem.problem.problem.committed_receipt;
      if (receipt === null) {
        return (
          <p role="status" className="text-2xs leading-relaxed text-state-error">
            the canonical partial terminal omitted its committed effect receipt
          </p>
        );
      }
      return (
        <div role="status" className="text-2xs leading-relaxed text-state-partial">
          <p>{result.problem.problem.problem.message}</p>
          <p>
            reconciliation required · {result.problem.committed_receipts.length.toLocaleString()} canonical receipt
            {result.problem.committed_receipts.length === 1 ? "" : "s"} · admitted effect {receipt.operation} · request {receipt.request_id}
          </p>
          <CommittedCurationEffects run={result.problem} />
        </div>
      );
    }
    case "reset_required":
      return (
        <p role="status" className="text-2xs leading-relaxed text-state-error">
          reset required · {result.problem.problem.problem.message}
        </p>
      );
    case "not_dispatched":
      return result.writability.state === "writable" ? null : (
        <p role="status" className="text-2xs leading-relaxed text-state-locked">
          {result.writability.reason}
        </p>
      );
    default:
      return (
        <p role="status" className="text-2xs leading-relaxed text-state-error">
          {result.detail}
        </p>
      );
  }
}

function CommittedCurationEffects({
  run,
}: {
  run: Pick<AutomaticCuratorRun, "committed_receipts">;
}) {
  const effects = run.committed_receipts.flatMap((committed) =>
    committed.kind === "curation"
      ? committed.receipt.receipt.operation_effects
      : [],
  );
  if (effects.length === 0) return null;
  return (
    <ol aria-label="Committed curator effects" className="flex flex-col gap-1 border-l border-edge-subtle pl-2">
      {effects.map((effect) => (
        <li key={`${effect.kind}:${effect.commit.last_event_id}`} className="text-3xs text-text-secondary">
          {effect.kind === "normalize_tags" ? (
            <>
              normalize tags · fact {effect.fact_id} · {effect.commit.disposition} · event {effect.commit.last_event_id}
            </>
          ) : (
            <>
              link facts · {effect.source_fact_id} → {effect.target_fact_id} · {effect.relation.kind} · {effect.commit.disposition} · event {effect.commit.last_event_id}
            </>
          )}
        </li>
      ))}
    </ol>
  );
}

function OutcomesBody({ data }: { data: AutomationOutcomesPayload }) {
  return (
    <div className="flex flex-col gap-2">
      {data.error !== "" ? (
        <p role="status" className="text-2xs leading-relaxed text-state-partial">
          outcome rows were refreshed, but their activation snapshot is unavailable: {data.error}
        </p>
      ) : null}
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Readout label="skills" size="sm" value={data.skills.length.toLocaleString()} />
        <Readout label="facts" size="sm" value={data.facts.length.toLocaleString()} />
        <Readout
          label="snapshot"
          size="sm"
          value={data.snapshot.available ? "available" : "unavailable"}
        />
        <Readout label="generated" size="sm" value={data.generated_at.toLocaleString()} />
      </div>
      {data.skills.length > 0 ? (
        <p className="text-2xs text-text-secondary">
          skills: {summarizeOutcomes(data.skills.map((skill) => skill.verdict))}
        </p>
      ) : null}
      {data.facts.length > 0 ? (
        <p className="text-2xs text-text-secondary">
          facts: {summarizeOutcomes(data.facts.map((fact) => fact.verdict))}
        </p>
      ) : null}
    </div>
  );
}

function summarizeOutcomes(values: readonly string[]): string {
  const counts = new Map<string, number>();
  for (const value of values) counts.set(value, (counts.get(value) ?? 0) + 1);
  return [...counts]
    .map(([value, count]) => `${count} ${value.replaceAll("_", " ")}`)
    .join(" · ");
}
