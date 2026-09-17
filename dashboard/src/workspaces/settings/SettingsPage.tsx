import { useCallback, useMemo, useRef, useState } from 'react';
import { Search, X } from 'lucide-react';
import {
  DashboardEnvelopeV1Schema,
  SettingsPayloadV1Schema,
  type DashboardEnvelopeV1,
  type SettingsPayloadV1,
} from '../../contracts/generated.ts';
import { usePayload } from '../../data/query/usePayload.ts';
import {
  scopeWritable,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from '../../data/scope/store.ts';
import { cn } from '../../ui/cn';
import { PayloadBoundary } from '../../ui/ReadSection.tsx';
import { WorkspaceHeader } from '../../ui/instrument.tsx';
import { EffectiveConfigTable, type SectionGroup } from './EffectiveConfigTable.tsx';
import { useSettingsEditor } from './SettingsEditorController.tsx';
import {
  settingsApplied,
  settingsRejection,
  settingsScopeDirty,
  type SettingsEditorState,
  type SettingsRoutes,
} from './settingsEditorMachine.ts';
import { writableScopes } from './settingsGates.ts';
import { SettingsInspector } from './SettingsInspector.tsx';
import { buildSettingsModel, type SettingsModel } from './settingsModel.ts';
import { effectiveRows, filterEffectiveRows, type EffectiveRow } from './settingsRows.ts';
import { SectionsRail } from './SettingsSections.tsx';

/**
 * Settings: the effective-configuration review.
 *
 * `/api/settings` reports effective values. It does not attribute individual
 * keys to the layer that set them, and its groups do not address a shared key
 * namespace, so this surface draws no override stack. What it draws is real:
 *
 *   - one row per served key with its effective value;
 *   - PROVENANCE exactly as far as the wire carries it — `explicit`/`default`
 *     for process-environment overrides, `unserved` for everything else — plus
 *     `edited` for this reader's own unapplied proposal;
 *   - ORIGIN per group: the file path or endpoint the payload names, or a
 *     stated absence;
 *   - WRITE per key: editable through the compare-and-swap PATCH path, locked
 *     by a named gate, or without any write path at all.
 *
 * Hover and focus inspect; click selects and opens the review under the row.
 * The effective value stays authoritative until the write authority validates,
 * persists against the held revision, reports its apply requirement, and the
 * read comes back.
 */
export function SettingsPage() {
  const scope = useScope((state) => state.scope);
  const settings = usePayload(
    ['settings'],
    '/api/settings',
    DashboardEnvelopeV1Schema(SettingsPayloadV1Schema),
  );
  const readUrl = scopedUrl(scope, '/api/settings');
  // Project and ordinary user writes are addressed through the project gateway.
  // The ProfileSessions worker resource deliberately is not.
  const writability = scopeWritable(scope);
  const routes = useMemo<SettingsRoutes>(
    () => ({
      readUrl,
      codeIndexWorkerReadUrl: '/api/settings',
      projectPatchUrl: scopedUrl(scope, '/api/settings/project'),
      userPatchUrl: scopedUrl(scope, '/api/settings/user'),
      codeIndexWorkerPatchUrl: '/api/settings/user/code-index-workers',
    }),
    [readUrl, scope],
  );
  const refetch = settings.refetch;
  const onApplied = useCallback(() => void refetch(), [refetch]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <PayloadBoundary title="Settings" pending={settings.isPending} result={settings.data}>
        {(envelope) => (
          <SettingsSurface
            envelope={envelope}
            writability={writability}
            routes={routes}
            onApplied={onApplied}
          />
        )}
      </PayloadBoundary>
    </div>
  );
}

/**
 * The rendered block for one configuration section, matched by comparing the
 * attribute's value rather than by interpolating it into a selector.
 *
 * `[data-section="${id}"]` is safe only for as long as every id is a bare
 * identifier. Today it is: `SettingsPayloadV1Schema` is a plain `z.object`, so
 * zod strips every key the contract does not name and the groups that survive
 * are all identifiers. But `buildSettingsModel` takes ids straight from the
 * payload's top-level keys and is written to accept `unknown` precisely so a
 * group the daemon starts reporting appears rather than vanishes — so the
 * safety rests on a parse step outside this function. Comparison is preferred
 * to `CSS.escape` because there is no escaping to get right, it costs one pass
 * over a handful of headings, and it stays exercisable under jsdom.
 */
export function findConfigSection(
  container: ParentNode,
  id: string,
): HTMLElement | undefined {
  return Array.from(container.querySelectorAll<HTMLElement>('[data-section]')).find(
    (section) => section.dataset['section'] === id,
  );
}

function SettingsSurface({
  envelope,
  writability,
  routes,
  onApplied,
}: {
  envelope: DashboardEnvelopeV1<SettingsPayloadV1>;
  writability: ScopeWritability;
  routes: SettingsRoutes;
  onApplied: () => void;
}) {
  const payload = envelope.payload;
  const model = useMemo(() => buildSettingsModel(payload), [payload]);
  const gates = useMemo(
    () => writableScopes(envelope.legal_actions, writability),
    [envelope.legal_actions, writability],
  );
  const editor = useSettingsEditor({ payload, routes, writability, onApplied });

  const [query, setQuery] = useState('');
  const [inspectedKey, setInspectedKey] = useState<string | null>(null);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const rows = useMemo(() => effectiveRows(model), [model]);
  const filtered = useMemo(() => filterEffectiveRows(rows, query), [rows, query]);
  const groups = useMemo(() => groupBySection(model, filtered), [model, filtered]);

  const rowByKey = useCallback(
    (key: string | null) => (key === null ? null : rows.find((row) => row.key === key) ?? null),
    [rows],
  );
  // The inspector follows the pointer and focus; when neither is on a row it
  // holds the selected one, so opening a review never empties it.
  const inspectedRow = rowByKey(inspectedKey) ?? rowByKey(selectedKey);

  const jumpTo = useCallback((id: string) => {
    const container = scrollRef.current;
    if (!container) return;
    const target = findConfigSection(container, id);
    if (!target) return;
    container.scrollTo({ top: target.offsetTop - 4, behavior: 'auto' });
  }, []);

  return (
    <>
      <WorkspaceHeader
        // `channels.ts` keys its channel list on unprefixed paths, so a
        // leading slash here silently falls through to the `--` fallback.
        path="settings"
        title="Settings"
        note="effective configuration review"
      />

      <SettingsRegister
        query={query}
        onQuery={setQuery}
        searchRef={searchRef}
        shown={filtered.length}
        total={rows.length}
        writability={writability}
        state={editor.state}
      />

      <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
        <SectionsRail
          entries={groups.map(({ section, rows: sectionRows }) => ({
            section,
            count: sectionRows.length,
          }))}
          total={model.sections.length}
          activeId={inspectedRow?.section.id ?? null}
          onJump={jumpTo}
        />

        <section
          aria-label="Effective settings"
          className="flex min-h-0 min-w-0 flex-1 flex-col"
        >
          {filtered.length === 0 ? (
            <div
              ref={scrollRef}
              role="status"
              className="td-graticule flex min-h-[var(--pane-min-height)] flex-1 items-center justify-center p-8"
            >
              <p className="border border-dashed border-edge-strong px-4 py-3 text-center text-xs text-text-muted">
                no key or value matches “{query}”
              </p>
            </div>
          ) : (
            <EffectiveConfigTable
              groups={groups}
              query={query}
              gates={gates}
              editor={editor}
              workerStatus={payload.user.code_index_worker_status}
              inspectedKey={inspectedKey}
              selectedKey={selectedKey}
              onInspect={setInspectedKey}
              onSelect={setSelectedKey}
              scrollRef={scrollRef}
            />
          )}
          <p className="shrink-0 border-t border-edge-subtle px-3 py-1.5 text-3xs leading-relaxed text-text-muted">
            Showing effective configuration only. Origins are shown only when the server names
            them; otherwise a key’s provenance is <span className="td-value">unserved</span>. Hover
            or focus a row to inspect it; Enter or click opens its review; Escape closes.
          </p>
        </section>

        {/* Stacked below `lg` the inspector takes a fixed band and scrolls
          * inside it, the same bound the split archetype uses: left to its
          * content height it took ~900px, squeezed the table to its floor,
          * and the page scrolled past the rows a reader came for. */}
        <aside
          aria-label="Inspector"
          tabIndex={0}
          className="w-full shrink-0 overflow-auto border-t border-edge-subtle bg-surface-1 max-lg:h-64 lg:w-80 lg:border-l lg:border-t-0 xl:w-[22rem]"
        >
          <SettingsInspector
            envelope={envelope}
            model={model}
            row={inspectedRow}
            gates={gates}
            state={editor.state}
            readUrl={routes.readUrl}
            writability={writability}
            workerStatus={payload.user.code_index_worker_status}
          />
        </aside>
      </div>
    </>
  );
}

/** Filtered rows regrouped under their sections, in the model's origin order. */
function groupBySection(model: SettingsModel, rows: readonly EffectiveRow[]): SectionGroup[] {
  const bySection = new Map<string, EffectiveRow[]>();
  for (const row of rows) {
    const bucket = bySection.get(row.section.id);
    if (bucket) bucket.push(row);
    else bySection.set(row.section.id, [row]);
  }
  return model.sections.flatMap((section) => {
    const sectionRows = bySection.get(section.id);
    return sectionRows ? [{ section, rows: sectionRows }] : [];
  });
}

/**
 * The register under the header: the search over key, value, and served
 * description, and the three readings a reader needs before touching a row —
 * what the scope permits, that this is the effective-only mode, and where the
 * one review the editor can hold currently stands.
 */
function SettingsRegister({
  query,
  onQuery,
  searchRef,
  shown,
  total,
  writability,
  state,
}: {
  query: string;
  onQuery: (query: string) => void;
  searchRef: React.RefObject<HTMLInputElement | null>;
  shown: number;
  total: number;
  writability: ScopeWritability;
  state: SettingsEditorState;
}) {
  const review = reviewReading(state);
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-edge-subtle bg-surface-1 px-3 py-2">
      <div className="relative min-w-0 flex-1 basis-64 md:max-w-md">
        <Search
          aria-hidden
          size={13}
          className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-text-muted"
        />
        <input
          ref={searchRef}
          value={query}
          onChange={(event) => onQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Escape' && query !== '') {
              event.stopPropagation();
              onQuery('');
            }
          }}
          placeholder="Search effective settings (key or value)…"
          aria-label="Filter configuration"
          className="h-[calc(var(--touch-target-min)+2px)] w-full rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 pl-7 pr-7 font-mono text-xs text-text-primary outline-none placeholder:font-sans placeholder:text-text-muted focus-visible:border-accent"
        />
        {query !== '' ? (
          <button
            type="button"
            onClick={() => {
              onQuery('');
              searchRef.current?.focus();
            }}
            aria-label="Clear filter"
            className="td-hit absolute right-0 top-1/2 -translate-y-1/2 text-text-muted hover:text-text-primary"
          >
            <X aria-hidden size={12} />
          </button>
        ) : null}
      </div>
      <p className="td-value shrink-0 text-3xs text-text-muted" aria-live="polite">
        {query === '' ? `${total} settings` : `${shown} of ${total} settings`}
      </p>
      <dl className="ml-auto flex flex-wrap items-center gap-x-4 gap-y-1">
        <Register label="scope" tone={writability.state === 'writable' ? 'text-text-primary' : 'text-state-locked'}>
          {writability.state === 'writable' ? 'writable' : writability.state.replace('_', '-')}
        </Register>
        <Register label="mode">effective-only</Register>
        <Register label="review" tone={review.tone} data-review={review.value}>
          {review.value}
        </Register>
      </dl>
    </div>
  );
}

function Register({
  label,
  tone = 'text-text-secondary',
  children,
  ...data
}: {
  label: string;
  tone?: string;
  children: React.ReactNode;
  'data-review'?: string;
}) {
  return (
    <div className="flex items-baseline gap-2" {...data}>
      <dt className="td-legend">{label}</dt>
      <dd className={cn('td-value text-2xs', tone)}>{children}</dd>
    </div>
  );
}

/** Where the editor's one review stands, as one word the register can carry. */
function reviewReading(state: SettingsEditorState): { value: string; tone: string } {
  switch (state.status) {
    case 'editor_unavailable':
      return { value: 'unavailable', tone: 'text-state-error' };
    case 'editing': {
      const applied = settingsApplied(state);
      if (applied) return { value: 'applied', tone: 'text-state-ready' };
      if (settingsRejection(state)) return { value: 'rejected', tone: 'text-state-error' };
      const dirty = (['project', 'user', 'code_index_workers'] as const).some((scope) =>
        settingsScopeDirty(state, scope),
      );
      return dirty
        ? { value: 'proposal', tone: 'text-accent' }
        : { value: 'none', tone: 'text-text-muted' };
    }
    case 'reviewing':
      return { value: 'pending', tone: 'text-accent' };
    case 'confirmed':
      return { value: 'confirmed', tone: 'text-accent' };
    case 'submitting':
      return { value: 'applying', tone: 'text-state-loading' };
    case 'conflicted':
      return { value: 'conflict', tone: 'text-state-conflicting' };
    case 'review_superseded':
      return { value: 'superseded', tone: 'text-state-conflicting' };
    case 'authority_withdrawn':
      return { value: 'withdrawn', tone: 'text-state-offline' };
    case 'submit_failed':
      return { value: 'failed', tone: 'text-state-error' };
    default: {
      const exhaustive: never = state;
      return exhaustive;
    }
  }
}
