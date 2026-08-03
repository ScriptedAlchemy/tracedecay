import type { ReactNode } from 'react';
import type { DashboardEnvelopeV1 } from '../contracts/generated.ts';
import type { EnvelopeResult } from '../data/query/envelope.ts';
import type { LegacyResult } from '../data/query/legacy.ts';
import { Corners } from './instrument.tsx';
import { StateChip, type DomainStateKind } from './StateChip';

/**
 * One read, resolved: either a value to render or a domain state that says why
 * there is none.
 *
 * Every wire ladder in this dashboard — the legacy JSON routes, the
 * `DashboardEnvelopeV1` routes, the structure reads, the Work envelope — is a
 * different union at the transport edge and the same question at the render
 * edge: *may the body run, and if not, what does the reader get told?* Each
 * ladder answers that once, in an adapter beside its fetcher, and every surface
 * downstream reads this one shape.
 *
 * That is the point of the type rather than a side effect of it. The ladders
 * previously terminated in two boundary components and eight hand-rolled
 * `pending → undefined → transport` chains, and those chains drifted: the same
 * transport failure was `error` on one surface and the daemon's own state on
 * another, because each site re-derived the mapping. A single normalisation
 * makes drift a compile error instead of a screen nobody looks at twice.
 */
export type ReadState<T> =
  | { kind: 'ready'; value: T }
  | {
      kind: 'blocked';
      /** What the reader is told this read is. Always one of the taxonomy. */
      state: DomainStateKind;
      /** Whatever the source said about this state, and nothing where it said nothing. */
      detail?: string | undefined;
      /**
       * The decoded body a *source-level* refusal still carried.
       *
       * Only the legacy ladder produces one, and only for `unavailable`: the
       * canonical routes answer 404/503 with a typed payload whose `status`
       * discriminates the condition. Surfaces that can word that condition
       * better than a generic chip read it from here; the rest ignore it and
       * render the state, which is the safe default.
       */
      payload?: T | undefined;
    };

/**
 * The states a legacy read can be blocked in — the six failure outcomes plus
 * the two the ladder itself contributes.
 *
 * Narrower than `DomainStateKind` on purpose. A surface that words these in its
 * own terms — the Automations scheduler queues do — can then switch over them
 * exhaustively and fail to build when the set grows, which is the guarantee the
 * per-surface `LegacyResult` switches used to hold individually.
 */
export type LegacyBlockedState =
  | 'loading'
  | 'unknown'
  | 'offline'
  | 'unauthorized'
  | 'denied'
  | 'error'
  | 'unsupported_schema'
  | 'unavailable';

/** A legacy read resolved, with its blocked states narrowed. Assignable to
 * `ReadState<T>` wherever only the taxonomy matters. */
export type LegacyReadState<T> =
  | { kind: 'ready'; value: T }
  | {
      kind: 'blocked';
      state: LegacyBlockedState;
      detail?: string | undefined;
      payload?: T | undefined;
    };

/** The domain state a non-ok legacy read renders as.
 *
 * Exhaustive over the failure outcomes, so a new one added to `LegacyResult`
 * fails to build here rather than falling into whichever arm a chain of
 * ternaries happened to end on — which is how 401 and 403 spent their whole
 * life rendering as a generic error whose only discriminator was status text. */
function legacyFailureState(
  result: Exclude<LegacyResult<unknown>, { outcome: 'ok' }>,
): LegacyBlockedState {
  switch (result.outcome) {
    case 'offline':
      return 'offline';
    case 'unauthorized':
      return 'unauthorized';
    case 'denied':
      return 'denied';
    case 'error':
      return 'error';
    case 'unsupported_schema':
      return 'unsupported_schema';
    case 'unavailable':
      return 'unavailable';
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/**
 * The legacy ladder, resolved.
 *
 * `unavailable` is the state that most needs its detail. Its chip word is the
 * same for a registry that is missing and one that failed to open, and the
 * payload's `status`/`error` is the only thing that tells them apart.
 */
export function legacyReadState<T>(
  pending: boolean,
  result: LegacyResult<T> | undefined,
): LegacyReadState<T> {
  if (pending) return { kind: 'blocked', state: 'loading' };
  if (!result) return { kind: 'blocked', state: 'unknown' };
  if (result.outcome === 'ok') return { kind: 'ready', value: result.data };
  if (result.outcome === 'unavailable') {
    return {
      kind: 'blocked',
      state: 'unavailable',
      detail: result.reason ?? result.status,
      payload: result.data,
    };
  }
  return {
    kind: 'blocked',
    state: legacyFailureState(result),
    detail: result.outcome === 'error' ? result.detail : undefined,
  };
}

/**
 * The envelope ladder, resolved to the envelope itself.
 *
 * The value is the whole `DashboardEnvelopeV1<T>` rather than its payload because the
 * truth header — coverage, freshness, authorization, the server's own legal
 * actions — is what every envelope surface renders beside the body, and a
 * reader handed only the payload would have to fetch it back out.
 *
 * The three sentences are per-site because what a reader needs to be told about
 * a missing store telemetry read is not what they need to be told about a
 * missing cost projection. The *states* are not: they are the daemon's, and a
 * surface that re-derived them could disagree with the wire.
 */
export function envelopeReadState<T>(
  pending: boolean,
  result: EnvelopeResult<T> | undefined,
  details: {
    /** What the reader is told while the request is in flight. */
    loading: string;
    /** What the reader is told when no response has been recorded. */
    unknown?: string;
    /** What the reader is told when the transport outcome carries no reason. */
    transport?: string;
  },
): ReadState<DashboardEnvelopeV1<T>> {
  if (pending) return { kind: 'blocked', state: 'loading', detail: details.loading };
  if (result === undefined) {
    return { kind: 'blocked', state: 'unknown', detail: details.unknown ?? 'no response recorded' };
  }
  if (result.outcome === 'transport') {
    return {
      kind: 'blocked',
      state: result.state,
      detail: result.detail ?? details.transport ?? 'daemon unreachable',
    };
  }
  return { kind: 'ready', value: result.envelope };
}

/**
 * A read rendered with its chrome: the body only ever runs against a value.
 *
 * The two chromes are the two places a blocked read can land, and they differ
 * because the surrounding surface differs. `centered` is for a channel that has
 * nothing else on it — the state takes the whole plate, and the title rides
 * inside it because there is no heading above. `panel` is for a titled section
 * among others: the heading and its blurb stay drawn, and only the body becomes
 * a chip, so the reader can still see which of six sections is the one that
 * failed.
 */
export function ReadSection<T>({
  title,
  state,
  chrome,
  blurb,
  className = 'border-b border-edge-subtle',
  children,
}: {
  title: string;
  state: ReadState<T>;
  chrome: 'centered' | 'panel';
  /** The line under the heading naming what the read model carries. `panel` only. */
  blurb?: ReactNode;
  /** `panel` only. */
  className?: string;
  children: (value: T) => ReactNode;
}) {
  if (chrome === 'centered') {
    return state.kind === 'ready' ? (
      <>{children(state.value)}</>
    ) : (
      <CenteredState title={title} kind={state.state} detail={state.detail} />
    );
  }
  return (
    <section className={className} aria-label={title}>
      <h2 className="px-4 pt-4 text-sm font-semibold tracking-tight">{title}</h2>
      {blurb ? <p className="px-4 pt-0.5 text-2xs text-text-muted">{blurb}</p> : null}
      {state.kind === 'ready' ? (
        children(state.value)
      ) : (
        <ReadModelState kind={state.state} detail={state.detail} />
      )}
    </section>
  );
}

/** Renders truthful states around a legacy fetch; children render only on ok. */
export function LegacyBoundary<T>({
  title,
  pending,
  result,
  statusInBody,
  children,
}: {
  title: string;
  pending: boolean;
  result: LegacyResult<T> | undefined;
  /**
   * Hand an `unavailable` body to the child instead of rendering the generic
   * state for it.
   *
   * For payloads that carry their own `status` discriminant, where the child
   * switches on it and has a sentence per condition — "the project registry is
   * not configured" says what to do about it, and the generic chip cannot,
   * because it does not know which source this is. Opt-in, because a child
   * that does not check `status` would otherwise render a failure body as
   * though the read had succeeded, which is the failure this whole boundary
   * exists to prevent.
   */
  statusInBody?: boolean;
  children: (data: T) => ReactNode;
}) {
  const read = legacyReadState(pending, result);
  // The opt-in, expressed as a promotion of the resolved state rather than as a
  // second arm through the boundary: a refusal that carried a body becomes a
  // read the child may render, and everything downstream stays one shape.
  const state =
    statusInBody === true && read.kind === 'blocked' && read.payload !== undefined
      ? ({ kind: 'ready', value: read.payload } as const)
      : read;
  return (
    <ReadSection title={title} state={state} chrome="centered">
      {children}
    </ReadSection>
  );
}

/**
 * A titled section wrapping one envelope read, which resolves every way the
 * read can fail to produce an envelope before its body is ever called: still in
 * flight, no response recorded, or a transport outcome carrying the daemon's
 * own domain state. The body therefore only ever runs against a decoded
 * envelope.
 */
export function EnvelopeSection<T>({
  title,
  blurb,
  pending,
  result,
  loadingDetail,
  transportDetail = 'daemon unreachable',
  className = 'border-b border-edge-subtle',
  children,
}: {
  title: string;
  blurb?: ReactNode;
  pending: boolean;
  result: EnvelopeResult<T> | undefined;
  loadingDetail: string;
  transportDetail?: string;
  className?: string;
  children: (envelope: DashboardEnvelopeV1<T>) => ReactNode;
}) {
  return (
    <ReadSection
      title={title}
      chrome="panel"
      blurb={blurb}
      className={className}
      state={envelopeReadState(pending, result, {
        loading: loadingDetail,
        transport: transportDetail,
      })}
    >
      {children}
    </ReadSection>
  );
}

/** Crafted truthful states (plan 11a): one sentence of what this state means
 * here plus the next action, not a bare chip. Workspace-specific sentences
 * come from the caller; these are the designed defaults per state.
 *
 * Partial on purpose. Every state listed has one next action that holds
 * wherever it appears — start the daemon, authenticate, update the build,
 * retry. `unavailable` has none: what to do about a source that cannot serve
 * depends on which source and on the reason it reported, both of which reach
 * the chip as its detail. A generic sentence here would either restate the
 * chip or invent a remedy this surface cannot know, so the state renders as
 * chip plus reported reason and nothing is added. */
const STATE_GUIDANCE: Partial<Record<DomainStateKind, { sentence: string; action: string }>> = {
  loading: { sentence: 'Reading from the daemon.', action: 'This resolves on its own.' },
  offline: {
    sentence: 'The daemon is not reachable from this browser.',
    action: 'Start it with `tracedecay daemon run`, then refresh.',
  },
  error: {
    sentence: 'The read failed and nothing is being invented in its place.',
    action: 'Retry, or check the daemon log if it persists.',
  },
  // Split from `error` because the two refusals need opposite next actions,
  // and neither of them is "retry": no identity was accepted at all, versus an
  // identity that was accepted and is not allowed to see this.
  unauthorized: {
    sentence: 'The daemon accepted no identity for this read, so it refused to answer.',
    action: 'Authenticate to the daemon, then refresh.',
  },
  denied: {
    sentence: 'The daemon knows this identity and does not permit it to read this scope.',
    action: 'Switch to a scope you hold, or grant this one access.',
  },
  unknown: {
    sentence: 'No response has been recorded for this surface yet.',
    action: 'Refresh once the daemon is serving.',
  },
  unsupported_schema: {
    sentence: 'The daemon answered with a shape this build does not understand.',
    action: 'Update the dashboard build to match the daemon.',
  },
};

export function CenteredState({
  title,
  kind,
  detail,
}: {
  title: string;
  kind: DomainStateKind;
  detail?: string | undefined;
}) {
  const guidance = STATE_GUIDANCE[kind];
  // A dead channel on an instrument still shows its ruled field and its bezel:
  // the reader can see the surface is present and simply carrying no signal.
  return (
    <div className="td-graticule flex h-full min-h-48 items-center justify-center bg-surface-0 p-8">
      <div className="relative flex max-w-md flex-col items-center gap-3 border border-edge-subtle bg-surface-1 px-8 py-6 text-center">
        <Corners />
        <h1 className="text-2xs font-semibold uppercase tracking-[0.2em] text-text-primary">
          {title}
        </h1>
        <span aria-hidden className="h-px w-10 bg-edge-strong" />
        <StateChip kind={kind} detail={detail} />
        {guidance ? (
          <p className="max-w-xs text-xs leading-relaxed text-text-muted">
            {guidance.sentence}{' '}
            <span className="text-text-secondary">{guidance.action}</span>
          </p>
        ) : null}
      </div>
    </div>
  );
}

/** A centred state for a read that produced no payload to render. */
export function ReadModelState({
  kind,
  detail,
}: {
  kind: DomainStateKind;
  detail?: string | undefined;
}) {
  return (
    <div className="flex min-h-28 items-center justify-center p-4">
      <StateChip kind={kind} detail={detail} />
    </div>
  );
}
