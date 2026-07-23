import type { ReactNode } from 'react';
import { StateChip, type DomainStateKind } from './StateChip';
import type { LegacyResult } from '../data/query/legacy.ts';

/** Renders truthful states around a legacy fetch; children render only on ok. */
export function LegacyBoundary<T>({
  title,
  pending,
  result,
  children,
}: {
  title: string;
  pending: boolean;
  result: LegacyResult<T> | undefined;
  children: (data: T) => ReactNode;
}) {
  if (pending) return <CenteredState title={title} kind="loading" />;
  if (!result) return <CenteredState title={title} kind="unknown" />;
  if (result.outcome === 'ok') return <>{children(result.data)}</>;
  const kind: DomainStateKind =
    result.outcome === 'offline'
      ? 'offline'
      : result.outcome === 'error'
        ? 'error'
        : 'unsupported_schema';
  const detail = result.outcome === 'error' ? result.detail : undefined;
  return <CenteredState title={title} kind={kind} detail={detail} />;
}

export function CenteredState({
  title,
  kind,
  detail,
}: {
  title: string;
  kind: DomainStateKind;
  detail?: string | undefined;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 p-8">
      <h1 className="text-lg font-semibold tracking-tight">{title}</h1>
      <StateChip kind={kind} detail={detail} />
    </div>
  );
}

/** Compact stat tile for overview grids. */
export function StatTile({ label, value, hint }: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="flex flex-col gap-0.5 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-3 py-2.5">
      <span className="text-2xs uppercase tracking-wide text-text-muted">{label}</span>
      <span className="tabular text-xl font-semibold leading-tight text-text-primary" data-cell="numeric">
        {value}
      </span>
      {hint ? <span className="text-2xs text-text-muted">{hint}</span> : null}
    </div>
  );
}
