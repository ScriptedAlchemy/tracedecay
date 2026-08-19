import { useState } from 'react';
import type {
  WorkflowDefinition,
  WorkflowRunProjection,
  WorkflowStep,
} from '../../contracts/index.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Corners, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import type { WorkResult } from '../work/workApi.ts';
import {
  useWorkflowDefinitions,
  useWorkflowLifecycle,
  useWorkflowRun,
  type WorkflowLifecycleAction,
} from './workflowQueries.ts';

/**
 * Workflows — channel fourteen: definitions, compare-and-swap lifecycle
 * transitions, and run projections over the canonical `/application/workflow`
 * routes. Everything rendered is a decoded generated contract; refusals wear
 * the daemon's own typed state. Handoffs and run control are deliberately
 * absent: the browser never holds a bearer or mints fences/command ids.
 */

const INPUT_CLASS =
  'min-h-[36px] rounded-sm border border-edge bg-surface-1 px-2 text-2xs text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent';

const BUTTON_CLASS =
  'min-h-[36px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:text-text-muted';

export function WorkflowsPage() {
  const definitions = useWorkflowDefinitions();
  const [selected, setSelected] = useState<string | null>(null);

  const listed = definitions.data?.outcome === 'value' ? definitions.data.value : null;
  const selectedDefinition =
    listed?.find(
      (definition) =>
        `${definition.definition_id}@${definition.definition_version}` === selected,
    ) ?? null;

  return (
    <div className="min-w-0" data-testid="workflows-page">
      <WorkspaceHeader
        path="workflows"
        title="Workflows"
        note="registered definitions, lifecycle control, and run projections · canonical /application/workflow routes"
      />

      <div
        role="region"
        aria-label="Workflows content"
        tabIndex={0}
        className="relative min-w-0 overflow-x-auto p-3"
      >
        <Corners />
        <Ticks />

        <div className="flex min-w-0 flex-col gap-3">
          <DefinitionsPanel
            result={definitions.data}
            pending={definitions.isPending}
            selected={selected}
            onSelect={setSelected}
          />

          {selectedDefinition === null ? null : (
            // Keyed so lifecycle state resets when switching definitions.
            <DefinitionDetail key={selected} definition={selectedDefinition} />
          )}

          <RunPanel />
        </div>
      </div>
    </div>
  );
}

function DefinitionsPanel({
  result,
  pending,
  selected,
  onSelect,
}: {
  result: WorkResult<WorkflowDefinition[]> | undefined;
  pending: boolean;
  selected: string | null;
  onSelect: (key: string | null) => void;
}) {
  return (
    <Panel legend="Workflow definitions" elevation="well">
      <div className="flex min-w-0 flex-col gap-2">
        {pending ? (
          <StateChip kind="loading" detail="reading registered workflow definitions" />
        ) : result === undefined ? (
          <StateChip kind="unknown" detail="the definitions read returned no result" />
        ) : result.outcome === 'refused' ? (
          // A refused registry and an empty registry must never render alike.
          <StateChip kind={result.state} detail={result.detail} />
        ) : result.value.length === 0 ? (
          <StateChip
            kind="complete_zero_findings"
            detail="the daemon answered: no workflow definitions are registered in this scope"
          />
        ) : (
          <ul
            className="flex min-w-0 flex-col"
            data-workflow-definitions={result.value.length}
          >
            {result.value.map((definition) => {
              const key = `${definition.definition_id}@${definition.definition_version}`;
              const active = key === selected;
              return (
                <li key={key} className="min-w-0 border-b border-edge-subtle last:border-b-0">
                  <button
                    type="button"
                    onClick={() => onSelect(active ? null : key)}
                    aria-pressed={active}
                    className={cn(
                      'flex min-h-[44px] w-full min-w-0 flex-wrap items-baseline gap-x-3 gap-y-0.5 px-2 py-1.5 text-left hover:bg-surface-3',
                      active ? 'bg-surface-2' : undefined,
                    )}
                  >
                    <span className="td-value min-w-0 truncate text-2xs text-text-primary">
                      {definition.definition_id}
                    </span>
                    <span className="shrink-0 text-3xs text-text-muted">
                      version {definition.definition_version}
                    </span>
                    <span className="shrink-0 text-3xs text-text-muted">
                      {definition.steps.length} {definition.steps.length === 1 ? 'step' : 'steps'}
                    </span>
                    <span className="min-w-0 truncate text-3xs text-text-muted">
                      {definition.project_id}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </Panel>
  );
}

function DefinitionDetail({ definition }: { definition: WorkflowDefinition }) {
  return (
    <Panel
      legend={`Definition ${definition.definition_id} · version ${definition.definition_version}`}
      elevation="well"
    >
      <div className="flex min-w-0 flex-col gap-3">
        <dl className="grid gap-x-4 gap-y-1 text-3xs sm:grid-cols-3">
          <PinnedDigest label="pinned policy" value={definition.pinned_policy_digest} />
          <PinnedDigest
            label="pinned configuration"
            value={definition.pinned_configuration_digest}
          />
          <PinnedDigest label="pinned catalog" value={definition.pinned_catalog_digest} />
        </dl>

        <div className="min-w-0 overflow-x-auto">
          <table className="w-full min-w-0 border-collapse text-3xs">
            <caption className="td-legend py-1 text-left text-text-secondary">
              steps · operations are catalog ids, admitted on activation
            </caption>
            <thead>
              <tr>
                {['step', 'operation', 'predecessors', 'outputs', 'fan-out'].map((column) => (
                  <th
                    key={column}
                    scope="col"
                    className="border border-edge-subtle p-1 text-left text-text-muted"
                  >
                    {column}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {definition.steps.map((step) => (
                <StepRow key={step.step_id} step={step} />
              ))}
            </tbody>
          </table>
        </div>

        <LifecycleControls definition={definition} />
      </div>
    </Panel>
  );
}

function PinnedDigest({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend truncate text-text-muted">{label}</dt>
      <dd className="min-w-0 truncate font-mono text-text-secondary" title={value}>
        {value}
      </dd>
    </div>
  );
}

function StepRow({ step }: { step: WorkflowStep }) {
  return (
    <tr data-workflow-step={step.step_id}>
      <th scope="row" className="border border-edge-subtle p-1 text-left text-text-secondary">
        {step.step_id}
      </th>
      <td className="border border-edge-subtle p-1 font-mono">{step.operation}</td>
      <td className="border border-edge-subtle p-1">
        {step.predecessors.length === 0 ? '— entry step' : step.predecessors.join(', ')}
      </td>
      <td className="border border-edge-subtle p-1">
        {step.outputs.length === 0 ? '— none declared' : step.outputs.join(', ')}
      </td>
      <td className="border border-edge-subtle p-1">
        {step.fan_out === null ? 'no fan-out' : `max width ${step.fan_out.max_width}`}
      </td>
    </tr>
  );
}

/** A finished lifecycle command, read as a chip. `undefined` while nothing has
 * been sent, so controls that never ran say nothing rather than success. */
function lifecycleReading(
  result: WorkResult<{ state: string; revision: number }> | undefined,
  pending: boolean,
): { state: DomainStateKind; detail: string } | undefined {
  if (pending) return { state: 'loading', detail: 'sending' };
  if (result === undefined) return undefined;
  if (result.outcome === 'value') {
    return {
      state: 'ready',
      detail: `disposition ${result.value.state} · revision ${result.value.revision}`,
    };
  }
  return { state: result.state, detail: result.detail };
}

function LifecycleControls({ definition }: { definition: WorkflowDefinition }) {
  const lifecycle = useWorkflowLifecycle();
  const [revision, setRevision] = useState('1');
  const parsedRevision = Number.parseInt(revision, 10);
  const validRevision = Number.isInteger(parsedRevision) && parsedRevision >= 1;
  const reading = lifecycleReading(lifecycle.data, lifecycle.isPending);

  const run = (action: WorkflowLifecycleAction) => {
    if (!validRevision) return;
    lifecycle.mutate({
      action,
      definitionId: definition.definition_id,
      definitionVersion: definition.definition_version,
      expectedRevision: parsedRevision,
    });
  };

  return (
    <div className="flex min-w-0 flex-col gap-2 border-t border-edge-subtle pt-2">
      <p className="text-3xs leading-snug text-text-muted">
        Compare-and-swap against the disposition revision (a registered candidate starts at 1);
        the daemon answers with the stored disposition or a typed conflict.
      </p>
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex flex-col gap-0.5 text-2xs text-text-secondary">
          Expected revision
          <input
            value={revision}
            inputMode="numeric"
            onChange={(event) => setRevision(event.target.value)}
            className={cn(INPUT_CLASS, 'w-28')}
          />
        </label>
        {(['activate', 'retire', 'reject'] as const).map((action) => (
          <button
            key={action}
            type="button"
            disabled={!validRevision || lifecycle.isPending}
            onClick={() => run(action)}
            className={BUTTON_CLASS}
          >
            {action}
          </button>
        ))}
        {reading === undefined ? null : <StateChip kind={reading.state} detail={reading.detail} />}
      </div>
    </div>
  );
}

function RunPanel() {
  const [draft, setDraft] = useState('');
  const [runId, setRunId] = useState<string | null>(null);
  const run = useWorkflowRun(runId);

  return (
    <Panel legend="Workflow run" elevation="well">
      <div className="flex min-w-0 flex-col gap-2">
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            setRunId(draft.trim() === '' ? null : draft.trim());
          }}
        >
          <label className="flex min-w-0 flex-1 flex-col gap-0.5 text-2xs text-text-secondary">
            Run id
            <input
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="run identity, exactly as its owning surface minted it"
              className={cn(INPUT_CLASS, 'font-mono')}
            />
          </label>
          <button type="submit" disabled={draft.trim() === ''} className={BUTTON_CLASS}>
            Read run
          </button>
        </form>

        {runId === null ? null : run.isPending ? (
          <StateChip kind="loading" detail={`reading run ${runId}`} />
        ) : run.data === undefined ? (
          <StateChip kind="unknown" detail="the run read returned no result" />
        ) : run.data.outcome === 'refused' ? (
          <StateChip kind={run.data.state} detail={run.data.detail} />
        ) : (
          <RunProjection projection={run.data.value} />
        )}
      </div>
    </Panel>
  );
}

function RunProjection({ projection }: { projection: WorkflowRunProjection }) {
  const steps = Object.entries(projection.steps);
  return (
    <div className="flex min-w-0 flex-col gap-2" data-workflow-run={projection.run_id}>
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 text-2xs">
        <span className="td-value text-text-primary">{projection.run_id}</span>
        <span className="text-text-secondary">status {projection.status}</span>
        <span className="text-text-muted">sequence {projection.sequence}</span>
        <span className="text-text-muted">
          definition {projection.definition.definition_id} · version{' '}
          {projection.definition.definition_version}
        </span>
        <span className="text-text-muted">
          fan-out attempts {projection.released_fan_out_attempts.length} released ·{' '}
          {projection.settled_fan_out_attempts.length} settled
        </span>
      </div>
      <ul className="flex min-w-0 flex-col gap-1">
        {steps.map(([stepId, step]) => (
          <li
            key={stepId}
            className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5 border-l border-edge-subtle pl-2 text-3xs"
            data-workflow-run-step={stepId}
          >
            <span className="text-2xs text-text-secondary">{stepId}</span>
            <span className="text-text-primary">{step.status}</span>
            <span className="text-text-muted">
              {step.effect_receipt === null
                ? 'no effect receipt yet'
                : `effect ${step.effect_receipt.outcome}`}
            </span>
            <span className="text-text-muted">
              {step.placement_receipt === null
                ? 'no placement receipt yet'
                : `placed on ${step.placement_receipt.backend} · ${step.placement_receipt.model}`}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
