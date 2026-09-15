/**
 * COMPARE — two exact local revisions in one identity-stable union layout.
 *
 * `GET /api/plugins/graph/compare/union-layout` (code_read_api.rs) resolves two
 * local branch references against the revisions the reader expects them to
 * hold, reads both retained generations under the bounded-read caps, and
 * returns one union sorted by identity. The view draws that union in the
 * daemon's order and never re-sorts it:
 *
 *   unchanged / changed   in place, on both sides
 *   added                 head side only, inserted at its identity position so
 *                         no existing landmark moves
 *   removed               base side only, keeping an outlined former space
 *
 * A reference that moved past its expected revision answers `stale` with
 * `selected_revision_changed`, and this view says exactly that instead of
 * comparing whatever the branch points at now. Filters are the route's own
 * `file` and `kind` parameters; the counts printed are of the filtered union
 * the daemon returned, not of the repository.
 */
import { useState, type FormEvent } from 'react';
import {
  CodeIndexFreshnessPayloadV1Schema,
  RevisionPairUnionLayoutV1Schema,
  type RevisionPairChangeV1,
  type RevisionPairFileRegionV1,
  type RevisionPairRevisionV1,
  type RevisionPairSymbolRegionV1,
  type RevisionPairUnionLayoutV1,
} from '../../contracts/generated.ts';
import { envelopePayload, useEnvelope } from '../../data/query/useEnvelope.ts';
import { authorizationState } from '../../ui/EnvelopeTruth.tsx';
import { CenteredState, ReadSection } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';
import { codeReadState } from './codeRead.ts';
import {
  COMPARE_CHANGE_RULES,
  branchFromReference,
  compareUnionLayoutUrl,
  countChanges,
  groupSymbolsByFile,
  isCompareSelectionComplete,
  type CompareSelection,
} from './compareLayout.ts';

export function CompareView({
  selection,
  onSelectionChange,
}: {
  selection: CompareSelection;
  onSelectionChange: (selection: CompareSelection) => void;
}) {
  const complete = isCompareSelectionComplete(selection);
  return (
    <div className="flex min-h-full flex-col" data-compare-selected={complete}>
      <header className="flex flex-col gap-1 border-b border-edge-subtle px-3 py-2">
        <h2 className="text-sm font-semibold tracking-tight">Compare</h2>
        <p className="text-3xs leading-relaxed text-text-muted">
          Two exact revisions of local branches in one union layout. Identity order is the
          daemon's: added regions appear without moving old landmarks, removed regions keep
          their outlined former space. A branch that has moved past its expected revision is
          reported as stale, never compared as whatever it points at now.
        </p>
      </header>
      <SelectionForm selection={selection} onSubmit={onSelectionChange} />
      {complete ? (
        <UnionLayoutReading selection={selection} />
      ) : (
        <CenteredState
          title="Compare needs two exact revisions"
          kind="unknown"
          detail="Name a base and a head branch with the commit each is expected to hold, then compare."
        />
      )}
    </div>
  );
}

/** The four identity fields and two lens filters. Submission writes the URL;
 * nothing is read until every identity field is present. */
function SelectionForm({
  selection,
  onSubmit,
}: {
  selection: CompareSelection;
  onSubmit: (selection: CompareSelection) => void;
}) {
  const [draft, setDraft] = useState<CompareSelection>(selection);
  const freshness = useEnvelope(
    ['code-index', 'freshness'],
    '/api/code-index/freshness',
    CodeIndexFreshnessPayloadV1Schema,
  );
  // The indexed worktree is the one revision the daemon already knows exactly.
  // It is offered as a fill for head only when the scheduler reported both the
  // reference and the revision; a missing revision is not defaulted.
  const indexed = envelopePayload(freshness.data)?.worktrees.find(
    (worktree) => worktree.source_reference !== null && worktree.source_revision !== null,
  );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    onSubmit({
      base: { branch: draft.base.branch.trim(), revision: draft.base.revision.trim() },
      head: { branch: draft.head.branch.trim(), revision: draft.head.revision.trim() },
      file: draft.file.trim(),
      kind: draft.kind.trim(),
    });
  };
  return (
    <form
      onSubmit={submit}
      aria-label="Revision selection"
      className="grid grid-cols-1 gap-x-3 gap-y-2 border-b border-edge-subtle px-3 py-2 md:grid-cols-2"
    >
      <RevisionFields
        legend="Base"
        value={draft.base}
        onChange={(base) => setDraft({ ...draft, base })}
      />
      <RevisionFields
        legend="Head"
        value={draft.head}
        onChange={(head) => setDraft({ ...draft, head })}
        fill={
          indexed && indexed.source_reference !== null && indexed.source_revision !== null
            ? {
                label: `Use indexed ${branchFromReference(indexed.source_reference)}`,
                apply: () =>
                  setDraft({
                    ...draft,
                    head: {
                      branch: branchFromReference(indexed.source_reference ?? ''),
                      revision: indexed.source_revision ?? '',
                    },
                  }),
              }
            : null
        }
      />
      <div className="flex flex-wrap items-end gap-2 md:col-span-2">
        <Field
          label="File filter"
          value={draft.file}
          onChange={(file) => setDraft({ ...draft, file })}
          placeholder="path substring"
        />
        <Field
          label="Kind filter"
          value={draft.kind}
          onChange={(kind) => setDraft({ ...draft, kind })}
          placeholder="symbol kind"
        />
        <button
          type="submit"
          className="min-h-[var(--touch-target-min)] rounded-[var(--radius-standard)] border border-edge-strong bg-surface-2 px-3 text-2xs text-text-primary hover:bg-surface-3"
        >
          Compare
        </button>
      </div>
    </form>
  );
}

function RevisionFields({
  legend,
  value,
  onChange,
  fill,
}: {
  legend: string;
  value: CompareSelection['base'];
  onChange: (value: CompareSelection['base']) => void;
  fill?: { label: string; apply: () => void } | null;
}) {
  return (
    <fieldset className="flex min-w-0 flex-col gap-1.5 border border-edge-subtle p-2">
      <legend className="td-legend px-1">{legend}</legend>
      <Field
        label={`${legend} branch`}
        value={value.branch}
        onChange={(branch) => onChange({ ...value, branch })}
        placeholder="local branch name"
      />
      <Field
        label={`${legend} revision`}
        value={value.revision}
        onChange={(revision) => onChange({ ...value, revision })}
        placeholder="expected commit object id"
        mono
      />
      {fill ? (
        <button
          type="button"
          onClick={fill.apply}
          className="self-start text-3xs text-text-secondary underline-offset-2 hover:text-text-primary hover:underline"
        >
          {fill.label}
        </button>
      ) : null}
    </fieldset>
  );
}

function Field({
  label,
  value,
  onChange,
  placeholder,
  mono,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  mono?: boolean;
}) {
  return (
    <label className="flex min-w-0 flex-1 flex-col gap-0.5 text-3xs text-text-muted">
      <span className="uppercase tracking-[0.08em]">{label}</span>
      <input
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        className={cn(
          'min-h-[var(--touch-target-min)] min-w-0 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-0 px-2 text-2xs text-text-primary placeholder:text-text-muted focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
          mono && 'td-value',
        )}
      />
    </label>
  );
}

function UnionLayoutReading({ selection }: { selection: CompareSelection }) {
  const url = compareUnionLayoutUrl(selection);
  const read = useEnvelope(['graph', 'compare', 'union-layout', url], url, RevisionPairUnionLayoutV1Schema, {
    activity: { id: 'compare-union-layout', label: 'Reading the revision union', cancelable: true },
  });
  return (
    <ReadSection
      title="Union layout"
      chrome="panel"
      className="border-0"
      state={codeReadState(read.isPending, read.data, {
        loading: 'resolving both revisions and reading their generations',
        transport: 'the revision comparison could not be completed',
      })}
    >
      {(envelope) => {
        const layout = envelope.payload;
        const authorization = authorizationState(envelope.authorization);
        return (
          <div className="flex flex-col gap-3 px-3 pb-3" data-compare-files={layout.files.length}>
            <div className="flex flex-wrap items-center gap-2">
              <StateChip kind={envelope.domain_state} />
              {authorization ? <StateChip kind={authorization} detail="read authorization" /> : null}
            </div>
            <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
              <RevisionIdentity side="base" revision={layout.base} />
              <RevisionIdentity side="head" revision={layout.head} />
            </div>
            {layout.files.length === 0 && layout.symbols.length === 0 ? (
              <CenteredState
                title="No regions in this union"
                kind="complete_zero_findings"
                detail={
                  selection.file !== '' || selection.kind !== ''
                    ? 'Both generations were read; nothing matched the active filters.'
                    : 'Both generations were read and contain no indexed files or symbols.'
                }
              />
            ) : (
              <>
                <ChangeLegend layout={layout} />
                <FileRegions layout={layout} />
              </>
            )}
          </div>
        );
      }}
    </ReadSection>
  );
}

/** One resolved side: the reference, the commit the daemon confirmed it holds,
 * its tree, and the generation the reading came from. */
function RevisionIdentity({ side, revision }: { side: 'base' | 'head'; revision: RevisionPairRevisionV1 }) {
  return (
    <dl
      className="td-raised flex flex-col gap-0.5 border border-edge-subtle px-2.5 py-2 text-3xs leading-snug"
      data-compare-side={side}
    >
      <Row label={side}>{revision.reference}</Row>
      <Row label="revision" mono>
        {revision.revision}
      </Row>
      <Row label="tree" mono>
        {revision.tree}
      </Row>
      <Row label="generation" mono>
        {revision.generation}
      </Row>
    </dl>
  );
}

function Row({ label, children, mono }: { label: string; children: string; mono?: boolean }) {
  return (
    <div className="flex min-w-0 items-baseline gap-1.5">
      <dt className="shrink-0 uppercase tracking-[0.08em] text-text-muted">{label}</dt>
      <dd className={cn('min-w-0 flex-1 truncate text-right text-text-secondary', mono && 'td-value')} title={children}>
        {children}
      </dd>
    </div>
  );
}

/** The change vocabulary with the counts of this union, so a mark on a row is
 * read against a stated rule and a stated total. */
function ChangeLegend({ layout }: { layout: RevisionPairUnionLayoutV1 }) {
  const files = countChanges(layout.files);
  const symbols = countChanges(layout.symbols);
  return (
    <dl className="grid grid-cols-2 gap-x-3 gap-y-1 text-3xs leading-snug md:grid-cols-4">
      {(Object.keys(COMPARE_CHANGE_RULES) as RevisionPairChangeV1[]).map((change) => (
        <div key={change} className="flex min-w-0 flex-col gap-0.5" data-compare-change={change}>
          <div className="flex items-center gap-1.5">
            <ChangeMark change={change} />
            <dt className="td-legend">{COMPARE_CHANGE_RULES[change].label}</dt>
          </div>
          <dd className="td-value text-text-secondary" data-cell="numeric">
            {files[change].toLocaleString()} files · {symbols[change].toLocaleString()} symbols
          </dd>
          <dd className="text-text-muted">{COMPARE_CHANGE_RULES[change].rule}</dd>
        </div>
      ))}
    </dl>
  );
}

/** The mark itself. `removed` is an outline with no fill — the former space —
 * and `added` is filled; `changed` and `unchanged` differ by weight. Colour
 * never carries the class alone: every row also prints its label. */
function ChangeMark({ change }: { change: RevisionPairChangeV1 }) {
  return (
    <span
      aria-hidden
      data-change-mark={change}
      className={cn(
        'inline-block size-2.5 shrink-0',
        change === 'removed' && 'border border-dashed border-edge-strong',
        change === 'added' && 'bg-accent',
        change === 'changed' && 'border-2 border-edge-strong',
        change === 'unchanged' && 'border border-edge-subtle',
      )}
    />
  );
}

function FileRegions({ layout }: { layout: RevisionPairUnionLayoutV1 }) {
  const groups = groupSymbolsByFile(layout);
  return (
    <ol className="flex flex-col gap-1.5" aria-label="File regions in identity order">
      {groups.map((group) => (
        <li key={group.fileIdentity} data-file-identity={group.fileIdentity}>
          {group.file ? (
            <FileRegionRow region={group.file} />
          ) : (
            <p className="td-legend px-2 py-1 normal-case tracking-normal text-text-muted">
              symbols whose file region is outside the active filter
            </p>
          )}
          {group.symbols.length > 0 ? (
            <ol className="flex flex-col border-l border-edge-subtle pl-2">
              {group.symbols.map((symbol) => (
                <SymbolRegionRow key={symbol.symbol_identity} region={symbol} />
              ))}
            </ol>
          ) : null}
        </li>
      ))}
    </ol>
  );
}

/** Both sides of one file identity. A side with no file is drawn as its
 * outlined former (or not-yet) space, so the column keeps its geometry. */
function FileRegionRow({ region }: { region: RevisionPairFileRegionV1 }) {
  const path = (region.head ?? region.base)?.path ?? region.file_identity;
  return (
    <div
      className={cn(
        'grid grid-cols-[auto_1fr_1fr] items-center gap-2 border px-2 py-1.5',
        region.change === 'removed' ? 'border-dashed border-edge-strong' : 'border-edge-subtle td-raised',
      )}
      data-region-change={region.change}
    >
      <span className="flex items-center gap-1.5">
        <ChangeMark change={region.change} />
        <span className="td-legend w-20 shrink-0">{COMPARE_CHANGE_RULES[region.change].label}</span>
      </span>
      <SideCell side="base" file={region.base} fallbackPath={path} />
      <SideCell side="head" file={region.head} fallbackPath={path} />
    </div>
  );
}

function SideCell({
  side,
  file,
  fallbackPath,
}: {
  side: 'base' | 'head';
  file: RevisionPairFileRegionV1['base'];
  fallbackPath: string;
}) {
  if (file === null) {
    return (
      <span
        className="td-value min-w-0 truncate border border-dashed border-edge-subtle px-1.5 py-0.5 text-3xs text-text-muted"
        data-side={side}
        title={`${fallbackPath} is not present in ${side}`}
      >
        not in {side}
      </span>
    );
  }
  return (
    <span className="flex min-w-0 flex-col" data-side={side}>
      <span className="td-value min-w-0 truncate text-2xs text-text-primary" title={file.path}>
        {elideStart(file.path, 40)}
      </span>
      <span className="td-value min-w-0 truncate text-3xs text-text-muted" title={file.content_digest}>
        {file.disposition} · {file.symbol_identities.length.toLocaleString()} symbols ·{' '}
        {file.content_digest.slice(0, 19)}
      </span>
    </span>
  );
}

function SymbolRegionRow({ region }: { region: RevisionPairSymbolRegionV1 }) {
  const shown = region.head ?? region.base;
  return (
    <li
      className="flex min-w-0 items-center gap-2 border-b border-edge-subtle px-2 py-1 text-2xs last:border-b-0"
      data-region-change={region.change}
    >
      <ChangeMark change={region.change} />
      <span className="td-legend w-20 shrink-0">{COMPARE_CHANGE_RULES[region.change].label}</span>
      <span className="td-legend w-20 shrink-0 truncate max-md:hidden">{shown?.kind ?? '—'}</span>
      <span className="td-value min-w-0 flex-1 truncate text-text-primary" title={shown?.qualified_name}>
        {shown?.qualified_name ?? region.symbol_identity}
      </span>
      {region.change === 'changed' && region.base && region.head ? (
        <span className="td-value shrink-0 text-3xs text-text-muted" title="content digests differ between base and head">
          {region.base.content_digest.slice(0, 11)} → {region.head.content_digest.slice(0, 11)}
        </span>
      ) : null}
    </li>
  );
}
