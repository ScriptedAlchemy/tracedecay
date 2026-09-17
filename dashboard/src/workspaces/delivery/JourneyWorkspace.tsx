import { useMemo } from 'react';
import type {
  DeliveryInboxPullRequestV1,
  DeliveryMembershipEdgeV1,
  DeliveryOverviewV1,
} from '../../contracts/generated.ts';
import { ReadSection } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { ReadoutBar } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { useProjectOverview, type DeliveryContext } from './DeliveryPage.tsx';
import {
  ControlLink,
  GradeMark,
  IdentityRow,
  ReadOnlyProviderBadge,
  microsToIso,
  shortSha,
} from './deliveryChrome.tsx';
import { projectFor } from './inboxFilter.ts';
import {
  buildJourney,
  laneLabel,
  laneServes,
  laneStateKind,
  type EpisodeRef,
  type JourneyEpisode,
  type JourneyModel,
} from './journey.ts';
import { JourneyField } from './JourneyField.tsx';
import { ProjectionLedger, laneStateDetail, overviewReadState } from './ProjectionLedger.tsx';

/**
 * Delivery · journey mode. One pull request read left to right over the eight
 * lanes Delivery can join; the field, the inspector and the exact table all
 * read the same URL selection. Nothing here writes to a provider.
 */
export function JourneyWorkspace({
  context,
  row,
  edges,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
  edges: readonly DeliveryMembershipEdgeV1[];
}) {
  const overview = useProjectOverview(row.project_id);
  return (
    <ReadSection
      title="PR journey"
      chrome="centered"
      state={overviewReadState(overview.isPending, overview.data)}
    >
      {(value) => <JourneyBody context={context} row={row} edges={edges} overview={value} />}
    </ReadSection>
  );
}

function JourneyBody({
  context,
  row,
  edges,
  overview,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
  edges: readonly DeliveryMembershipEdgeV1[];
  overview: DeliveryOverviewV1;
}) {
  const { navigate, location } = context;
  const model = useMemo(() => buildJourney(overview, { row, edges }), [overview, row, edges]);
  const selected = model.episodes.find((episode) => episode.id === location.episode) ?? null;
  const project = projectFor(context.inbox, row.project_id);
  const title = row.pull_request.identity?.title ?? row.pull_request.label;
  const servedLanes = model.lanes.filter((lane) => laneServes(lane.state)).length;
  const control = 'td-hit inline-flex min-h-9 items-center border px-3 text-xs text-text-primary hover:bg-surface-2';

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-auto">
      <header className="flex flex-wrap items-center gap-3 border-b border-edge-subtle bg-surface-1 px-3 py-2">
        <nav aria-label="Journey breadcrumb" className="flex min-w-0 items-center gap-2 font-mono text-3xs">
          <span className="text-text-muted">{project?.label ?? row.project_id}</span>
          <span aria-hidden className="text-text-muted">·</span>
          <span className="text-text-secondary">#{row.pull_request.pull_request_id}</span>
          <span aria-hidden className="text-text-muted">·</span>
          <span className="truncate font-sans text-xs text-text-primary">{title}</span>
        </nav>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          <button type="button" className={cn(control, 'border-edge-strong')} onClick={() => navigate({ mode: 'inbox' })}>
            Back to inbox
          </button>
          <button type="button" className={cn(control, 'border-accent')} onClick={() => navigate({ mode: 'review' })}>
            {location.thread !== null ? 'Continue review' : 'Start review'}
          </button>
          <ReadOnlyProviderBadge />
        </div>
      </header>
      <ReadoutBar
        label="Journey readings"
        elevation="raised"
        items={[
          { label: 'episodes', value: model.episodes.length },
          { label: 'dated', value: model.episodes.length - model.undated },
          { label: 'undated', value: model.undated },
          { label: 'lanes served', value: servedLanes, note: `${model.lanes.length} total` },
          { label: 'time span', value: spanLabel(model) },
        ]}
      />

      <div className="grid grid-cols-1 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="flex min-w-0 flex-col border-r border-edge-subtle p-3">
          <JourneyField
            model={model}
            selectedEpisodeId={location.episode}
            onSelect={(episode) => navigate({ episode: episode.id })}
          />
          <FieldLegend />
        </div>
        <EpisodeInspector episode={selected} />
      </div>

      <EpisodeTable model={model} selectedId={location.episode} onSelect={(id) => navigate({ episode: id })} />
      <GapList model={model} />
      <ProjectionLedger overview={overview} className="m-3" />
    </div>
  );
}

function spanLabel(model: JourneyModel): string {
  if (model.span === null) return '—';
  return `${utcStamp(model.span.start, 16)} → ${utcStamp(model.span.end, 16)}`;
}

/** `YYYY-MM-DD HH:MM` at 16, `YYYY-MM-DD HH:MM:SS` at 19. */
function utcStamp(micros: number, length: 16 | 19): string {
  return microsToIso(micros).slice(0, length).replace('T', ' ');
}

function timeKindLabel(kind: JourneyEpisode['timeKind']): string {
  switch (kind) {
    case 'event':
      return 'EVENT';
    case 'observed':
      return 'OBSERVED';
    case 'undated':
      return 'undated';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

function destinationLabel(href: string): string {
  return href.startsWith('/loom') ? 'Open in Loom' : 'Open in Code · Compare';
}

function FieldLegend() {
  return (
    <p aria-label="Journey legend" className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 font-mono text-3xs text-text-muted">
      <span>● EVENT time · ◇ OBSERVED time · ▭ undated gutter · dashed lane = authority not served · edge grade</span>
      <GradeMark grade="exact" />
      <GradeMark grade="inferred" />
    </p>
  );
}

function EpisodeInspector({ episode }: { episode: JourneyEpisode | null }) {
  return (
    <section aria-label="Episode detail" className="flex min-w-0 flex-col bg-surface-1">
      {episode === null ? (
        <p className="p-6 text-center text-xs text-text-muted">Select an episode in the field or the table to read its identity, time kind and destination.</p>
      ) : (
        <>
          <header className="border-b border-edge-subtle px-3 py-3">
            <h2 className="break-words text-sm font-semibold leading-snug text-text-primary">{episode.label}</h2>
            <div className="mt-1 flex flex-wrap items-center gap-2">
              <GradeMark grade={episode.grade} source={episode.source} />
              {episode.status === null ? null : <StateChip kind={episode.status} />}
            </div>
            <p className="mt-2 text-3xs leading-relaxed text-text-secondary">{episode.detail}</p>
          </header>
          <div className="px-3 py-2">
            <IdentityRow label="time" value={`${episode.at === null ? '—' : microsToIso(episode.at)} · ${timeKindLabel(episode.timeKind)}`} />
            <IdentityRow label="lane" value={laneLabel(episode.lane)} />
            <IdentityRow label="episode id" value={episode.id} />
          </div>
          <div className="border-t border-edge-subtle px-3 py-2">
            <RefIdentity episodeRef={episode.ref} />
          </div>
          {episode.href === null ? null : (
            <div className="mt-auto flex flex-wrap gap-2 border-t border-edge-subtle p-3">
              <ControlLink href={episode.href}>{destinationLabel(episode.href)}</ControlLink>
            </div>
          )}
        </>
      )}
    </section>
  );
}

function yesNo(value: boolean): string {
  return value ? 'yes' : 'no';
}

function RefIdentity({ episodeRef: ref }: { episodeRef: EpisodeRef }) {
  switch (ref.kind) {
    case 'commit':
      return (
        <>
          <IdentityRow label="sha" value={shortSha(ref.commit.commit)} />
          <IdentityRow label="author" value={ref.commit.author_name} />
          <IdentityRow label="subject" value={ref.commit.subject} />
        </>
      );
    case 'pull_request': {
      const identity = ref.pullRequest.identity;
      if (identity === null) {
        return <IdentityRow label="identity" value="not served by the provider" />;
      }
      return (
        <>
          <IdentityRow label="title" value={identity.title} />
          <IdentityRow label="state" value={identity.state} />
          <IdentityRow label="draft" value={yesNo(identity.draft)} />
          <IdentityRow label="diff" value={`+${identity.additions} −${identity.deletions}`} />
          <IdentityRow label="files" value={String(identity.changed_files)} />
        </>
      );
    }
    case 'provider_observation':
      return (
        <>
          <IdentityRow label="operation" value={ref.operation} />
          <IdentityRow label="outcome" value={ref.outcome} />
          <IdentityRow label="fetched" value={microsToIso(ref.fetchedAtMicros)} />
        </>
      );
    case 'review': {
      const observation = ref.observation;
      const preview = observation.body_preview;
      return (
        <>
          <IdentityRow label="location" value={observation.line === null ? observation.path : `${observation.path}:${observation.line}`} />
          <IdentityRow label="review state" value={observation.review_state} />
          <IdentityRow label="lifecycle" value={observation.lifecycle} />
          <IdentityRow label="author class" value={observation.author_class} />
          <div className="py-1">
            <span className="td-legend">body preview</span>
            <p className="mt-1 break-words text-xs text-text-secondary">
              {preview === null ? '—' : `${preview.text}${preview.truncated ? ' (truncated)' : ''}`}
            </p>
          </div>
          {observation.source_url === null ? null : (
            <ControlLink href={observation.source_url} external className="mt-2">
              Open in provider
            </ControlLink>
          )}
        </>
      );
    }
    case 'check': {
      const check = ref.check;
      return (
        <>
          <IdentityRow label="workflow" value={check.workflow_path} />
          <IdentityRow label="job" value={statusWithConclusion(check.job_status, check.job_conclusion)} />
          <IdentityRow label="check" value={statusWithConclusion(check.check_status, check.check_conclusion)} />
          <IdentityRow label="failure kind" value={check.failure_kind} />
          <IdentityRow label="failed step" value={check.failed_step ?? '—'} />
          <IdentityRow label="annotations" value={String(check.annotation_count)} />
          {check.annotations.length === 0 ? null : (
            <ul className="mt-1 space-y-1">
              {check.annotations.map((annotation, index) => (
                <li key={`${annotation.path}:${annotation.start_line}:${index}`} className="break-all font-mono text-3xs text-text-secondary">
                  {`${annotation.path}:${annotation.start_line}-${annotation.end_line} · ${annotation.level} · ${annotation.title ?? '—'}`}
                </li>
              ))}
            </ul>
          )}
        </>
      );
    }
    case 'release': {
      const release = ref.release;
      return (
        <>
          <IdentityRow label="tag" value={release.tag} />
          <IdentityRow label="name" value={release.name ?? '—'} />
          <IdentityRow label="draft" value={yesNo(release.draft)} />
          <IdentityRow label="prerelease" value={yesNo(release.prerelease)} />
          <IdentityRow label="assets" value={String(release.assets.length)} />
          <ControlLink href={release.source_url} external className="mt-2">
            Open release
          </ControlLink>
        </>
      );
    }
    case 'work_objective':
      return <IdentityRow label="work item" value={ref.workItemId} />;
    case 'session':
      return (
        <>
          <IdentityRow label="session" value={ref.sessionId} />
          <IdentityRow label="commit" value={shortSha(ref.commitId)} />
        </>
      );
    case 'agent':
      return <IdentityRow label="agent" value={ref.agentId} />;
    case 'handoff':
      return <IdentityRow label="handoff" value={ref.handoffId} />;
    default: {
      const unhandled: never = ref;
      return unhandled;
    }
  }
}

function statusWithConclusion(status: string, conclusion: string | null): string {
  return conclusion === null ? status : `${status} · ${conclusion}`;
}

const TABLE_HEADINGS = ['Time (UTC)', 'Kind', 'Lane', 'Source / Grade', 'Episode', 'Detail', 'Destination'] as const;

function sortedEpisodes(model: JourneyModel): readonly JourneyEpisode[] {
  return [...model.episodes].sort((left, right) => {
    if (left.at === null && right.at === null) return 0;
    if (left.at === null) return 1;
    if (right.at === null) return -1;
    return left.at - right.at;
  });
}

/** The exact fallback for the field: every episode, its time kind spelled
 * out, its grade printed, and the same URL selection as the SVG. */
function EpisodeTable({
  model,
  selectedId,
  onSelect,
}: {
  model: JourneyModel;
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  const rows = sortedEpisodes(model);
  return (
    <section aria-label="Journey episodes" className="border-t border-edge-subtle">
      <header className="px-3 py-2">
        <h2 className="td-title">Episodes · exact table</h2>
        <p className="mt-1 text-3xs text-text-muted">dated episodes ascending, then undated · {rows.length} rows</p>
      </header>
      <div className="overflow-auto">
        <table className="w-full border-collapse text-xs">
          <thead className="bg-surface-1 text-left">
            <tr className="border-b border-edge-subtle">
              {TABLE_HEADINGS.map((heading) => (
                <th key={heading} scope="col" className="td-legend px-3 py-2 font-normal">
                  {heading}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((episode) => (
              <tr
                key={episode.id}
                className={cn('border-b border-edge-subtle align-top', selectedId === episode.id && 'bg-surface-2')}
              >
                <td className="px-3 py-2 font-mono text-3xs text-text-secondary" data-cell="numeric">
                  {episode.at === null ? 'undated' : utcStamp(episode.at, 19)}
                </td>
                <td className="px-3 py-2 font-mono text-3xs text-text-muted">{timeKindLabel(episode.timeKind)}</td>
                <td className="px-3 py-2 text-text-secondary">{laneLabel(episode.lane)}</td>
                <td className="px-3 py-2">
                  <GradeMark grade={episode.grade} source={episode.source} />
                </td>
                <td className="px-3 py-2">
                  <button
                    type="button"
                    aria-pressed={selectedId === episode.id}
                    className="break-all text-left text-text-primary hover:underline"
                    onClick={() => onSelect(episode.id)}
                  >
                    {episode.label}
                  </button>
                </td>
                <td className="px-3 py-2 text-text-secondary">{episode.detail}</td>
                <td className="px-3 py-2">
                  {episode.href === null ? (
                    <span className="text-text-muted">—</span>
                  ) : (
                    <a href={episode.href} className="text-accent hover:underline">
                      {destinationLabel(episode.href)}
                    </a>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

/** Typed gaps: the model's own sentences plus every lane whose authority did
 * not serve, each with the daemon's state and reason. Rendered even when
 * empty so the absence of a gap is itself a printed claim. */
function GapList({ model }: { model: JourneyModel }) {
  const unserved = model.lanes.filter((lane) => !laneServes(lane.state));
  return (
    <section aria-label="Journey gaps" className="border-t border-edge-subtle px-3 py-2">
      <h2 className="td-title">Gaps · what the joined authorities did not serve</h2>
      {model.gaps.length === 0 && unserved.length === 0 ? (
        <p className="mt-2 text-xs text-text-muted">No gap reported by the joined authorities.</p>
      ) : (
        <ul className="mt-2 space-y-1.5">
          {model.gaps.map((gap) => (
            <li key={gap} className="text-xs text-text-secondary">{gap}</li>
          ))}
          {unserved.map((lane) => (
            <li key={lane.id} className="flex flex-wrap items-center gap-2">
              <span className="td-legend w-24 shrink-0">{lane.label}</span>
              <StateChip kind={laneStateKind(lane.state)} detail={laneStateDetail(lane.state)} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
