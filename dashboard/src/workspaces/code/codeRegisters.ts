/**
 * The three registers the Code workspace posts to the shell's status strip:
 * the graph read, the index the graph is a picture of, and the pinned
 * selection. Each is the authority's own word plus one qualifier; none is
 * inferred from another. Pure, so the strip's wording can be pinned by a test
 * without mounting the page.
 */
import { Crosshair, Database, Waypoints } from 'lucide-react';

import type {
  CodeIndexFreshnessPayloadV1,
  GraphOverviewPayloadV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { StatusRegister } from '../../data/shell/statusRegisters.ts';
import { displayName } from './cortex.ts';

const OWNER = 'code';

export function graphRegister(
  pending: boolean,
  result: EnvelopeResult<GraphOverviewPayloadV1> | undefined,
): StatusRegister {
  const id = `${OWNER}:graph`;
  const label = 'Graph';
  const icon = Waypoints;
  if (pending) return { id, label, icon, value: 'loading', state: 'loading' };
  if (result === undefined) {
    return { id, label, icon, value: 'unknown', state: 'unknown', detail: 'no response recorded' };
  }
  if (result.outcome === 'transport') {
    return { id, label, icon, value: result.state, state: result.state, detail: result.detail };
  }
  const { envelope } = result;
  return {
    id,
    label,
    icon,
    value: envelope.domain_state,
    state: envelope.domain_state,
    detail: `${envelope.payload.totals.nodes.toLocaleString()} symbols`,
  };
}

/** `HH:MM:SS` UTC of a microsecond stamp, for a strip cell with no room for a date. */
function clockUtc(micros: number): string {
  return new Date(Math.floor(micros / 1000)).toISOString().slice(11, 19);
}

export function indexRegister(
  pending: boolean,
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined,
): StatusRegister {
  const id = `${OWNER}:index`;
  const label = 'Index';
  const icon = Database;
  if (pending) return { id, label, icon, value: 'loading', state: 'loading' };
  if (result === undefined) {
    return { id, label, icon, value: 'unknown', state: 'unknown', detail: 'no response recorded' };
  }
  if (result.outcome === 'transport') {
    return { id, label, icon, value: result.state, state: result.state, detail: result.detail };
  }
  const { envelope } = result;
  const [worktree, ...rest] = envelope.payload.worktrees;
  if (!worktree) {
    return {
      id,
      label,
      icon,
      value: envelope.domain_state,
      state: envelope.domain_state,
      detail: envelope.payload.note,
    };
  }
  // The scheduler's own staleness word leads when it gave one; the envelope
  // state is what lights the swatch either way.
  const value = worktree.staleness_state ?? envelope.domain_state;
  const sealed =
    worktree.sealed_at_micros != null ? `sealed ${clockUtc(worktree.sealed_at_micros)} UTC` : 'no sealed generation';
  const more = rest.length > 0 ? ` · +${rest.length} more ${rest.length === 1 ? 'worktree' : 'worktrees'}` : '';
  return {
    id,
    label,
    icon,
    value,
    state: envelope.domain_state,
    detail: `${sealed}${more}`,
  };
}

export function selectionRegister(
  pinned: { id: string; name?: string | null; qualified_name?: string | null; kind: string } | null,
): StatusRegister {
  const id = `${OWNER}:selection`;
  const label = 'Selection';
  const icon = Crosshair;
  if (pinned === null) {
    return { id, label, icon, value: 'none', state: 'unknown', detail: 'click a symbol to pin' };
  }
  return {
    id,
    label,
    icon,
    value: displayName(pinned),
    state: 'identity',
    detail: pinned.kind,
  };
}
