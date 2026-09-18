import {
  DeliveryAttentionSourceV1Schema,
  DeliveryProviderStateV1Schema,
  DeliveryPullRequestStateV1Schema,
  DeliveryReviewLifecycleV1Schema,
  type DeliveryAttentionSourceV1,
  type DeliveryProviderStateV1,
  type DeliveryPullRequestStateV1,
  type DeliveryReviewLifecycleV1,
} from '../../contracts/generated.ts';

/**
 * Delivery's URL-addressable state. Every workspace mode, scope, filter and
 * selection lives here so a pasted link restores the same inbox row, umbrella,
 * journey episode or review thread, and so the DOM, the field and the exact
 * table fallback all read one location instead of three.
 */
export const DELIVERY_MODES = ['inbox', 'umbrella', 'journey', 'review'] as const;
export type DeliveryMode = (typeof DELIVERY_MODES)[number];

export const DELIVERY_LAYOUTS = ['field', 'table'] as const;
export type DeliveryLayout = (typeof DELIVERY_LAYOUTS)[number];

export interface DeliveryLocation {
  readonly mode: DeliveryMode;
  /** Exact list/tree/table fallback instead of the SVG field. */
  readonly layout: DeliveryLayout;
  readonly project: string | null;
  readonly pullRequest: string | null;
  readonly umbrella: string | null;
  readonly attention: DeliveryAttentionSourceV1 | null;
  readonly status: DeliveryPullRequestStateV1 | 'draft' | null;
  readonly provider: DeliveryProviderStateV1 | null;
  readonly unresolvedOnly: boolean;
  readonly evidence: string | null;
  readonly episode: string | null;
  readonly thread: string | null;
  readonly check: string | null;
  readonly lane: DeliveryReviewLifecycleV1 | null;
}

const PARAMS = {
  mode: 'mode',
  layout: 'layout',
  project: 'project',
  pullRequest: 'pr',
  umbrella: 'umbrella',
  attention: 'attention',
  status: 'status',
  provider: 'provider',
  unresolvedOnly: 'unresolved',
  evidence: 'evidence',
  episode: 'episode',
  thread: 'thread',
  check: 'check',
  lane: 'lane',
} as const;

function oneOf<T extends string>(
  value: string | null,
  accepted: readonly T[],
): T | null {
  if (value === null) return null;
  return accepted.find((candidate) => candidate === value) ?? null;
}

function enumValue<T extends string>(
  value: string | null,
  schema: { options: readonly T[] },
): T | null {
  return oneOf(value, schema.options);
}

export function readDeliveryLocation(params: URLSearchParams): DeliveryLocation {
  const status = params.get(PARAMS.status);
  return {
    mode: oneOf(params.get(PARAMS.mode), DELIVERY_MODES) ?? 'inbox',
    layout: oneOf(params.get(PARAMS.layout), DELIVERY_LAYOUTS) ?? 'field',
    project: params.get(PARAMS.project),
    pullRequest: params.get(PARAMS.pullRequest),
    umbrella: params.get(PARAMS.umbrella),
    attention: enumValue(params.get(PARAMS.attention), DeliveryAttentionSourceV1Schema),
    status:
      status === 'draft'
        ? 'draft'
        : enumValue(status, DeliveryPullRequestStateV1Schema),
    provider: enumValue(params.get(PARAMS.provider), DeliveryProviderStateV1Schema),
    unresolvedOnly: params.get(PARAMS.unresolvedOnly) === '1',
    evidence: params.get(PARAMS.evidence),
    episode: params.get(PARAMS.episode),
    thread: params.get(PARAMS.thread),
    check: params.get(PARAMS.check),
    lane: enumValue(params.get(PARAMS.lane), DeliveryReviewLifecycleV1Schema),
  };
}

export type DeliveryLocationPatch = Partial<DeliveryLocation>;

/**
 * Writes a patch over the current params. Defaults are written as absence so
 * the canonical inbox URL stays `/delivery`. Keys the patch does not name are
 * left untouched, so a mode change never drops a selection it did not own.
 */
export function writeDeliveryLocation(
  current: URLSearchParams,
  patch: DeliveryLocationPatch,
): URLSearchParams {
  const next = new URLSearchParams(current);
  const set = (key: string, value: string | null) => {
    if (value === null || value === '') next.delete(key);
    else next.set(key, value);
  };
  if (patch.mode !== undefined) set(PARAMS.mode, patch.mode === 'inbox' ? null : patch.mode);
  if (patch.layout !== undefined) {
    set(PARAMS.layout, patch.layout === 'field' ? null : patch.layout);
  }
  if (patch.project !== undefined) set(PARAMS.project, patch.project);
  if (patch.pullRequest !== undefined) set(PARAMS.pullRequest, patch.pullRequest);
  if (patch.umbrella !== undefined) set(PARAMS.umbrella, patch.umbrella);
  if (patch.attention !== undefined) set(PARAMS.attention, patch.attention);
  if (patch.status !== undefined) set(PARAMS.status, patch.status);
  if (patch.provider !== undefined) set(PARAMS.provider, patch.provider);
  if (patch.unresolvedOnly !== undefined) {
    set(PARAMS.unresolvedOnly, patch.unresolvedOnly ? '1' : null);
  }
  if (patch.evidence !== undefined) set(PARAMS.evidence, patch.evidence);
  if (patch.episode !== undefined) set(PARAMS.episode, patch.episode);
  if (patch.thread !== undefined) set(PARAMS.thread, patch.thread);
  if (patch.check !== undefined) set(PARAMS.check, patch.check);
  if (patch.lane !== undefined) set(PARAMS.lane, patch.lane);
  return next;
}

/** A full `/delivery?…` href for a patch over the current location, the
 * anchor form of `writeDeliveryLocation`, for links rather than handlers. */
export function deliveryHref(current: URLSearchParams, patch: DeliveryLocationPatch): string {
  const query = writeDeliveryLocation(current, patch).toString();
  return query === '' ? '/delivery' : `/delivery?${query}`;
}

export function modeLabel(mode: DeliveryMode): string {
  switch (mode) {
    case 'inbox':
      return 'Inbox';
    case 'umbrella':
      return 'Umbrella';
    case 'journey':
      return 'Journey';
    case 'review':
      return 'Review';
    default: {
      const unhandled: never = mode;
      return unhandled;
    }
  }
}
