import { useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import type { WorkflowDefinition, WorkflowStep } from '../../contracts/index.ts';
import { cn } from '../../ui/cn.ts';
import { Absence, GradeTag } from '../../ui/EvidenceGrade.tsx';
import { Panel, Readout } from '../../ui/instrument.tsx';
import { moveRovingFocus } from '../../ui/rovingFocus.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';
import { DigestLine, DispositionCell } from './WorkflowRegistry.tsx';
import {
  DEFINITION_ABSENCES,
  definitionKey,
  definitionShape,
  stepNeighbours,
  versionTrack,
  type PinDelta,
  type ReceiptLedger,
  type RegistryEntry,
} from './workflowLedger.ts';

/**
 * The selected definition, decoded: identity and version, what its step graph
 * declares, the three digests it pins, the version track its identity has
 * accumulated, and the step table itself. Everything is read from the served
 * `WorkflowDefinition`; what the plate shows and the contract lacks is named
 * as an absence.
 */

const CELL = 'border border-edge-subtle p-1 align-top';
const HEAD = `${CELL} td-legend text-left text-text-muted`;

export function SelectedDefinitionPanel({
  entry,
  definition,
  receipts,
}: {
  entry: RegistryEntry;
  definition: WorkflowDefinition;
  receipts: ReceiptLedger;
}) {
  const shape = useMemo(() => definitionShape(definition), [definition]);
  const receipt =
    receipts.get(definitionKey(definition.definition_id, definition.definition_version)) ?? null;
  return (
    <Panel
      legend="Selected definition"
      tone="signal"
      actions={<GradeTag grade="EXACT" source="registry" />}
      bodyClassName="flex min-w-0 flex-col gap-3 p-3"
    >
      <div
        className="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1"
        data-workflow-selected={definitionKey(definition.definition_id, definition.definition_version)}
      >
        <h3 className="td-display min-w-0 break-all text-xl text-text-primary">
          {definition.definition_id}
        </h3>
        <span
          className="td-value border border-edge-strong px-1.5 py-px text-2xs text-accent"
          data-cell="numeric"
        >
          v{definition.definition_version}
        </span>
        <DispositionCell receipt={receipt} />
      </div>

      <dl className="grid grid-cols-2 gap-x-4 gap-y-2 text-3xs sm:grid-cols-4">
        <Term label="project">
          {entry.projectAgreement === 'agree' ? (
            definition.project_id
          ) : (
            <span className="inline-flex flex-wrap items-baseline gap-1">
              <GradeTag grade="AMBIGUOUS" />
              {definition.project_id}
            </span>
          )}
        </Term>
        <Term label="versions served">{String(entry.versions.length)}</Term>
        <Term label="steps decoded">{String(shape.steps)}</Term>
        <Term label="distinct operations">{String(shape.distinctOperations)}</Term>
      </dl>

      <div className="grid gap-3 border-y border-edge-subtle py-2 sm:grid-cols-4">
        <Readout label="entry steps" value={shape.entrySteps} size="sm" />
        <Readout label="terminal steps" value={shape.terminalSteps} size="sm" />
        <Readout
          label="fan-out steps"
          value={shape.fanOutSteps}
          unit={shape.maxFanOutWidth === null ? undefined : `max width ${shape.maxFanOutWidth}`}
          size="sm"
        />
        <Readout label="declared outputs" value={shape.declaredOutputs} size="sm" />
      </div>

      {shape.unresolvedReferences.length > 0 ? (
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-3xs">
          <GradeTag grade="AMBIGUOUS" source="registry" />
          <span className="text-text-secondary">
            {shape.unresolvedReferences.length} reference
            {shape.unresolvedReferences.length === 1 ? '' : 's'} name steps this version does not
            declare: {shape.unresolvedReferences.join(', ')}. Whether that blocks activation is the
            daemon’s validation to decide.
          </span>
        </div>
      ) : null}

      <div className="flex min-w-0 flex-col gap-1.5">
        <span className="td-legend">pinned references · immutable</span>
        <DigestLine label="policy" digest={definition.pinned_policy_digest} />
        <DigestLine label="configuration" digest={definition.pinned_configuration_digest} />
        <DigestLine label="catalog" digest={definition.pinned_catalog_digest} />
        <p className="text-3xs text-text-muted">
          Digests are the pin. No dashboard route resolves a digest to its policy, configuration, or
          catalog document, so the referent is not shown here.
        </p>
      </div>

      <div className="flex min-w-0 flex-col gap-1.5 border-t border-edge-subtle pt-2">
        {DEFINITION_ABSENCES.map((absence) => (
          <Absence key={absence.field} field={absence.field} reason={absence.reason} />
        ))}
      </div>
    </Panel>
  );
}

function Term({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="td-legend">{label}</dt>
      <dd className="td-value min-w-0 break-all text-3xs text-text-secondary">{children}</dd>
    </div>
  );
}

/* --------------------------------------------------------------------------
 * Version track
 * ----------------------------------------------------------------------- */

export function VersionTrackPanel({
  definitionId,
  result,
  pending,
  selectedVersion,
  receipts,
  onSelectVersion,
}: {
  definitionId: string;
  result: WorkResult<WorkflowDefinition[]> | undefined;
  pending: boolean;
  selectedVersion: number;
  receipts: ReceiptLedger;
  onSelectVersion: (version: number) => void;
}) {
  const tableRef = useRef<HTMLTableElement | null>(null);
  const rows = useMemo(
    () => (result?.outcome === 'value' ? versionTrack(result.value) : []),
    [result],
  );
  const foreign = rows.filter((row) => row.definition.definition_id !== definitionId);
  return (
    <Panel
      legend={`Version track · ${definitionId}`}
      elevation="well"
      bodyClassName="flex min-w-0 flex-col gap-2 p-2"
      actions={
        result?.outcome === 'value' ? <GradeTag grade="EXACT" source="definition history" /> : null
      }
    >
      {pending ? (
        <StateChip kind="loading" detail="reading the definition’s version history" />
      ) : result === undefined ? (
        <StateChip kind="unknown" detail="the history read returned no result" />
      ) : result.outcome === 'refused' ? (
        <StateChip kind={result.state} detail={result.detail} />
      ) : rows.length === 0 ? (
        <StateChip
          kind="complete_zero_findings"
          detail="the daemon answered: this identity has no versions in its history"
        />
      ) : (
        <div className="min-w-0 overflow-x-auto">
          {foreign.length > 0 ? (
            <div className="mb-2 flex flex-wrap items-baseline gap-x-2 text-3xs">
              <GradeTag grade="AMBIGUOUS" source="definition history" />
              <span className="text-text-secondary">
                {foreign.length} served version{foreign.length === 1 ? '' : 's'} name a different
                identity and are listed as answered.
              </span>
            </div>
          ) : null}
          <table
            ref={tableRef}
            onKeyDown={(event: KeyboardEvent) => {
              moveRovingFocus(tableRef.current, event);
            }}
            className="w-full min-w-[30rem] border-collapse text-3xs"
            data-workflow-version-track={rows.length}
          >
            <caption className="sr-only">
              Immutable versions of {definitionId}, with pin changes against the previous version
            </caption>
            <thead>
              <tr>
                {['version', 'steps', 'policy pin', 'configuration pin', 'catalog pin', 'disposition'].map(
                  (column) => (
                    <th key={column} scope="col" className={HEAD}>
                      {column}
                    </th>
                  ),
                )}
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => {
                const version = row.definition.definition_version;
                const selected =
                  version === selectedVersion && row.definition.definition_id === definitionId;
                return (
                  <tr
                    key={definitionKey(row.definition.definition_id, version)}
                    data-workflow-version={version}
                    className={cn(selected && 'bg-surface-2')}
                  >
                    <th scope="row" className={cn(CELL, 'relative text-left')}>
                      {selected ? (
                        <span aria-hidden className="absolute inset-y-0 left-0 w-[3px] bg-accent" />
                      ) : null}
                      <button
                        type="button"
                        aria-pressed={selected}
                        onClick={() => onSelectVersion(version)}
                        className="td-value min-h-[var(--touch-target-min)] min-w-[var(--touch-target-min)] px-1 text-left text-2xs text-text-primary hover:bg-surface-3"
                      >
                        v{version}
                        {row.definition.definition_id === definitionId
                          ? ''
                          : ` · ${row.definition.definition_id}`}
                      </button>
                    </th>
                    <td className={cn(CELL, 'td-value')} data-cell="numeric">
                      {row.definition.steps.length}
                      {row.stepsDelta === null ? (
                        <span className="ml-1 text-text-muted">first</span>
                      ) : row.stepsDelta === 0 ? null : (
                        <span className="ml-1 text-text-muted">
                          {row.stepsDelta > 0 ? `+${row.stepsDelta}` : row.stepsDelta}
                        </span>
                      )}
                    </td>
                    <td className={CELL}>
                      <PinDeltaCell delta={row.policy} digest={row.definition.pinned_policy_digest} />
                    </td>
                    <td className={CELL}>
                      <PinDeltaCell
                        delta={row.configuration}
                        digest={row.definition.pinned_configuration_digest}
                      />
                    </td>
                    <td className={CELL}>
                      <PinDeltaCell delta={row.catalog} digest={row.definition.pinned_catalog_digest} />
                    </td>
                    <td className={CELL}>
                      <DispositionCell
                        receipt={
                          receipts.get(definitionKey(row.definition.definition_id, version)) ?? null
                        }
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p className="mt-1.5 text-3xs text-text-muted">
            Pin columns compare each version’s digest with the previous version’s. Which steps
            changed is the daemon’s diff (`operation.workflow.diff_definition`), not declared on this
            surface. Created, activated, and retired instants are not carried by the definition
            contract.
          </p>
        </div>
      )}
    </Panel>
  );
}

function PinDeltaCell({ delta, digest }: { delta: PinDelta; digest: string }) {
  const short = `${digest.slice(0, 14)}…${digest.slice(-6)}`;
  return (
    <span className="flex min-w-0 flex-col gap-0.5" title={digest} data-pin-delta={delta}>
      <span
        className={cn(
          'w-fit border px-1 uppercase tracking-[0.1em]',
          delta === 'changed'
            ? 'border-solid border-state-partial text-text-secondary'
            : delta === 'first'
              ? 'border-dotted border-edge-subtle text-text-muted'
              : 'border-dashed border-edge-subtle text-text-muted',
        )}
      >
        {delta}
      </span>
      <span className="td-value truncate text-3xs text-text-muted">{short}</span>
    </span>
  );
}

/* --------------------------------------------------------------------------
 * Decoded step table
 * ----------------------------------------------------------------------- */

export function DecodedStepsPanel({ definition }: { definition: WorkflowDefinition }) {
  const [hovered, setHovered] = useState<string | null>(null);
  const [pinned, setPinned] = useState<string | null>(null);
  const tableRef = useRef<HTMLTableElement | null>(null);
  const inspected = hovered ?? pinned;
  const neighbours = useMemo(
    () => (inspected === null ? null : stepNeighbours(definition.steps, inspected)),
    [definition.steps, inspected],
  );
  const inspectedStep =
    inspected === null ? null : definition.steps.find((step) => step.step_id === inspected) ?? null;

  return (
    <Panel
      legend={`Decoded step table · v${definition.definition_version}`}
      elevation="well"
      bodyClassName="flex min-w-0 flex-col gap-2 p-2"
      actions={
        <span className="td-legend shrink-0 text-text-muted" data-testid="workflow-step-inspect">
          {inspectedStep === null
            ? 'hover or focus a step to light its neighbours'
            : `${inspectedStep.step_id} · ${neighbours?.upstream.size ?? 0} upstream · ${neighbours?.downstream.size ?? 0} downstream${pinned === inspected ? ' · pinned' : ''}`}
        </span>
      }
    >
      <div className="min-w-0 overflow-x-auto">
        <table
          ref={tableRef}
          onKeyDown={(event: KeyboardEvent) => {
            moveRovingFocus(tableRef.current, event);
          }}
          onMouseLeave={() => setHovered(null)}
          className="w-full min-w-[40rem] border-collapse text-3xs"
          data-workflow-steps={definition.steps.length}
        >
          <caption className="sr-only">
            Steps of {definition.definition_id} version {definition.definition_version}; operations
            are catalog ids, admitted on activation
          </caption>
          <thead>
            <tr>
              {['#', 'step', 'operation', 'predecessors', 'inputs', 'outputs', 'fan-out'].map(
                (column) => (
                  <th key={column} scope="col" className={HEAD}>
                    {column}
                  </th>
                ),
              )}
            </tr>
          </thead>
          <tbody>
            {definition.steps.map((step, index) => (
              <StepRow
                key={step.step_id}
                index={index + 1}
                step={step}
                relation={
                  neighbours === null
                    ? 'none'
                    : step.step_id === inspected
                      ? 'self'
                      : neighbours.upstream.has(step.step_id)
                        ? 'upstream'
                        : neighbours.downstream.has(step.step_id)
                          ? 'downstream'
                          : 'unrelated'
                }
                pinned={pinned === step.step_id}
                onHover={setHovered}
                onPin={() => setPinned((current) => (current === step.step_id ? null : step.step_id))}
              />
            ))}
          </tbody>
        </table>
      </div>
      <p className="text-3xs text-text-muted">
        Timeout and retry budgets are not step fields; placement policy owns them at run time.
      </p>
    </Panel>
  );
}

type StepRelation = 'none' | 'self' | 'upstream' | 'downstream' | 'unrelated';

function StepRow({
  index,
  step,
  relation,
  pinned,
  onHover,
  onPin,
}: {
  index: number;
  step: WorkflowStep;
  relation: StepRelation;
  pinned: boolean;
  onHover: (stepId: string | null) => void;
  onPin: () => void;
}) {
  return (
    <tr
      data-workflow-step={step.step_id}
      data-step-relation={relation}
      onMouseEnter={() => onHover(step.step_id)}
      className={cn(
        relation === 'self' && 'td-raised',
        // Dimming is reinforcement; `data-step-relation` and the inspect
        // readout carry the relation for anyone not reading opacity.
        relation === 'unrelated' && 'opacity-40',
        (relation === 'upstream' || relation === 'downstream') && 'bg-surface-2',
      )}
    >
      <td className={cn(CELL, 'td-value text-text-muted')} data-cell="numeric">
        {index}
      </td>
      <th scope="row" className={cn(CELL, 'text-left')}>
        <button
          type="button"
          aria-pressed={pinned}
          onClick={onPin}
          onFocus={() => onHover(step.step_id)}
          onBlur={() => onHover(null)}
          className="td-value min-h-[var(--touch-target-min)] px-1 text-left text-2xs text-text-primary hover:bg-surface-3"
        >
          {step.step_id}
        </button>
      </th>
      <td className={cn(CELL, 'td-value break-all text-text-secondary')}>{step.operation}</td>
      <td className={CELL}>
        {step.predecessors.length === 0 ? (
          <span className="text-text-muted">— entry step</span>
        ) : (
          step.predecessors.join(', ')
        )}
      </td>
      <td className={CELL}>
        {step.inputs.length === 0 ? (
          <span className="text-text-muted">— none</span>
        ) : (
          step.inputs.map((input) => `${input.producer_step_id}.${input.output_name}`).join(', ')
        )}
      </td>
      <td className={CELL}>
        {step.outputs.length === 0 ? (
          <span className="text-text-muted">— none declared</span>
        ) : (
          step.outputs.join(', ')
        )}
      </td>
      <td className={cn(CELL, 'td-value')} data-cell="numeric">
        {step.fan_out === null ? (
          <span className="text-text-muted">no fan-out</span>
        ) : (
          `max width ${step.fan_out.max_width}`
        )}
      </td>
    </tr>
  );
}
