/**
 * Remote Brain operational plane, as the daemon reports it.
 *
 * `/api/remote/status` is the Settings reading of the same canonical
 * operational model Doctor, CLI, and MCP share. This panel is a reading, not
 * a control: enrollment, failover, and restore stay on their owning
 * application commands.
 */
import {
  DashboardEnvelopeV1Schema,
  RemoteOperationalStatusPayloadV1Schema,
  type DashboardEnvelopeV1,
  type RemoteAuthoritySummaryV1,
  type RemoteListenerKindV1,
  type RemoteOperationalStatusPayloadV1,
  type RemoteReadinessKindV1,
} from '../../contracts/generated.ts';
import { usePayload } from '../../data/query/usePayload.ts';
import type { PayloadResult } from '../../data/query/payload.ts';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Legend, Readout } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';

const STATUS_SCHEMA = DashboardEnvelopeV1Schema(RemoteOperationalStatusPayloadV1Schema);

export function RemoteBrainPanel() {
  const status = usePayload(['remote-status'], '/api/remote/status', STATUS_SCHEMA);

  if (status.isPending) {
    return (
      <PanelFrame>
        <p className="td-value text-3xs text-text-muted">reading remote brain…</p>
      </PanelFrame>
    );
  }
  if (!status.data) {
    return (
      <PanelFrame>
        <p className="td-value text-3xs text-text-muted">
          The remote operational read produced no response, so enrollment and
          authority are unknown.
        </p>
      </PanelFrame>
    );
  }
  if (status.data.outcome !== 'ok') {
    return (
      <PanelFrame>
        <TransportRefusal result={status.data} />
      </PanelFrame>
    );
  }

  return (
    <PanelFrame>
      <RemoteBrainBody envelope={status.data.data} />
    </PanelFrame>
  );
}

function TransportRefusal({
  result,
}: {
  result: Exclude<PayloadResult<DashboardEnvelopeV1<RemoteOperationalStatusPayloadV1>>, { outcome: 'ok' }>;
}) {
  switch (result.outcome) {
    case 'unavailable':
      return (
        <StateChip
          kind="unavailable"
          detail={result.reason ?? `remote status reported ${result.status}`}
        />
      );
    case 'offline':
      return <StateChip kind="offline" detail="the daemon could not be reached" />;
    case 'unauthorized':
      return <StateChip kind="unauthorized" detail="the remote status read was not authorized" />;
    case 'denied':
      return <StateChip kind="denied" detail="the remote status read was denied" />;
    case 'error':
      return <StateChip kind="error" detail={result.detail} />;
    case 'unsupported_schema':
      return (
        <StateChip
          kind="unsupported_schema"
          detail="the remote status payload is not a shape this build reads"
        />
      );
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

function RemoteBrainBody({
  envelope,
}: {
  envelope: DashboardEnvelopeV1<RemoteOperationalStatusPayloadV1>;
}) {
  const payload = envelope.payload;
  switch (payload.kind) {
    case 'unconfigured':
      return (
        <div data-remote-brain="unconfigured">
          <CenteredState
            title="Remote Brain is not enrolled"
            kind="unknown"
            detail="No enrollment is configured on this daemon, so authority, spool, and failover are not claimed."
          />
        </div>
      );
    case 'unavailable':
      return (
        <div data-remote-brain="unavailable">
          <CenteredState
            title="Remote Brain is unavailable"
            kind={envelope.domain_state === 'unsupported' ? 'unsupported' : 'unavailable'}
            detail={payload.note}
          />
        </div>
      );
    case 'observed':
      return <ObservedBody payload={payload} />;
    default: {
      const exhaustive: never = payload;
      return exhaustive;
    }
  }
}

function ObservedBody({
  payload,
}: {
  payload: Extract<RemoteOperationalStatusPayloadV1, { kind: 'observed' }>;
}) {
  return (
    <div className="flex flex-col gap-3" data-remote-brain="observed">
      <div className="flex flex-wrap gap-2" data-remote-readiness={payload.readiness}>
        <StateChip
          kind={readinessKind(payload.readiness)}
          detail={`readiness ${payload.readiness.replaceAll('_', ' ')}`}
        />
        <StateChip
          kind={listenerKind(payload.listener)}
          detail={`listener ${payload.listener}`}
        />
        <AuthorityChip authority={payload.authority} />
      </div>

      <div className="flex flex-wrap gap-4">
        <Readout
          label="enrollment"
          size="sm"
          value={payload.enrollment_configured ? 'configured' : 'not configured'}
        />
        <Readout label="spool pending" size="sm" value={String(payload.spool.pending_count)} />
        <Readout
          label="spool quarantined"
          size="sm"
          value={String(payload.spool.quarantined_count)}
        />
      </div>

      {payload.spool.has_sequence_gap ? (
        <StateChip
          kind="error"
          detail="sequence gap in the remote offline-capture spool"
        />
      ) : null}

      <dl className="flex flex-col gap-1">
        <Field
          term="replay coverage"
          detail={payload.replay_coverage_complete ? 'complete' : 'incomplete'}
        />
        <Field
          term="backup verified"
          detail={payload.current_backup_verified ? 'verified' : 'not verified'}
        />
        <Field
          term="failover"
          detail={payload.failover_in_progress ? 'in progress' : 'idle'}
        />
        <Field
          term="recovery"
          detail={payload.recovery_required ? 'required' : 'not required'}
        />
        <Field term="observed at" detail={formatMicrosUtc(payload.observed_at)} />
        <Field term="coverage" detail={payload.coverage} />
      </dl>

      <AuthorityDetail authority={payload.authority} />
    </div>
  );
}

function AuthorityChip({ authority }: { authority: RemoteAuthoritySummaryV1 }) {
  switch (authority.state) {
    case 'available':
      return <StateChip kind="ready" detail="authority available" />;
    case 'partial':
      return (
        <StateChip
          kind="partial"
          detail={`authority partial · ${authority.missing.map(reasonLabel).join(', ')}`}
        />
      );
    case 'unavailable':
      return (
        <StateChip
          kind="unavailable"
          detail={`authority unavailable · ${reasonLabel(authority.reason)}`}
        />
      );
    default: {
      const exhaustive: never = authority;
      return exhaustive;
    }
  }
}

function AuthorityDetail({ authority }: { authority: RemoteAuthoritySummaryV1 }) {
  switch (authority.state) {
    case 'available':
      return (
        <dl className="flex flex-col gap-1" data-remote-authority="available">
          <Field term="brain" detail={authority.fence.brain_id} />
          <Field term="shard" detail={authority.fence.shard_id} />
          <Field term="generation" detail={authority.fence.generation_id} />
          <Field term="epoch" detail={String(authority.fence.authority_epoch)} />
          <Field term="authority node" detail={authority.fence.authority_node_id} />
        </dl>
      );
    case 'partial':
      return (
        <div data-remote-authority="partial">
          {authority.fence ? (
            <dl className="flex flex-col gap-1">
              <Field term="brain" detail={authority.fence.brain_id} />
              <Field term="shard" detail={authority.fence.shard_id} />
              <Field term="epoch" detail={String(authority.fence.authority_epoch)} />
            </dl>
          ) : (
            <p className="td-value text-3xs text-text-muted">
              No verified fence is known for this partial authority.
            </p>
          )}
          <p className="td-value text-3xs text-text-muted">
            Missing evidence: {authority.missing.map(reasonLabel).join(', ')}
          </p>
        </div>
      );
    case 'unavailable':
      return (
        <p className="td-value text-3xs text-text-muted" data-remote-authority="unavailable">
          Authority unavailable: {reasonLabel(authority.reason)}
        </p>
      );
    default: {
      const exhaustive: never = authority;
      return exhaustive;
    }
  }
}

function Field({ term, detail }: { term: string; detail: string }) {
  return (
    <div className="flex gap-2">
      <dt className="td-legend shrink-0">{term}</dt>
      <dd className="td-value min-w-0 break-all font-mono text-3xs text-text-secondary">
        {detail}
      </dd>
    </div>
  );
}

function readinessKind(readiness: RemoteReadinessKindV1): DomainStateKind {
  switch (readiness) {
    case 'ready':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'recovery_required':
      return 'error';
    case 'unconfigured':
      return 'unknown';
    default: {
      const exhaustive: never = readiness;
      return exhaustive;
    }
  }
}

function listenerKind(listener: RemoteListenerKindV1): DomainStateKind {
  switch (listener) {
    case 'serving':
      return 'ready';
    case 'degraded':
      return 'partial';
    case 'disabled':
      return 'unavailable';
    default: {
      const exhaustive: never = listener;
      return exhaustive;
    }
  }
}

function reasonLabel(reason: string): string {
  return reason.replaceAll('_', ' ');
}

function PanelFrame({ children }: { children: React.ReactNode }) {
  return (
    <section
      aria-label="Remote Brain"
      className="flex shrink-0 flex-col gap-2 border-b border-edge-subtle p-3"
    >
      <Legend>Remote Brain</Legend>
      {children}
    </section>
  );
}
