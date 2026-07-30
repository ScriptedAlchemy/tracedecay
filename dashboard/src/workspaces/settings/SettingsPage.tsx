import { useCallback, useMemo, useRef, useState } from 'react';
import { Search, X } from 'lucide-react';
import {
  EnvelopeSchema,
  SettingsPayloadV1Schema,
  type SettingsPayloadV1,
  type WireLegalActionRef,
} from '../../contracts/wire.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  scopeWritable,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from '../../data/scope/store.ts';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { WorkspaceHeader } from '../../ui/instrument.tsx';
import {
  SettingsEditorPanel,
  settingsWriteGate,
  type WritableScopes,
} from './SettingsEditorController.tsx';
import { OriginBand, SectionIndex } from './SettingsProvenance.tsx';
import { ConfigSectionBlock } from './SettingsValues.tsx';
import {
  buildSettingsModel,
  countSettings,
  filterOverrides,
  filterRows,
} from './settingsModel.ts';

/**
 * Settings: effective configuration, with provenance shown exactly as far as
 * the wire supports it and no further.
 *
 * `/api/settings` reports effective values — it does not attribute individual
 * keys to the file or default that set them, and the groups it returns do not
 * address a shared key namespace. So this surface does not draw a
 * layer-override stack; that would be a fabrication. What it does show is real:
 *
 *   - ORIGIN per group: the file path or endpoint the payload names as the
 *     source of that group's values (`config_path` / `config_endpoint`).
 *   - EXPLICIT vs DEFAULT for process-environment overrides, the one place the
 *     payload carries per-value provenance (`environment.variables[].active`).
 *   - The gap itself, stated on the surface rather than papered over.
 *
 * Values are typed at render: booleans are lamped pills, numbers tabular, paths
 * dim their directory so the meaningful tail reads first. Every literal comes
 * from `/api/settings`, parsed by the generated contract before it gets here.
 */
export function SettingsPage() {
  const scope = useScope((state) => state.scope);
  const settings = useLegacy(
    ['settings'],
    '/api/settings',
    EnvelopeSchema(SettingsPayloadV1Schema),
  );
  const readUrl = scopedUrl(scope, '/api/settings');
  // Both patch routes are addressed through the project gateway, so whether
  // this scope accepts a write bears on both scopes' editors.
  const writability = scopeWritable(scope);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <LegacyBoundary title="Settings" pending={settings.isPending} result={settings.data}>
        {(envelope) => (
          <SettingsSurface
            payload={envelope.payload}
            writable={writableScopes(envelope.legal_actions, writability)}
            writability={writability}
            readUrl={readUrl}
            projectPatchUrl={scopedUrl(scope, '/api/settings/project')}
            userPatchUrl={scopedUrl(scope, '/api/settings/user')}
            onApplied={() => void settings.refetch()}
          />
        )}
      </LegacyBoundary>
    </div>
  );
}

/**
 * Which settings scopes the server currently authorizes a write for.
 *
 * The two scopes have different authorities — a project batch is applied by
 * the daemon-owned configuration control plane, user settings by the profile
 * authority — so the envelope advertises them separately and a dashboard
 * without the control plane omits the project action. Offering the editor
 * anyway would put a control on screen whose only outcome is a 503.
 */
function writableScopes(
  legalActions: readonly WireLegalActionRef[],
  writability: ScopeWritability,
): WritableScopes {
  const authorizes = (operation: string) =>
    legalActions.some(
      (action) => action.kind === 'request_apply' && action.operation === operation,
    );
  return {
    project: settingsWriteGate(authorizes('configuration_batch'), writability),
    user: settingsWriteGate(authorizes('user_settings_mutate'), writability),
  };
}

function SettingsSurface({
  payload,
  writable,
  writability,
  readUrl,
  projectPatchUrl,
  userPatchUrl,
  onApplied,
}: {
  payload: SettingsPayloadV1;
  writable: WritableScopes;
  writability: ScopeWritability;
  readUrl: string;
  projectPatchUrl: string;
  userPatchUrl: string;
  onApplied: () => void;
}) {
  const model = useMemo(() => buildSettingsModel(payload), [payload]);
  const [query, setQuery] = useState('');
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const overrides = useMemo(
    () => filterOverrides(model.overrides, query),
    [model.overrides, query],
  );

  const filtered = useMemo(
    () =>
      model.sections
        .map((section) => ({ section, rows: filterRows(section.rows, query) }))
        // The environment section still earns its place while its generic rows
        // are filtered out, as long as an override matched the same query.
        .filter(
          (entry) =>
            entry.rows.length > 0 ||
            (entry.section.origin === 'environment' && overrides.length > 0),
        ),
    [model.sections, query, overrides.length],
  );

  const shown = useMemo(
    () => filtered.reduce((total, entry) => total + countSettings(entry.rows), 0),
    [filtered],
  );

  const jumpTo = useCallback((id: string) => {
    const container = scrollRef.current;
    // Interpolated unescaped, which is safe for a reason worth writing down:
    // section ids are the keys of `SettingsPayloadV1Schema`, a closed
    // `z.object` over Rust field names, so Zod has already stripped anything
    // that is not one of them. Were that not so, a daemon-chosen key holding a
    // quote would build a selector that does not parse, and `querySelector`
    // answers an unparseable selector by throwing — inside this handler.
    // `SettingsPage.dom.test.tsx` pins the premise rather than this line.
    const target = container?.querySelector<HTMLElement>(`[data-section="${id}"]`);
    if (!container || !target) return;
    container.scrollTo({ top: target.offsetTop - 4, behavior: 'auto' });
  }, []);

  return (
    <>
      <WorkspaceHeader
        // `channels.ts` keys its channel list on unprefixed paths, so a
        // leading slash here silently falls through to the `--` fallback.
        path="settings"
        title="Settings"
        note="effective configuration · validated changes"
        actions={
          model.stamps.length > 0 ? (
            // `min-w-0`, not `shrink-0`: this strip is 494.8px of snapshot,
            // revision, version and channel, and the header offers 254px at
            // 320 CSS px. Refusing to shrink did not make it fit — it put
            // 356.9px of provenance outside the header, most of it past the
            // screen edge. Allowed to shrink, its own `flex-wrap` lays the
            // stamps out over as many lines as the width needs and every one
            // of them stays readable.
            <span className="flex min-w-0 flex-wrap items-center gap-1.5">
              {model.stamps.map((stamp) => (
                <span
                  key={`${stamp.label}:${stamp.value}`}
                  className="inline-flex items-center gap-1 border border-edge-subtle px-1.5 py-0.5"
                >
                  <span className="td-legend">{stamp.label}</span>
                  <span className="td-value text-3xs text-text-secondary">
                    {stamp.value}
                  </span>
                </span>
              ))}
            </span>
          ) : null
        }
      />

      <div className="flex shrink-0 items-center gap-2.5 border-b border-edge-subtle px-3 py-2">
        <div className="relative min-w-0 flex-1 md:max-w-md">
          <Search
            aria-hidden
            size={13}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-text-muted"
          />
          <input
            ref={searchRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Escape' && query !== '') {
                event.stopPropagation();
                setQuery('');
              }
            }}
            placeholder="Filter keys and values…"
            aria-label="Filter configuration"
            className="h-[calc(var(--touch-target-min)+2px)] w-full rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 pl-7 pr-7 text-xs text-text-primary outline-none placeholder:text-text-muted focus-visible:border-accent"
          />
          {query !== '' ? (
            <button
              type="button"
              onClick={() => {
                setQuery('');
                searchRef.current?.focus();
              }}
              aria-label="Clear filter"
              className="absolute right-1.5 top-1/2 flex size-5 -translate-y-1/2 items-center justify-center text-text-muted hover:text-text-primary"
            >
              <X aria-hidden size={12} />
            </button>
          ) : null}
        </div>
        <p className="td-value shrink-0 text-3xs text-text-muted" aria-live="polite">
          {query === ''
            ? `${model.settingCount} settings`
            : `${shown} of ${model.settingCount} settings`}
        </p>
      </div>

      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        <SectionIndex entries={filtered} total={model.sections.length} onJump={jumpTo} />
        <div
          ref={scrollRef}
          tabIndex={0}
          role="region"
          aria-label="Effective configuration"
          // Stacked below `md` the section index takes its content height
          // first, and this pane — a scroll container, so its automatic
          // minimum size is zero — took the whole shortfall and resolved to
          // `height: 0` at 400% zoom, hiding 4,388px of configuration behind a
          // live "N settings" count. Same floor as the split archetype: keep a
          // readable pane and let the page scroller carry the overflow.
          className="min-h-[var(--pane-min-height)] min-w-0 flex-1 overflow-auto"
        >
          {filtered.length === 0 ? (
            <p className="p-8 text-center text-xs text-text-muted">
              no key or value matches “{query}”
            </p>
          ) : (
            <>
              {query === '' ? (
                <SettingsEditorPanel
                  payload={payload}
                  writable={writable}
                  writability={writability}
                  readUrl={readUrl}
                  projectPatchUrl={projectPatchUrl}
                  userPatchUrl={userPatchUrl}
                  onApplied={onApplied}
                />
              ) : null}
              {query === '' ? <OriginBand model={model} onJump={jumpTo} /> : null}
              {filtered.map(({ section, rows }) => (
                <ConfigSectionBlock
                  key={section.id}
                  section={section}
                  rows={rows}
                  overrides={section.origin === 'environment' ? overrides : []}
                  query={query}
                />
              ))}
            </>
          )}
        </div>
      </div>
    </>
  );
}
