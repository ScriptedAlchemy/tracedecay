import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityReadResultV1,
  FeedbackProximityRelationV1,
  ProximityCoverageV1,
} from '../../contracts/index.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import type { WorkResult } from '../../workspaces/work/workApi.ts';
import {
  encounterAt,
  proximityColor,
  proximityLabel,
  proximityPage,
} from './proximity.ts';

export function ProximityPanel({
  result,
  selectedId,
  onSelect,
  legend = 'Observed encounters',
}: {
  result: WorkResult<FeedbackProximityReadResultV1> | undefined;
  selectedId: string | null;
  onSelect: (encounterId: string | null) => void;
  legend?: string;
}) {
  if (result === undefined) {
    return (
      <Panel legend={legend}>
        <StateChip kind="loading" detail="reading concurrent-work proximity" />
      </Panel>
    );
  }
  if (result.outcome === 'refused') {
    return (
      <Panel legend={legend}>
        <StateChip kind={result.state} detail={result.detail} />
      </Panel>
    );
  }
  const value = result.value;
  const page = proximityPage(value);
  if (page === null) {
    return (
      <Panel legend={legend}>
        <StateChip
          kind={value.state === 'denied' ? 'denied' : 'unavailable'}
          detail={
            value.state === 'denied'
              ? 'participant disclosure was denied'
              : 'the proximity authority is unavailable'
          }
        />
      </Panel>
    );
  }
  const selected = encounterAt(page.encounters, selectedId);
  if (selectedId !== null && selected === null) {
    return (
      <Panel legend={legend}>
        <StateChip kind="unavailable" detail="the selected encounter is outside this read" />
        <button type="button" className="mt-2 min-h-11 text-xs" onClick={() => onSelect(null)}>
          Return to encounters
        </button>
      </Panel>
    );
  }
  if (selected !== null) {
    return <EncounterDetail encounter={selected} onReturn={() => onSelect(null)} />;
  }
  return (
    <Panel legend={legend}>
      <ReadState result={value} />
      {page.encounters.length === 0 ? (
        <p className="text-xs text-text-muted">
          The authority completed this scope and found no evidenced encounters.
        </p>
      ) : (
        <div className="grid gap-2">
          {page.encounters.map((encounter) => (
            <button
              key={encounter.encounter_id}
              type="button"
              onClick={() => onSelect(encounter.encounter_id)}
              className="min-h-11 border border-edge-subtle p-2 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
              data-proximity-encounter={encounter.encounter_id}
            >
              <span className="flex items-center gap-2 text-xs text-text-primary">
                <span
                  aria-hidden
                  className="size-2 shrink-0 rounded-full"
                  style={{ backgroundColor: proximityColor(encounter.relation) }}
                />
                {proximityLabel(encounter.relation)}
              </span>
              <span className="mt-1 block text-3xs text-text-muted">
                {encounter.participants.map((participant) => participant.agent_id).join(' + ')}
                {' · '}
                {formatMicros(encounter.interval.start)} to {formatMicros(encounter.interval.end)}
              </span>
            </button>
          ))}
        </div>
      )}
    </Panel>
  );
}

function ReadState({ result }: { result: FeedbackProximityReadResultV1 }) {
  switch (result.state) {
    case 'complete':
      return <StateChip kind="ready" detail={`${result.page.encounters.length} encounters`} />;
    case 'complete_zero':
      return <StateChip kind="complete_zero_findings" detail="zero evidenced encounters" />;
    case 'partial':
      return (
        <StateChip
          kind="partial"
          detail={`${result.page.encounters.length} encounters · ${result.omissions.join(', ')}`}
        />
      );
    case 'stale':
      return (
        <StateChip
          kind="stale"
          detail={`expired evidence · ${result.omissions.join(', ')}`}
        />
      );
    case 'denied':
      return <StateChip kind="denied" detail="participant disclosure denied" />;
    case 'unavailable':
      return <StateChip kind="unavailable" detail="proximity authority unavailable" />;
    default: {
      const unhandled: never = result;
      return unhandled;
    }
  }
}

function EncounterDetail({
  encounter,
  onReturn,
}: {
  encounter: FeedbackProximityEncounterV1;
  onReturn: () => void;
}) {
  return (
    <Panel legend="Encounter evidence">
      <button type="button" onClick={onReturn} className="mb-2 min-h-11 text-xs text-text-secondary">
        Return to encounters
      </button>
      <StateChip
        kind={encounterCoverageKind(encounter.coverage)}
        detail={`${proximityLabel(encounter.relation)} · observed ${formatMicros(encounter.observed_at)} · expires ${formatMicros(encounter.expires_at)}`}
      />
      <div className="mt-2 grid gap-2 md:grid-cols-2">
        {encounter.participants.map((participant) => (
          <article
            key={`${participant.source.provider}:${participant.source.session_id}`}
            className="border border-edge-subtle p-2 text-3xs text-text-muted"
          >
            <p className="text-xs text-text-primary">
              {participant.agent_id} · {participant.access}
            </p>
            <p>
              {participant.source.provider} · {participant.source.session_id}
            </p>
            <p>{participant.worktree_id ?? participant.worktree_root}</p>
            <p>{participant.head_revision ?? 'head revision unavailable'}</p>
            <p>
              {participant.address.file}
              {participant.address.span === null
                ? ''
                : ` · bytes ${participant.address.span.start_byte}-${participant.address.span.end_byte}`}
            </p>
            <nav className="mt-2 flex flex-wrap gap-3" aria-label="Encounter pivots">
              <a href={codeHref(encounter.relation)} className="min-h-11 text-accent">
                Open Code
              </a>
              <a href="/sessions" className="min-h-11 text-accent">
                Open Sessions
              </a>
              <a href="/work" className="min-h-11 text-accent">
                Open Work
              </a>
              <a href="/delivery" className="min-h-11 text-accent">
                Open Delivery
              </a>
            </nav>
          </article>
        ))}
      </div>
      <RelationEvidence relation={encounter.relation} />
    </Panel>
  );
}

function encounterCoverageKind(coverage: ProximityCoverageV1): DomainStateKind {
  switch (coverage) {
    case 'complete':
      return 'ready';
    case 'partial':
      return 'partial';
    case 'stale':
      return 'stale';
    case 'unavailable':
      return 'unavailable';
    case 'denied':
    case 'private':
      return 'denied';
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

function RelationEvidence({ relation }: { relation: FeedbackProximityRelationV1 }) {
  switch (relation.relation_kind) {
    case 'code_neighborhood_candidate':
      return (
        <p className="mt-2 text-3xs text-text-muted">
          The graph records this code neighborhood. No clone or conflict is claimed.
        </p>
      );
    case 'shared_code_candidate':
      return (
        <p className="mt-2 text-3xs text-text-muted">
          Clone lookup · generation {relation.clone_handle.source_generation} · symbol{' '}
          {relation.clone_handle.source_symbol}
        </p>
      );
    case 'overlapping_edit':
      return (
        <p className="mt-2 text-3xs text-text-muted">
          The activity and code ranges overlap. No content conflict is claimed.
        </p>
      );
    case 'confirmed_conflict':
      return (
        <div className="mt-2 text-3xs text-text-muted">
          <p>Common base · {relation.conflict_handle.common_base_revision}</p>
          <p>
            Exact differences · {relation.conflict_handle.differences.length} ·{' '}
            {relation.conflict_handle.evidence_digest}
          </p>
        </div>
      );
    default: {
      const unhandled: never = relation;
      return unhandled;
    }
  }
}

function codeHref(relation: FeedbackProximityRelationV1): string {
  switch (relation.relation_kind) {
    case 'shared_code_candidate':
      return `/code?view=shared-code&symbol=${encodeURIComponent(relation.clone_handle.source_symbol)}`;
    case 'code_neighborhood_candidate':
    case 'overlapping_edit':
    case 'confirmed_conflict':
      return '/code';
    default: {
      const unhandled: never = relation;
      return unhandled;
    }
  }
}

function formatMicros(value: number): string {
  return new Date(value / 1_000).toISOString();
}
