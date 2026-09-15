/**
 * SHARED CODE — verified exact copies of one selected body.
 *
 * Two reads per selected symbol occurrence, one per match class, against
 * `GET /api/plugins/graph/shared-code/family` (code_read_api.rs). Each family
 * is a digest group: the daemon verified every member's token bytes against
 * the representative payload before listing it, so a member here is a copy,
 * not a candidate. The route serves groups, never pairs — a thousand identical
 * generated functions arrive as one family with a thousand members and pages
 * through them by cursor.
 *
 * The view renders the daemon's own states and nothing inferred from them:
 *
 *   complete, no families      "no verified copies" — a measured zero
 *   complete, families         the groups, with their stitch mark
 *   partial                    the groups listed plus the budget sentence
 *   excluded_*                 an exclusion, worded as one, never as zero
 *   error / stale / …          the envelope's domain state, chip and reason
 *
 * Near-clone, containment, difference, and stale-relation stitches belong to
 * the routes that carry that evidence and are not drawn from this one.
 */
import { useState } from 'react';
import { Waypoints } from 'lucide-react';
import {
  SimilarResultV1Schema,
  type SimilarFamilyV1,
  type SimilarMatchClassV1,
  type SimilarOccurrenceV1,
  type SimilarResultV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { authorizationState } from '../../ui/EnvelopeTruth.tsx';
import { CenteredState, ReadSection } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';
import { codeReadState } from './codeRead.ts';
import type { TraceFocus } from './TraceView.tsx';
import {
  SHARED_CODE_MATCH_CLASSES,
  copiesOf,
  describeOccurrence,
  readSharedCodeCoverage,
  sharedFamilyUrl,
  shortDigest,
} from './sharedCode.ts';

export function SharedCodeView({
  focus,
  onFocusMember,
  onTraceMember,
}: {
  focus: TraceFocus;
  /** Re-centre Shared Code on one listed copy. */
  onFocusMember: (symbolOccurrenceId: string) => void;
  /** Open Trace on one listed copy. */
  onTraceMember: (symbolOccurrenceId: string) => void;
}) {
  return (
    <div className="flex min-h-full flex-col" data-shared-code-source={focus.id}>
      <header className="flex flex-col gap-1 border-b border-edge-subtle px-3 py-2">
        <h2 className="text-sm font-semibold tracking-tight">
          Shared Code · {focus.name ?? focus.qualified_name ?? focus.id}
        </h2>
        <p className="text-3xs leading-relaxed text-text-muted">
          Verified copies of this body's canonical tokens under two normalizations. A member is
          listed only after its token bytes matched the family's representative payload; the
          digest is the index key, never the evidence.
        </p>
        <StitchLegend />
      </header>
      {SHARED_CODE_MATCH_CLASSES.map((definition) => (
        <FamilyClassSection
          key={definition.matchClass}
          focus={focus}
          definition={definition}
          onFocusMember={onFocusMember}
          onTraceMember={onTraceMember}
        />
      ))}
    </div>
  );
}

/** The grammar, stated once above both sections so a bracket is read as a
 * relation kind and not as decoration. */
function StitchLegend() {
  return (
    <dl className="flex flex-wrap items-center gap-x-4 gap-y-1 pt-1 text-3xs text-text-muted">
      {SHARED_CODE_MATCH_CLASSES.map((definition) => (
        <div key={definition.matchClass} className="flex items-center gap-1.5">
          <StitchMark stitch={definition.stitch} />
          <dt className="sr-only">{definition.stitchLabel}</dt>
          <dd>
            {definition.stitchLabel} = {definition.label.toLowerCase()} clone
          </dd>
        </div>
      ))}
    </dl>
  );
}

function StitchMark({ stitch }: { stitch: 'solid' | 'double' }) {
  return (
    <span
      aria-hidden
      data-stitch={stitch}
      className={cn(
        'inline-block h-3.5 w-2 shrink-0 border-y border-l border-edge-strong',
        stitch === 'double' && 'border-double border-l-[3px]',
      )}
    />
  );
}

function FamilyClassSection({
  focus,
  definition,
  onFocusMember,
  onTraceMember,
}: {
  focus: TraceFocus;
  definition: (typeof SHARED_CODE_MATCH_CLASSES)[number];
  onFocusMember: (symbolOccurrenceId: string) => void;
  onTraceMember: (symbolOccurrenceId: string) => void;
}) {
  return (
    <section
      aria-label={definition.label}
      className="border-b border-edge-subtle"
      data-shared-code-class={definition.matchClass}
    >
      <div className="flex items-baseline gap-2 px-3 pt-3">
        <StitchMark stitch={definition.stitch} />
        <h3 className="text-xs font-semibold tracking-tight">{definition.label}</h3>
      </div>
      <p className="px-3 pt-0.5 text-3xs leading-relaxed text-text-muted">
        verified: {definition.verified}
      </p>
      <FamilyPage
        focus={focus}
        matchClass={definition.matchClass}
        stitch={definition.stitch}
        cursor={null}
        onFocusMember={onFocusMember}
        onTraceMember={onTraceMember}
      />
    </section>
  );
}

/**
 * One page of one class. A family the daemon marked incomplete carries a
 * `next_cursor`; following it mounts the next page beneath this one, so the
 * list grows by the daemon's own pagination rather than by any re-read of the
 * first page.
 */
function FamilyPage({
  focus,
  matchClass,
  stitch,
  cursor,
  onFocusMember,
  onTraceMember,
}: {
  focus: TraceFocus;
  matchClass: SimilarMatchClassV1;
  stitch: 'solid' | 'double';
  cursor: string | null;
  onFocusMember: (symbolOccurrenceId: string) => void;
  onTraceMember: (symbolOccurrenceId: string) => void;
}) {
  const read = useEnvelope(
    ['graph', 'shared-code', 'family', focus.id, matchClass, cursor ?? ''],
    sharedFamilyUrl(focus.id, matchClass, cursor),
    SimilarResultV1Schema,
    {
      activity: {
        id: `shared-code-${matchClass}`,
        label: `Reading ${matchClass.replace(/_/g, ' ')} families`,
        cancelable: true,
      },
    },
  );
  // Every family that carries a cursor may be followed, each once; the daemon
  // mints one cursor per incomplete family, so one page may hand out several.
  const [followed, setFollowed] = useState<ReadonlyArray<string>>([]);
  return (
    <ReadSection
      title={cursor === null ? 'Families' : 'More members'}
      chrome="panel"
      className="border-0"
      state={codeReadState(read.isPending, read.data, {
        loading: 'reading verified families',
        transport: 'the shared-code family read could not be completed',
      })}
    >
      {(envelope) => {
        const result = envelope.payload;
        const coverage = readSharedCodeCoverage(result.coverage);
        const authorization = authorizationState(envelope.authorization);
        return (
          <div className="flex flex-col gap-2 px-3 pb-3" data-shared-code-coverage={result.coverage.status}>
            <div className="flex flex-wrap items-center gap-2">
              <StateChip kind={envelope.domain_state} />
              {authorization ? <StateChip kind={authorization} detail="read authorization" /> : null}
              {coverage.kind === 'partial' ? (
                <span className="text-3xs text-text-muted">Coverage: partial</span>
              ) : null}
            </div>
            {cursor === null ? <SourceIdentity result={result} /> : null}
            {coverage.kind === 'excluded' ? (
              <CenteredState title={coverage.title} kind="complete_zero_findings" detail={coverage.sentence} />
            ) : result.families.length === 0 ? (
              coverage.kind === 'complete' && cursor === null ? (
                <CenteredState
                  title="No verified copies of this body are indexed"
                  kind="complete_zero_findings"
                />
              ) : (
                // A partial or continuation page with no families is not a
                // measured zero; it is a page the budget or cursor left empty.
                <p className="text-3xs leading-relaxed text-text-muted">
                  No further families on this page.
                </p>
              )
            ) : (
              <ol className="flex flex-col gap-2">
                {result.families.map((family) => (
                  <FamilyGroup
                    key={family.family_digest}
                    family={family}
                    stitch={stitch}
                    sourceId={result.source.symbol_occurrence_id}
                    onFocusMember={onFocusMember}
                    onTraceMember={onTraceMember}
                    onFollow={
                      family.next_cursor !== null && !followed.includes(family.next_cursor)
                        ? () => setFollowed([...followed, family.next_cursor as string])
                        : null
                    }
                  />
                ))}
              </ol>
            )}
            {coverage.kind === 'partial' ? (
              <p className="text-3xs leading-relaxed text-text-muted">{coverage.sentence}</p>
            ) : null}
            {followed.map((next) => (
              <FamilyPage
                key={next}
                focus={focus}
                matchClass={matchClass}
                stitch={stitch}
                cursor={next}
                onFocusMember={onFocusMember}
                onTraceMember={onTraceMember}
              />
            ))}
          </div>
        );
      }}
    </ReadSection>
  );
}

/** The selected body as the daemon resolved it: which occurrence, in which
 * generation, was the family looked up for. Printed once per class read so a
 * generation rollover between the two reads stays visible. */
function SourceIdentity({ result }: { result: SimilarResultV1 }) {
  return (
    <dl className="flex flex-col gap-0.5 text-3xs leading-snug">
      <div className="flex min-w-0 items-baseline gap-1.5">
        <dt className="shrink-0 uppercase tracking-[0.08em] text-text-muted">source</dt>
        <dd className="td-value min-w-0 flex-1 truncate text-text-secondary" title={describeOccurrence(result.source)}>
          {describeOccurrence(result.source)}
        </dd>
      </div>
      <div className="flex min-w-0 items-baseline gap-1.5">
        <dt className="shrink-0 uppercase tracking-[0.08em] text-text-muted">generation</dt>
        <dd className="td-value min-w-0 flex-1 truncate text-text-secondary" title={result.source_generation}>
          {result.source_generation}
        </dd>
      </div>
    </dl>
  );
}

function FamilyGroup({
  family,
  stitch,
  sourceId,
  onFocusMember,
  onTraceMember,
  onFollow,
}: {
  family: SimilarFamilyV1;
  stitch: 'solid' | 'double';
  sourceId: string;
  onFocusMember: (symbolOccurrenceId: string) => void;
  onTraceMember: (symbolOccurrenceId: string) => void;
  onFollow: (() => void) | null;
}) {
  const copies = copiesOf(family.members, sourceId);
  // `member_count` is the daemon's count of the authorized members *on this
  // page* (code_reads.rs sets it from the filtered page), not a family total.
  // The header therefore never claims a total: a complete family is counted,
  // an incomplete one is counted "on this page" with more to follow.
  return (
    <li
      className="td-raised flex flex-col border border-edge-subtle"
      data-family-digest={family.family_digest}
      data-family-complete={family.complete}
    >
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 border-b border-edge-subtle px-2.5 py-1.5">
        <span className="td-legend">family</span>
        <span className="td-value text-2xs text-text-primary" title={family.family_digest}>
          {shortDigest(family.family_digest)}
        </span>
        <span aria-hidden className="td-rule" />
        <span className="td-value shrink-0 text-2xs text-text-secondary" data-cell="numeric">
          {family.member_count.toLocaleString()}
          <span className="td-unit ml-1">
            {family.member_count === 1 ? 'member' : 'members'}
            {family.complete ? '' : ' on this page'}
          </span>
        </span>
        {family.complete ? null : (
          <span className="td-legend shrink-0 normal-case tracking-normal text-text-muted">
            family incomplete · more members follow
          </span>
        )}
        <span className="td-legend shrink-0 normal-case tracking-normal text-text-muted">
          normalization rev {family.normalization_revision}
        </span>
      </div>
      {copies.length === 0 ? (
        <p className="px-2.5 py-2 text-3xs text-text-muted">
          Only the selected body itself is on this page of the family.
        </p>
      ) : (
        <ol className="flex flex-col">
          {copies.map((member) => (
            <MemberRow
              key={member.symbol_occurrence_id}
              member={member}
              stitch={stitch}
              onFocus={() => onFocusMember(member.symbol_occurrence_id)}
              onTrace={() => onTraceMember(member.symbol_occurrence_id)}
            />
          ))}
        </ol>
      )}
      {onFollow ? (
        <button
          type="button"
          onClick={onFollow}
          className="border-t border-edge-subtle px-2.5 py-1.5 text-left text-2xs text-text-secondary hover:bg-surface-2 hover:text-text-primary"
        >
          Load more members of this family
        </button>
      ) : family.next_cursor !== null ? (
        <p className="border-t border-edge-subtle px-2.5 py-1.5 text-3xs text-text-muted">
          more members follow below
        </p>
      ) : null}
    </li>
  );
}

/** One verified copy. The stitch mark leads the row so the relation kind is
 * read before the path; the row's controls are the accessible equivalent of
 * the bracket. */
function MemberRow({
  member,
  stitch,
  onFocus,
  onTrace,
}: {
  member: SimilarOccurrenceV1;
  stitch: 'solid' | 'double';
  onFocus: () => void;
  onTrace: () => void;
}) {
  return (
    <li
      className="flex min-w-0 items-center gap-2 border-b border-edge-subtle px-2.5 py-1.5 last:border-b-0"
      data-member={member.symbol_occurrence_id}
    >
      <StitchMark stitch={stitch} />
      <button
        type="button"
        onClick={onFocus}
        className="flex min-w-0 flex-1 flex-col text-left hover:text-text-primary"
        title={describeOccurrence(member)}
      >
        <span className="td-value min-w-0 truncate text-2xs text-text-primary">
          {elideStart(member.path, 48)}
        </span>
        <span className="td-value min-w-0 truncate text-3xs text-text-muted">
          bytes {member.body_span.start_byte.toLocaleString()}–
          {member.body_span.end_byte.toLocaleString()} · {member.source_generation}
          {member.worktree_id ? ` · ${member.worktree_id}` : ''}
        </span>
      </button>
      <button
        type="button"
        onClick={onTrace}
        aria-label={`Trace ${member.path}`}
        className="flex min-h-[var(--touch-target-min)] items-center gap-1 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-2 text-3xs text-text-secondary hover:bg-surface-2 hover:text-text-primary"
      >
        <Waypoints aria-hidden size={11} />
        Trace
      </button>
    </li>
  );
}
