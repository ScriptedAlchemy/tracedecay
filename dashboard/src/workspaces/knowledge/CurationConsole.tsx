/** Read-only observability and configuration for scheduler-owned curation. */
import { mintBrowserIdempotencyKey } from "../../data/identity.ts";
import {
  useAutomationOutcomes,
  type AutomationOutcomesPayload,
} from "../../data/query/automation.ts";
import { PayloadBoundary } from "../../ui/ReadSection.tsx";
import { Panel, Readout } from "../../ui/instrument.tsx";
import { StateChip } from "../../ui/StateChip.tsx";
import {
  scopeWriteSentence,
  type ScopeWritability,
} from "../../data/scope/store.ts";
import {
  useCurationConfig,
  useCurationConfigPatch,
  useCurationRuns,
  type CurationConfigMutation,
  type CurationConfigPayload,
  type CurationConfigWriteResult,
  type CurationRunRecord,
  type CurationRunsPayload,
} from "../../data/query/memory.ts";
import { runStatusState } from "./memoryModel.ts";

export function CurationConsole() {
  const runs = useCurationRuns();
  const outcomes = useAutomationOutcomes();
  const config = useCurationConfig();
  const patch = useCurationConfigPatch();

  return (
    <div
      role="region"
      aria-label="Curation console"
      tabIndex={0}
      className="flex h-full flex-col gap-3 overflow-auto p-3"
    >
      <Panel legend="Automatic run history" elevation="well">
        <PayloadBoundary
          title="Automatic run history"
          pending={runs.isPending}
          result={runs.data}
        >
          {(data) => <RunsBody data={data} />}
        </PayloadBoundary>
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
      <Panel legend="Automation configuration">
        <PayloadBoundary
          title="Automation configuration"
          pending={config.isPending}
          result={config.data}
        >
          {(data) => (
            <ConfigBody
              data={data}
              writability={patch.writability}
              pending={patch.isPending}
              failure={configWriteFailure(patch.data)}
              onPatch={(next) => patch.mutate(next)}
            />
          )}
        </PayloadBoundary>
      </Panel>
    </div>
  );
}

function RunsBody({ data }: { data: CurationRunsPayload }) {
  if (data.error !== "") {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        the automatic run ledger could not be read: {data.error}
      </p>
    );
  }
  if (data.records.length === 0) {
    return (
      <p className="text-2xs leading-relaxed text-text-muted">
        the automatic run ledger is readable and holds no records
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="text-3xs leading-relaxed text-text-muted">
        These are scheduler-owned automatic runs recorded by the automation
        authority.
      </p>
      <div className="flex flex-wrap items-end gap-4">
        <Readout
          label="runs"
          size="sm"
          value={data.records.length.toLocaleString()}
        />
        <Readout
          label="non-completed"
          size="sm"
          value={data.records
            .filter((record) => record.status !== "completed")
            .length.toLocaleString()}
        />
      </div>
      <ol
        role="region"
        aria-label="Automatic run records"
        tabIndex={0}
        className="flex max-h-80 flex-col gap-1.5 overflow-auto"
      >
        {data.records.map((record) => (
          <RunRecordLine key={record.run_id} record={record} />
        ))}
      </ol>
    </div>
  );
}

function RunRecordLine({ record }: { record: CurationRunRecord }) {
  const details: string[] = [
    `${record.accepted_count.toLocaleString()} applied`,
    `${record.rejected_count.toLocaleString()} quarantined`,
    `${record.skipped_count.toLocaleString()} skipped`,
  ];
  if (record.validation_repairs && record.validation_repairs.length > 0) {
    details.push(`${record.validation_repairs.length} validation repair`);
  }
  if (record.deployment) {
    details.push(`deployment ${record.deployment.status}`);
    if (record.deployment.retry_required) details.push("retry required");
  }
  return (
    <li className="flex flex-col gap-0.5 border-l-2 border-edge-subtle pl-2">
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-3xs text-text-muted">
        <StateChip
          kind={runStatusState(record.status)}
          detail={record.status}
        />
        <span className="text-text-secondary">{record.task}</span>
        <span>{record.trigger}</span>
        <span>{record.backend}</span>
        {record.model ? (
          <span>{record.model}</span>
        ) : (
          <span>model unrecorded</span>
        )}
      </p>
      <p className="td-value text-3xs text-text-muted" data-cell="numeric">
        {record.started_at} → {record.completed_at} · {details.join(" · ")}
      </p>
      {record.error ? (
        <p className="text-2xs leading-relaxed text-state-error">
          {record.error}
        </p>
      ) : null}
    </li>
  );
}

function OutcomesBody({ data }: { data: AutomationOutcomesPayload }) {
  if (data.error !== "") {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        automatic outcomes could not be refreshed: {data.error}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Readout
          label="skills"
          size="sm"
          value={data.skills.length.toLocaleString()}
        />
        <Readout
          label="facts"
          size="sm"
          value={data.facts.length.toLocaleString()}
        />
        <Readout
          label="snapshot"
          size="sm"
          value={data.snapshot.available ? "available" : "unavailable"}
        />
        <Readout
          label="generated"
          size="sm"
          value={data.generated_at.toLocaleString()}
        />
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

function ConfigBody({
  data,
  writability,
  pending,
  failure,
  onPatch,
}: {
  data: CurationConfigPayload;
  writability: ScopeWritability;
  pending: boolean;
  failure: string | null;
  onPatch: (patch: CurationConfigMutation) => void;
}) {
  const blocked = writability.state !== "writable";
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <StateChip
          kind={data.backend_availability.available ? "ready" : "unavailable"}
          detail={
            data.backend_availability.available
              ? (data.backend_availability.executable ??
                data.backend_availability.backend)
              : (data.backend_availability.reason ??
                `backend ${data.backend_availability.backend}`)
          }
        />
        <span className="text-2xs text-text-secondary">
          source {data.source} · revision {data.configuration_revision_id}
        </span>
      </div>
      <p className="text-3xs leading-relaxed text-text-muted">
        The daemon validates and applies memory operations and activates skills
        automatically. This control only enables or disables future scheduler
        runs; it does not approve individual facts or skills.
      </p>
      <label className="flex items-center gap-1.5 text-2xs">
        <input
          type="checkbox"
          className="td-check"
          checked={data.effective.enabled}
          disabled={pending || blocked}
          aria-describedby="curation-config-scope"
          onChange={(event) =>
            onPatch({
              enabled: event.target.checked,
              expected_revision_id: data.configuration_revision_id,
              idempotency_key: mintBrowserIdempotencyKey("dashboard-settings"),
            })
          }
        />
        <span>
          <span className="text-text-primary">Automation enabled</span>
          <span className="block text-3xs text-text-muted">
            the scheduler runs automatic validation and application when tasks
            come due
          </span>
        </span>
      </label>
      <p
        id="curation-config-scope"
        data-scope-writability={writability.state}
        className="text-2xs text-text-secondary"
      >
        {scopeWriteSentence(writability, {
          writable: (target) => `Applies to ${target}.`,
        })}
      </p>
      {failure ? (
        <p role="status" className="text-2xs text-text-secondary">
          {failure}
        </p>
      ) : null}
      <dl className="grid grid-cols-1 gap-x-3 gap-y-0.5 border-t border-edge-subtle pt-2 text-2xs sm:grid-cols-3">
        {(["memory_curator", "session_reflector", "skill_writer"] as const).map(
          (task) => {
            const taskConfig = data.effective.tasks[task];
            return (
              <div key={task} className="flex flex-col gap-0.5">
                <dt className="td-legend">{task}</dt>
                <dd className="text-3xs text-text-secondary">
                  {taskConfig.enabled ? "enabled" : "disabled"}
                  {taskConfig.schedule === null
                    ? " · no schedule"
                    : ` · ${taskConfig.schedule}`}
                  {taskConfig.interval_secs == null
                    ? " · interval unset"
                    : ` · every ${taskConfig.interval_secs.toLocaleString()}s`}
                </dd>
              </div>
            );
          },
        )}
      </dl>
    </div>
  );
}

export function configWriteFailure(
  result: CurationConfigWriteResult | undefined,
): string | null {
  if (result === undefined || result.outcome === "ok") return null;
  switch (result.outcome) {
    case "offline":
      return "The daemon did not answer, so the automation setting was not changed.";
    case "unauthorized":
      return "The daemon accepted no identity for the automation setting change.";
    case "denied":
      return "This identity is not permitted to change the automation setting.";
    case "error":
      return `The daemon refused the automation setting change (${result.detail}).`;
    case "unsupported_schema":
      return "The daemon answered in a shape this dashboard cannot read, so whether the automation setting changed is unknown — reload to re-read it.";
    case "unavailable":
      return `The automation setting was not changed: ${result.reason ?? result.status}.`;
    case "read_only_scope":
      return `The automation setting was not changed: ${result.refusal.detail}.`;
    case "not_dispatched":
      return scopeWriteSentence(result.writability, {
        writable: (target) =>
          `Nothing was sent, though writes to ${target} are accepted — reload to re-read the automation setting.`,
        refused: (reason) => `Nothing was sent. ${reason}`,
      });
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
