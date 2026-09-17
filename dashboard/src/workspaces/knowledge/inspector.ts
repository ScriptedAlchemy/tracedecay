/**
 * The fact inspector's pure readings: what state each rung of the detail
 * ladder is in, in the taxonomy's own words.
 *
 * The concept plate for this workspace shows a "detail availability" list.
 * The shipping product has real authorities for some rungs and none for
 * others, and the ladder must say which is which: a rung the daemon serves
 * reports the daemon's state; a rung no authority serves reports
 * `unavailable` with the reason, and is never drawn as a ticked box.
 */
import {
  assertNever,
  type MemoryFactDetailPayloadV1,
  type MemoryFactRowV1,
  type MemoryReadStatusV1,
  type PayloadAccessState,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { PayloadResult } from '../../data/query/payload.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { TrustHistoryPayload } from '../../data/query/memory.ts';

/** How the reader arrived at this fact. Inspection previews the bounded
 * overview row; selection reads the canonical authorities. */
export type InspectorMode = 'inspecting' | 'selected';

/** The payload-access state as a typed chip. `eligible` is content shown, so
 * it maps to `ready`; every other state names why the content is withheld. */
export function payloadAccessState(access: PayloadAccessState): {
  kind: DomainStateKind;
  detail: string;
} {
  switch (access) {
    case 'eligible':
      return { kind: 'ready', detail: 'payload eligible' };
    case 'redacted':
      return { kind: 'redacted', detail: 'content withheld by policy; identity retained' };
    case 'quarantined':
      return { kind: 'locked', detail: 'payload quarantined; content withheld' };
    case 'deleted':
      return { kind: 'unavailable', detail: 'payload deleted; identity retained' };
    case 'retention_expired':
      return { kind: 'unavailable', detail: 'payload past its retention window' };
    case 'unavailable':
      return { kind: 'unavailable', detail: 'payload unavailable' };
    case 'ambiguous':
      return { kind: 'conflicting', detail: 'more than one payload candidate; none chosen' };
    default:
      return assertNever(access);
  }
}

export interface LadderRung {
  readonly id:
    | 'canonical_detail'
    | 'payload_access'
    | 'source_label'
    | 'trust_history'
    | 'graph_relations'
    | 'source_verification'
    | 'geometry';
  readonly label: string;
  readonly state: DomainStateKind;
  readonly detail: string;
}

export interface LadderInput {
  readonly mode: InspectorMode;
  /** The bounded overview row, when the fact is in the loaded slice. */
  readonly row: MemoryFactRowV1 | undefined;
  /** The canonical detail read, only issued for a selected fact. */
  readonly detail:
    | { pending: true }
    | { pending: false; result: EnvelopeResult<MemoryFactDetailPayloadV1> | undefined }
    | null;
  /** The trust audit read, only issued for a selected fact. */
  readonly history:
    | { pending: true }
    | { pending: false; result: PayloadResult<TrustHistoryPayload> | undefined }
    | null;
  /** Relations touching this fact in the drawn graph, and the graph sub-read. */
  readonly relations: number | null;
  readonly graphRead: MemoryReadStatusV1 | undefined;
}

/** The canonical detail rung. Distinguishes a detail still loading, a detail
 * served, a fact the store does not hold, a daemon-side error carried inside
 * the envelope, and a transport failure. */
function canonicalDetailRung(input: LadderInput): LadderRung {
  const label = 'canonical detail';
  if (input.mode === 'inspecting' || input.detail === null) {
    return {
      id: 'canonical_detail',
      label,
      state: 'unknown',
      detail: 'bounded overview row shown; select the fact to read its canonical detail',
    };
  }
  if (input.detail.pending) {
    return { id: 'canonical_detail', label, state: 'loading', detail: 'reading canonical fact detail' };
  }
  const result = input.detail.result;
  if (!result) {
    return { id: 'canonical_detail', label, state: 'unknown', detail: 'detail read has not answered' };
  }
  if (result.outcome === 'transport') {
    return {
      id: 'canonical_detail',
      label,
      state: result.state,
      detail: result.detail ?? 'detail transport failed',
    };
  }
  const payload = result.envelope.payload;
  if (payload == null || payload.fact == null) {
    // The route answers `complete_zero_findings` with a null payload for an
    // identity the store does not hold; any other empty envelope carries the
    // daemon's own state and is reported in its words.
    return {
      id: 'canonical_detail',
      label,
      state:
        result.envelope.domain_state === 'complete_zero_findings'
          ? 'unavailable'
          : result.envelope.domain_state,
      detail:
        payload?.error && payload.error !== ''
          ? payload.error
          : 'the store holds no fact under this identity in the current scope',
    };
  }
  if (payload.error !== '') {
    return { id: 'canonical_detail', label, state: 'partial', detail: payload.error };
  }
  return { id: 'canonical_detail', label, state: 'ready', detail: 'canonical row served' };
}

function trustHistoryRung(input: LadderInput): LadderRung {
  const label = 'trust history';
  if (input.mode === 'inspecting' || input.history === null) {
    return { id: 'trust_history', label, state: 'unknown', detail: 'loads on selection' };
  }
  if (input.history.pending) {
    return { id: 'trust_history', label, state: 'loading', detail: 'reading the feedback audit' };
  }
  const result = input.history.result;
  if (!result) {
    return { id: 'trust_history', label, state: 'unknown', detail: 'audit read has not answered' };
  }
  if (result.outcome !== 'ok') {
    return {
      id: 'trust_history',
      label,
      state: result.outcome,
      detail:
        result.outcome === 'error'
          ? result.detail
          : result.outcome === 'unavailable'
            ? (result.reason ?? result.status)
            : 'audit read failed',
    };
  }
  if (result.data.error !== '') {
    return { id: 'trust_history', label, state: 'error', detail: result.data.error };
  }
  const count = result.data.trust_history.length;
  return {
    id: 'trust_history',
    label,
    state: result.data.completeness === 'complete' ? (count === 0 ? 'complete_zero_findings' : 'ready') : 'partial',
    detail:
      result.data.completeness === 'complete'
        ? `${count.toLocaleString()} feedback ${count === 1 ? 'event' : 'events'}, complete`
        : `${count.toLocaleString()} of a partial window; a continuation exists`,
  };
}

function graphRung(input: LadderInput): LadderRung {
  const label = 'memory graph relations';
  const read = input.graphRead;
  if (!read) {
    return { id: 'graph_relations', label, state: 'unknown', detail: 'graph sub-read not reported' };
  }
  const complete = read.state === 'ready' || read.state === 'complete_zero_findings';
  const count = input.relations;
  const drawn =
    count == null
      ? 'fact is not among the drawn roots'
      : `${count.toLocaleString()} ${count === 1 ? 'relation' : 'relations'} drawn`;
  if (!complete) {
    return {
      id: 'graph_relations',
      label,
      state: read.state,
      detail: `${drawn}; ${read.error ?? read.code ?? 'graph read incomplete'}`,
    };
  }
  return { id: 'graph_relations', label, state: 'ready', detail: drawn };
}

/** Every rung of the ladder, in reading order. */
export function detailLadder(input: LadderInput): LadderRung[] {
  const row = input.row;
  const detailRow =
    input.detail !== null && !input.detail.pending && input.detail.result?.outcome === 'envelope'
      ? input.detail.result.envelope.payload?.fact ?? undefined
      : undefined;
  const subject = detailRow ?? row;
  const access = subject ? payloadAccessState(subject.payload_access) : null;
  const sourceLabel = subject?.source_label ?? null;
  return [
    canonicalDetailRung(input),
    access
      ? { id: 'payload_access', label: 'payload access', state: access.kind, detail: access.detail }
      : { id: 'payload_access', label: 'payload access', state: 'unknown', detail: 'no row to read access from' },
    sourceLabel
      ? { id: 'source_label', label: 'source label', state: 'ready', detail: sourceLabel }
      : { id: 'source_label', label: 'source label', state: 'unknown', detail: 'no source label recorded on this fact' },
    trustHistoryRung(input),
    graphRung(input),
    {
      id: 'source_verification',
      label: 'source verification',
      state: 'unavailable',
      detail: 'no verification authority is served; symbol, document, test and runtime checks are not claimed',
    },
    {
      id: 'geometry',
      label: 'geometry membership',
      state: 'unknown',
      detail: 'read by the separate Geometry camera; not derived here',
    },
  ];
}
