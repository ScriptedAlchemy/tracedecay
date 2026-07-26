import type {
  DoctorStorageFindingKind,
  StorageFindingKindStatus,
  StorageFindingSourceState,
  StoreBudgetDimension,
  StoreGrowthDimension,
  TableGrowthDimension,
  WireLegalActionRef,
} from '../../contracts/wire.ts';

const FINDING_LABELS: Record<DoctorStorageFindingKind, string> = {
  over_budget_store: 'Over-budget stores',
  orphan_store: 'Orphan stores',
  stale_branch_dbs: 'Stale branch databases',
  incident_debris_present: 'Incident debris',
  retention_backlog: 'Retention backlog',
  table_growth: 'Table growth',
};

export function storageFindingLabel(kind: DoctorStorageFindingKind): string {
  return FINDING_LABELS[kind];
}

const SOURCE_STATE_LABELS: Record<StorageFindingSourceState, string> = {
  real: 'Observed',
  unset: 'Unset',
  partial: 'Partial',
  unsupported: 'Unsupported',
};

export function storageSourcePresentation(status: StorageFindingKindStatus): {
  label: string;
  tokenClass: string;
  dotClass: string;
} {
  const label = SOURCE_STATE_LABELS[status.state];
  switch (status.state) {
    case 'real':
      return { label, tokenClass: 'text-state-ready', dotClass: 'bg-state-ready' };
    case 'unset':
      return { label, tokenClass: 'text-state-locked', dotClass: 'bg-state-locked' };
    case 'partial':
      return { label, tokenClass: 'text-state-partial', dotClass: 'bg-state-partial' };
    case 'unsupported':
      return {
        label,
        tokenClass: 'text-state-unsupported-schema',
        dotClass: 'bg-state-unsupported-schema',
      };
    default:
      return assertNever(status.state);
  }
}

export function refreshOperation(actions: WireLegalActionRef[]): string | undefined {
  return actions.find((action) => action.kind === 'refresh')?.operation;
}

/* --------------------------------------------------------------------------
 * Store telemetry dimensions (storage_telemetry_api.rs)
 *
 * The budget and growth dimensions are both *honest* read models: neither ever
 * degrades into a fabricated pass. These presenters keep that honesty in the
 * UI wording — an unset budget is a missing owner *setting* and says so, and a
 * growth read never states a delta without naming the window it covers.
 * ------------------------------------------------------------------------ */

/** Visual tone for a dimension row. `unset` is deliberately its own tone: a
 * budget the owner has not configured must not read like a budget the server
 * could not determine. */
export type DimensionTone = 'ready' | 'over' | 'unset' | 'baseline' | 'unknown';

export interface DimensionPresentation {
  /** Wire state, surfaced as a data attribute for tests and the audit. */
  state: string;
  tone: DimensionTone;
  /** The one-line honest summary rendered beside the dimension label. */
  summary: string;
  /** Provenance rendered verbatim beneath the summary (server reasons and the
   * growth coverage sentence — never paraphrased). */
  notes: string[];
  /** The owner setting a value would come from, rendered as a mono token. This
   * is what visually separates `unset` from `unknown`. */
  settingKey?: string;
}

const DIMENSION_DOT: Record<DimensionTone, string> = {
  ready: 'bg-state-ready',
  over: 'bg-state-error',
  unset: 'bg-state-locked',
  baseline: 'bg-state-partial',
  unknown: 'bg-state-unknown',
};

export function dimensionDotClass(tone: DimensionTone): string {
  return DIMENSION_DOT[tone];
}

export function budgetPresentation(budget: StoreBudgetDimension): DimensionPresentation {
  switch (budget.state) {
    case 'evaluated': {
      const { evaluation } = budget;
      if (evaluation.state === 'over_budget') {
        return {
          state: 'over_budget',
          tone: 'over',
          summary: `over budget · ${formatBytes(evaluation.observed)} of ${formatBytes(
            evaluation.soft_limit,
          )} soft limit · over by ${formatBytes(evaluation.overage)}`,
          notes: [budget.reason],
          settingKey: budget.setting_key,
        };
      }
      return {
        state: 'within_budget',
        tone: 'ready',
        summary: `within budget · ${formatBytes(evaluation.observed)} of ${formatBytes(
          evaluation.soft_limit,
        )} soft limit`,
        notes: [budget.reason],
        settingKey: budget.setting_key,
      };
    }
    case 'unset':
      return {
        state: 'unset',
        tone: 'unset',
        // A missing *setting*, not a missing feature: the summary names the
        // exact setting an owner would configure.
        summary: `no budget configured · set ${budget.setting_key}`,
        notes: [budget.reason],
        settingKey: budget.setting_key,
      };
    case 'unknown':
      return {
        state: 'unknown',
        tone: 'unknown',
        summary: 'budget could not be determined',
        notes: [budget.reason],
      };
  }
}

export function growthPresentation(growth: StoreGrowthDimension): DimensionPresentation {
  switch (growth.state) {
    case 'baseline':
      return {
        state: 'baseline',
        tone: 'baseline',
        summary: `first sample this daemon lifetime — not zero growth · ${formatBytes(
          growth.total_bytes,
        )} measured`,
        // `coverage` states that the window is since-daemon-start, not
        // historical. It is surfaced verbatim.
        notes: [growth.reason, growth.coverage],
      };
    case 'observed':
      return {
        state: 'observed',
        // An observed delta is a measurement, not a verdict: growing is not
        // itself unhealthy, so the tone stays "observed" in either direction.
        tone: 'ready',
        summary: `${formatSignedBytes(growth.growth_bytes)} over ${growth.sample_count} store-size watermarks · ${formatBytes(
          growth.first_total_bytes,
        )} → ${formatBytes(growth.current_total_bytes)}`,
        notes: [growth.coverage],
      };
    case 'unknown':
      return {
        state: 'unknown',
        tone: 'unknown',
        summary: 'growth could not be determined',
        notes: [growth.reason],
      };
  }
}

export function tableGrowthPresentation(growth: TableGrowthDimension): DimensionPresentation {
  switch (growth.state) {
    case 'observed':
      return {
        state: 'observed',
        tone: 'ready',
        summary: `${growth.significant_samples.length} significant table ${
          growth.significant_samples.length === 1 ? 'change' : 'changes'
        } · ${growth.omissions.length} below threshold`,
        notes: [],
      };
    case 'baseline_established':
      return {
        state: 'baseline_established',
        tone: 'baseline',
        summary: `no baseline yet · captured ${growth.tables_observed} ${
          growth.tables_observed === 1 ? 'table' : 'tables'
        }`,
        notes: growth.omission_reasons,
      };
    case 'unknown':
      return {
        state: 'unknown',
        tone: 'unknown',
        summary: 'Unknown · table growth could not be determined',
        notes: growth.omission_reasons,
      };
    case 'denied':
      return {
        state: 'denied',
        tone: 'unset',
        summary: 'Denied · table growth access was not authorized',
        notes: growth.omission_reasons,
      };
    case 'unsupported':
      return {
        state: 'unsupported',
        tone: 'baseline',
        summary: 'Unsupported · this store cannot measure table growth',
        notes: growth.omission_reasons,
      };
    default:
      return assertNever(growth);
  }
}

/** Every role this one store file serves. More than one means the roles share a
 * database — not that a store was reported twice. */
export function storeRolesLabel(roles: string[], role: string): string {
  const ordered = roles.length > 0 ? roles : [role];
  return ordered.length > 1 ? `${ordered.join(' · ')} (shared store file)` : ordered.join(' · ');
}

export function formatBytes(bytes: number | null): string {
  if (bytes == null) return '—';
  const sign = bytes < 0 ? '-' : '';
  const size = Math.abs(bytes);
  if (size >= 1024 * 1024 * 1024) return `${sign}${(size / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  if (size >= 1024 * 1024) return `${sign}${(size / (1024 * 1024)).toFixed(1)} MiB`;
  if (size >= 1024) return `${sign}${(size / 1024).toFixed(1)} KiB`;
  return `${sign}${size} B`;
}

/** Growth is signed on the wire; a shrinking store must read as a shrink and an
 * unchanged store must not read as "grew by nothing measured". */
export function formatSignedBytes(bytes: number): string {
  if (bytes === 0) return 'no size change';
  return `${bytes > 0 ? '+' : '−'}${formatBytes(Math.abs(bytes))}`;
}

function assertNever(value: never): never {
  throw new Error(`unsupported storage source state: ${String(value)}`);
}
