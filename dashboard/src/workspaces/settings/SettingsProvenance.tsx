/**
 * Provenance display: where each group of settings is read from, and which
 * process-environment overrides are actually in force.
 *
 * This surface states ORIGIN, never precedence. `/api/settings` reports
 * effective values without attributing an individual key to the file or
 * default that set it, and the groups it returns do not address a shared key
 * namespace — so an override stack drawn here would be a fabrication. The gap
 * is stated on the surface instead of papered over.
 */

import { Lamp } from '../../ui/instrument.tsx';
import { countSettings, type ConfigRow, type ConfigSection, type SettingsModel } from './settingsModel.ts';
import { ORIGIN_WORD, OriginMark, PathText } from './SettingsValues.tsx';

export function SectionIndex({
  entries,
  total,
  onJump,
}: {
  entries: ReadonlyArray<{ section: ConfigSection; rows: ConfigRow[] }>;
  total: number;
  onJump: (id: string) => void;
}) {
  return (
    <nav
      aria-label="Configuration groups"
      tabIndex={0}
      className="flex max-h-28 w-full shrink-0 flex-col overflow-auto border-b border-edge-subtle bg-surface-1 md:max-h-none md:w-48 md:border-b-0 md:border-r"
    >
      <div className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <span className="td-title">
          {entries.length === total ? 'Groups' : `${entries.length}/${total} groups`}
        </span>
        <span aria-hidden className="td-rule" />
      </div>
      <div className="grid grid-cols-2 p-1.5 sm:grid-cols-3 md:flex md:flex-col">
        {entries.map(({ section, rows }) => (
          <button
            key={section.id}
            type="button"
            onClick={() => onJump(section.id)}
            className="flex min-h-[var(--touch-target-min)] items-center gap-2 px-1.5 py-1.5 text-left text-xs text-text-secondary hover:bg-surface-2 hover:text-text-primary focus-visible:bg-surface-2"
          >
            <OriginMark origin={section.origin} />
            <span className="min-w-0 flex-1 truncate">{section.title}</span>
            <span className="td-value shrink-0 text-3xs text-text-muted">
              {countSettings(rows)}
            </span>
          </button>
        ))}
      </div>
    </nav>
  );
}

/**
 * The provenance headline — deliberately a statement of ORIGIN, not of
 * precedence. Each card names the source the payload gives for that group and
 * how many values it carries. The band also states, in plain text, the thing
 * the payload cannot tell us, because a surface that quietly implies per-key
 * layer attribution would be lying.
 */
export function OriginBand({
  model,
  onJump,
}: {
  model: SettingsModel;
  onJump: (id: string) => void;
}) {
  if (model.sections.length === 0) return null;
  return (
    <section aria-labelledby="settings-origins" className="border-b border-edge-subtle p-3">
      <div className="mb-1.5 flex items-center gap-2.5">
        <h2 id="settings-origins" className="td-title">
          Provenance
        </h2>
        <span aria-hidden className="td-rule" />
      </div>
      <p className="mb-2.5 max-w-3xl text-2xs leading-relaxed text-text-muted">
        <span className="text-text-secondary">
          This API reports effective values only.
        </span>{' '}
        It does not attribute an individual key to the file or default that set
        it, and these groups do not address a shared key namespace — so no
        override order is shown. What is real: where each group is read from,
        and which process-environment overrides are actually in force.
      </p>
      <ul className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
        {model.sections.map((section) => (
          <li key={section.id} className="min-w-0">
            <button
              type="button"
              onClick={() => onJump(section.id)}
              className="flex w-full min-w-0 flex-col gap-1 border border-edge-subtle bg-surface-1 px-2.5 py-2 text-left hover:border-edge-strong focus-visible:border-accent"
            >
              <span className="flex min-w-0 items-center gap-2">
                <OriginMark origin={section.origin} />
                <span className="min-w-0 flex-1 truncate text-xs font-semibold text-text-primary">
                  {section.title}
                </span>
                <span className="td-value shrink-0 text-3xs text-text-muted">
                  {section.settingCount}
                </span>
              </span>
              <span className="td-legend truncate">{ORIGIN_WORD[section.origin]}</span>
              <span className="block truncate text-2xs text-text-muted">
                {section.blurb}
              </span>
              {section.location ? (
                <span className="td-value block min-w-0 break-all text-3xs">
                  {section.locationKind === 'path' ? (
                    <PathText value={section.location} />
                  ) : (
                    <span className="text-text-secondary">{section.location}</span>
                  )}
                </span>
              ) : null}
              {section.origin === 'environment' && model.overrides.length > 0 ? (
                <span className="flex items-center gap-1.5 pt-0.5">
                  <Lamp
                    tone={model.activeOverrides > 0 ? 'bg-state-ready' : 'bg-surface-3'}
                  />
                  <span className="text-2xs text-text-secondary">
                    {model.activeOverrides} of {model.overrides.length} overrides in
                    force
                  </span>
                </span>
              ) : null}
              {section.notes.length > 0 ? (
                <span className="flex flex-wrap gap-1 pt-0.5">
                  {section.notes.map((note) => (
                    <span
                      key={note}
                      className="border border-edge-subtle px-1.5 py-px text-2xs text-text-secondary"
                    >
                      {note}
                    </span>
                  ))}
                </span>
              ) : null}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
