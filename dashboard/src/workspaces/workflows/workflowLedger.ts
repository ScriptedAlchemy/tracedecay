import type {
  WorkflowDefinition,
  WorkflowDefinitionDisposition,
  WorkflowDefinitionLifecycleState,
  WorkflowRunProjection,
  WorkflowRunStatus,
  WorkflowStep,
  WorkflowStepEffectOutcome,
  WorkflowStepStatus,
} from '../../contracts/index.ts';

/**
 * The Workflows ledger model: pure derivations over decoded generated
 * contracts, so the page renders and never computes.
 *
 * Every figure here is reducible to a field the daemon served. Nothing
 * extrapolates: a definition carries no timestamps, so none are derived; a run
 * carries a journal, so its timing is read from the journal's own stamps. What
 * the contracts do not carry is listed in `DEFINITION_ABSENCES` and rendered
 * as a typed absence rather than left blank.
 */

export function definitionKey(definitionId: string, definitionVersion: number): string {
  return `${definitionId}@${definitionVersion}`;
}

/* --------------------------------------------------------------------------
 * Registry
 * ----------------------------------------------------------------------- */

/** One stable workflow identity and every immutable version the registry
 * serves for it, ascending. The list route returns the whole source journal
 * (every `(definition_id, definition_version)` row), so grouping is the only
 * step between it and a registry of names. */
export interface RegistryEntry {
  readonly definitionId: string;
  readonly latest: WorkflowDefinition;
  readonly versions: readonly WorkflowDefinition[];
  /** Whether every version names the same project. A registry row whose
   * versions disagree is preserved as `AMBIGUOUS`, never collapsed onto the
   * latest version's project. */
  readonly projectAgreement: 'agree' | 'disagree';
}

export function groupRegistry(definitions: readonly WorkflowDefinition[]): RegistryEntry[] {
  const byId = new Map<string, WorkflowDefinition[]>();
  for (const definition of definitions) {
    const group = byId.get(definition.definition_id);
    if (group === undefined) byId.set(definition.definition_id, [definition]);
    else group.push(definition);
  }
  return [...byId.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([definitionId, group]) => {
      const versions = dedupeVersions(group);
      const latest = versions[versions.length - 1]!;
      const projects = new Set(versions.map((version) => version.project_id));
      return {
        definitionId,
        latest,
        versions,
        projectAgreement: projects.size === 1 ? 'agree' : 'disagree',
      };
    });
}

/** Ascending by version; a version the journal served twice keeps its first
 * occurrence, because the two rows are the same immutable record. */
function dedupeVersions(group: readonly WorkflowDefinition[]): WorkflowDefinition[] {
  const seen = new Map<number, WorkflowDefinition>();
  for (const definition of group) {
    if (!seen.has(definition.definition_version)) {
      seen.set(definition.definition_version, definition);
    }
  }
  return [...seen.values()].sort((a, b) => a.definition_version - b.definition_version);
}

/** Case-insensitive substring match on the stable identity only. Filtering
 * never reaches into digests or steps, so a hit means the name matched. */
export function filterRegistry(entries: readonly RegistryEntry[], query: string): RegistryEntry[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') return [...entries];
  return entries.filter((entry) => entry.definitionId.toLowerCase().includes(needle));
}

/* --------------------------------------------------------------------------
 * Definition shape
 * ----------------------------------------------------------------------- */

/** What the decoded step graph says about itself. Every count is over the
 * served `steps` array; nothing is inferred about behaviour. */
export interface DefinitionShape {
  readonly steps: number;
  readonly entrySteps: number;
  readonly terminalSteps: number;
  readonly fanOutSteps: number;
  readonly maxFanOutWidth: number | null;
  readonly declaredOutputs: number;
  readonly distinctOperations: number;
  /** Step ids named as a predecessor or input producer that no step in this
   * version declares. The daemon's validation owns whether that blocks
   * activation; this only reports that the decoded payload names them. */
  readonly unresolvedReferences: readonly string[];
}

export function definitionShape(definition: WorkflowDefinition): DefinitionShape {
  const ids = new Set(definition.steps.map((step) => step.step_id));
  const withSuccessors = new Set<string>();
  const unresolved = new Set<string>();
  let fanOutSteps = 0;
  let maxFanOutWidth: number | null = null;
  let declaredOutputs = 0;
  const operations = new Set<string>();
  for (const step of definition.steps) {
    operations.add(step.operation);
    declaredOutputs += step.outputs.length;
    if (step.fan_out !== null) {
      fanOutSteps += 1;
      maxFanOutWidth =
        maxFanOutWidth === null
          ? step.fan_out.max_width
          : Math.max(maxFanOutWidth, step.fan_out.max_width);
    }
    for (const predecessor of step.predecessors) {
      if (ids.has(predecessor)) withSuccessors.add(predecessor);
      else unresolved.add(predecessor);
    }
    for (const input of step.inputs) {
      if (!ids.has(input.producer_step_id)) unresolved.add(input.producer_step_id);
    }
  }
  return {
    steps: definition.steps.length,
    entrySteps: definition.steps.filter((step) => step.predecessors.length === 0).length,
    terminalSteps: definition.steps.filter((step) => !withSuccessors.has(step.step_id)).length,
    fanOutSteps,
    maxFanOutWidth,
    declaredOutputs,
    distinctOperations: operations.size,
    unresolvedReferences: [...unresolved].sort(),
  };
}

/** The neighbourhood a hovered or focused step lights: the steps it names as
 * predecessors and the steps that name it. One hop, in both directions, read
 * from the declared edges only. */
export function stepNeighbours(
  steps: readonly WorkflowStep[],
  stepId: string,
): { upstream: ReadonlySet<string>; downstream: ReadonlySet<string> } {
  const step = steps.find((candidate) => candidate.step_id === stepId);
  const upstream = new Set<string>(step?.predecessors ?? []);
  for (const input of step?.inputs ?? []) upstream.add(input.producer_step_id);
  const downstream = new Set<string>();
  for (const candidate of steps) {
    if (
      candidate.predecessors.includes(stepId) ||
      candidate.inputs.some((input) => input.producer_step_id === stepId)
    ) {
      downstream.add(candidate.step_id);
    }
  }
  return { upstream, downstream };
}

/** Fields the concept plate shows that `WorkflowDefinition` does not carry.
 * Rendered as `UNAVAILABLE` with the reason, never as a blank cell. */
export const DEFINITION_ABSENCES: readonly { readonly field: string; readonly reason: string }[] = [
  {
    field: 'created / updated',
    reason: 'the definition contract carries no timestamps; the source journal keeps them',
  },
  {
    field: 'description',
    reason: 'no summary text is part of the definition contract',
  },
  {
    field: 'step timeout / retries',
    reason: 'budgets are placement-time policy, not step fields',
  },
];

/* --------------------------------------------------------------------------
 * Version track
 * ----------------------------------------------------------------------- */

export type PinDelta = 'first' | 'same' | 'changed';

export interface VersionTrackRow {
  readonly definition: WorkflowDefinition;
  readonly policy: PinDelta;
  readonly configuration: PinDelta;
  readonly catalog: PinDelta;
  /** Steps added or removed relative to the previous version; `null` for the
   * first. A count of steps, not a diff of their contents, the daemon's
   * `diff_definition` owns which steps changed. */
  readonly stepsDelta: number | null;
}

export function versionTrack(history: readonly WorkflowDefinition[]): VersionTrackRow[] {
  const versions = dedupeVersions(history);
  return versions.map((definition, index) => {
    const previous = index === 0 ? null : versions[index - 1]!;
    const delta = (current: string, prior: string | null): PinDelta =>
      prior === null ? 'first' : current === prior ? 'same' : 'changed';
    return {
      definition,
      policy: delta(definition.pinned_policy_digest, previous?.pinned_policy_digest ?? null),
      configuration: delta(
        definition.pinned_configuration_digest,
        previous?.pinned_configuration_digest ?? null,
      ),
      catalog: delta(definition.pinned_catalog_digest, previous?.pinned_catalog_digest ?? null),
      stepsDelta: previous === null ? null : definition.steps.length - previous.steps.length,
    };
  });
}

/* --------------------------------------------------------------------------
 * Lifecycle receipts
 * ----------------------------------------------------------------------- */

export type WorkflowLifecycleAction = 'activate' | 'retire' | 'reject';

/** A disposition the daemon answered during this session, kept beside the
 * instant it was answered. The registry serves no disposition read, so this is
 * the only lifecycle state the page can show, and it is shown as what it is:
 * the daemon's answer to one compare-and-swap, at one time. */
export interface LifecycleReceipt {
  readonly action: WorkflowLifecycleAction;
  readonly expectedRevision: number;
  readonly disposition: WorkflowDefinitionDisposition;
  readonly answeredAtMillis: number;
}

export type ReceiptLedger = ReadonlyMap<string, LifecycleReceipt>;

export function recordReceipt(ledger: ReceiptLedger, receipt: LifecycleReceipt): ReceiptLedger {
  const next = new Map(ledger);
  next.set(
    definitionKey(receipt.disposition.definition_id, receipt.disposition.definition_version),
    receipt,
  );
  return next;
}

/** The transition each action asks for, in the daemon's state vocabulary. */
export function lifecycleTarget(action: WorkflowLifecycleAction): WorkflowDefinitionLifecycleState {
  switch (action) {
    case 'activate':
      return 'active';
    case 'retire':
      return 'retired';
    case 'reject':
      return 'rejected';
    default: {
      const unhandled: never = action;
      return unhandled;
    }
  }
}

/* --------------------------------------------------------------------------
 * Run projection
 * ----------------------------------------------------------------------- */

export interface RunTiming {
  /** `occurred_at` of the `admitted` event, or `null` when the served journal
   * carries none. */
  readonly admittedAt: number | null;
  readonly firstEventAt: number | null;
  readonly lastEventAt: number | null;
  readonly events: number;
  readonly terminal: boolean;
  /** Admission to last event, only once the run has reached a terminal
   * status. A running run has elapsed time, not a duration. */
  readonly durationMicros: number | null;
  /** Admission to last observed event for a run that has not finished. */
  readonly elapsedMicros: number | null;
}

export function isTerminalRunStatus(status: WorkflowRunStatus): boolean {
  switch (status) {
    case 'completed':
    case 'failed':
    case 'cancelled':
      return true;
    case 'running':
    case 'paused':
    case 'cancelling':
      return false;
    default: {
      const unhandled: never = status;
      return unhandled;
    }
  }
}

export function runTiming(projection: WorkflowRunProjection): RunTiming {
  const stamps = projection.history.map((event) => event.occurred_at);
  const admitted = projection.history.find((event) => event.event.type === 'admitted');
  const admittedAt = admitted?.occurred_at ?? null;
  const firstEventAt = stamps.length === 0 ? null : Math.min(...stamps);
  const lastEventAt = stamps.length === 0 ? null : Math.max(...stamps);
  const terminal = isTerminalRunStatus(projection.status);
  const span =
    admittedAt === null || lastEventAt === null ? null : Math.max(0, lastEventAt - admittedAt);
  return {
    admittedAt,
    firstEventAt,
    lastEventAt,
    events: projection.history.length,
    terminal,
    durationMicros: terminal ? span : null,
    elapsedMicros: terminal ? null : span,
  };
}

export interface RunStepRow {
  readonly index: number;
  readonly stepId: string;
  /** The projection's status for this step, or `absent` when the projection
   * names no entry for a step the pinned definition declares. */
  readonly status: WorkflowStepStatus | 'absent';
  /** Whether this step is declared by the pinned definition. A projection
   * entry the definition does not declare is kept and marked. */
  readonly declared: boolean;
  readonly operation: string | null;
  readonly startedAt: number | null;
  readonly settledAt: number | null;
  readonly durationMicros: number | null;
  readonly placement: { readonly backend: string; readonly model: string } | null;
  readonly effect: WorkflowStepEffectOutcome | null;
  readonly outputs: number;
}

/** Steps in the pinned definition's own order, each joined to its projection
 * entry and to the journal events that started and settled it. */
export function runStepSequence(projection: WorkflowRunProjection): RunStepRow[] {
  const started = new Map<string, number>();
  const settled = new Map<string, number>();
  for (const event of projection.history) {
    switch (event.event.type) {
      case 'step_started':
        if (!started.has(event.event.step_id)) started.set(event.event.step_id, event.occurred_at);
        break;
      case 'step_completed':
      case 'step_failed':
        settled.set(event.event.step_id, event.occurred_at);
        break;
      case 'admitted':
      case 'cancellation_requested':
      case 'cancelled':
      case 'fan_out_child_retry_rebound':
      case 'fan_out_children_released':
      case 'fan_out_children_settled':
      case 'paused':
      case 'resumed':
        break;
      default: {
        const unhandled: never = event.event;
        return unhandled;
      }
    }
  }
  const rows: RunStepRow[] = [];
  const declared = new Set<string>();
  const row = (stepId: string, operation: string | null, isDeclared: boolean): RunStepRow => {
    const entry = projection.steps[stepId];
    const startedAt = started.get(stepId) ?? null;
    const settledAt = settled.get(stepId) ?? null;
    return {
      index: rows.length + 1,
      stepId,
      status: entry === undefined ? 'absent' : entry.status,
      declared: isDeclared,
      operation,
      startedAt,
      settledAt,
      durationMicros:
        startedAt === null || settledAt === null ? null : Math.max(0, settledAt - startedAt),
      placement:
        entry?.placement_receipt == null
          ? null
          : { backend: entry.placement_receipt.backend, model: entry.placement_receipt.model },
      effect: entry?.effect_receipt?.outcome ?? null,
      outputs: entry === undefined ? 0 : Object.keys(entry.outputs).length,
    };
  };
  for (const step of projection.definition.steps) {
    declared.add(step.step_id);
    rows.push(row(step.step_id, step.operation, true));
  }
  for (const stepId of Object.keys(projection.steps).sort()) {
    if (!declared.has(stepId)) rows.push(row(stepId, null, false));
  }
  return rows;
}

export function succeededStepCount(projection: WorkflowRunProjection): number {
  return Object.values(projection.steps).filter((step) => step.status === 'succeeded').length;
}

/** `hh:mm:ss` from microseconds; sub-second spans still print as a span so a
 * fast step never reads as no time at all. `null` in, em dash out. */
export function formatDurationMicros(micros: number | null): string {
  if (micros === null || !Number.isFinite(micros)) return '—';
  const totalSeconds = Math.floor(Math.max(0, micros) / 1_000_000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const pad = (n: number) => String(n).padStart(2, '0');
  if (totalSeconds === 0 && micros > 0) return '<00:00:01';
  return `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`;
}

/* --------------------------------------------------------------------------
 * State tones, text carries the state; these only pick the lamp hue.
 * ----------------------------------------------------------------------- */

export function runStatusTone(status: WorkflowRunStatus): string {
  switch (status) {
    case 'completed':
      return 'bg-state-ready';
    case 'failed':
      return 'bg-state-error';
    case 'cancelled':
    case 'cancelling':
      return 'bg-state-cancelled';
    case 'paused':
      return 'bg-state-partial';
    case 'running':
      return 'bg-state-loading';
    default: {
      const unhandled: never = status;
      return unhandled;
    }
  }
}

export function stepStatusTone(status: WorkflowStepStatus | 'absent'): string {
  switch (status) {
    case 'succeeded':
      return 'bg-state-ready';
    case 'failed':
      return 'bg-state-error';
    case 'cancelled':
      return 'bg-state-cancelled';
    case 'running':
    case 'ready':
      return 'bg-state-loading';
    case 'blocked':
      return 'bg-state-unknown';
    case 'absent':
      return 'bg-state-unsupported-schema';
    default: {
      const unhandled: never = status;
      return unhandled;
    }
  }
}

export function lifecycleStateTone(state: WorkflowDefinitionLifecycleState): string {
  switch (state) {
    case 'active':
      return 'bg-state-ready';
    case 'candidate':
    case 'validated':
      return 'bg-state-loading';
    case 'retired':
      return 'bg-state-cancelled';
    case 'rejected':
      return 'bg-state-error';
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}
