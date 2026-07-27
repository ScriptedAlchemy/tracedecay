import * as Dialog from '@radix-ui/react-dialog';
import { useMutation } from '@tanstack/react-query';
import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react';
import { Search, X } from 'lucide-react';
import {
  EnvelopeSchema,
  SettingsPayloadV1Schema,
  type WireLegalActionRef,
} from '../../contracts/wire.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { scopedUrl, useScope } from '../../data/scope/store.ts';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { cn } from '../../ui/cn';
import { Lamp, WorkspaceHeader } from '../../ui/instrument.tsx';
import {
  buildSettingsEditor,
  buildSettingsModel,
  countSettings,
  filterOverrides,
  filterRows,
  planProjectSettingsChange,
  planUserSettingsChange,
  splitPath,
  type ConfigRow,
  type ConfigSection,
  type EnvOverride,
  type OriginKind,
  type ProjectSettingsChangeSet,
  type ProjectSettingsValues,
  type SettingsChangePlan,
  type SettingsModel,
  type SettingsValidationError,
  type UserSettingsChangeSet,
  type UserSettingsValues,
} from './settingsModel.ts';
import {
  applySettingsMutation,
  type SettingsMutationResult,
  type SettingsMutationScope,
} from './settingsMutation.ts';

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
 * dim their directory so the meaningful tail reads first. Read-only, and every
 * literal comes from `/api/settings`.
 */
export function SettingsPage() {
  const scope = useScope((state) => state.scope);
  const settings = useLegacy(
    ['settings'],
    '/api/settings',
    EnvelopeSchema(SettingsPayloadV1Schema),
  );
  const readUrl = scopedUrl(scope, '/api/settings');

  return (
    <div className="flex h-full min-h-0 flex-col">
      <LegacyBoundary title="Settings" pending={settings.isPending} result={settings.data}>
        {(envelope) => (
          <SettingsSurface
            payload={envelope.payload}
            writable={writableScopes(envelope.legal_actions)}
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
function writableScopes(legalActions: readonly WireLegalActionRef[]): WritableScopes {
  const authorizes = (operation: string) =>
    legalActions.some(
      (action) => action.kind === 'request_apply' && action.operation === operation,
    );
  return {
    project: authorizes('configuration_batch'),
    user: authorizes('user_settings_mutate'),
  };
}

interface WritableScopes {
  readonly project: boolean;
  readonly user: boolean;
}

function SettingsSurface({
  payload,
  writable,
  readUrl,
  projectPatchUrl,
  userPatchUrl,
  onApplied,
}: {
  payload: unknown;
  writable: WritableScopes;
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
            <span className="flex shrink-0 flex-wrap items-center gap-1.5">
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

type ReadyProjectPlan = Extract<
  SettingsChangePlan<ProjectSettingsChangeSet>,
  { outcome: 'ready' }
>;
type ReadyUserPlan = Extract<SettingsChangePlan<UserSettingsChangeSet>, { outcome: 'ready' }>;
type PendingSettingsReview =
  | { readonly scope: 'project'; readonly plan: ReadyProjectPlan }
  | { readonly scope: 'user'; readonly plan: ReadyUserPlan };

function SettingsEditorPanel({
  payload,
  writable,
  readUrl,
  projectPatchUrl,
  userPatchUrl,
  onApplied,
}: {
  payload: unknown;
  writable: WritableScopes;
  readUrl: string;
  projectPatchUrl: string;
  userPatchUrl: string;
  onApplied: () => void;
}) {
  const editor = useMemo(() => buildSettingsEditor(payload), [payload]);
  const [project, setProject] = useState<ProjectSettingsValues | null>(
    editor?.project ?? null,
  );
  const [user, setUser] = useState<UserSettingsValues | null>(editor?.user ?? null);
  const [review, setReview] = useState<PendingSettingsReview | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [clientErrors, setClientErrors] = useState<readonly SettingsValidationError[]>(
    [],
  );
  const [notice, setNotice] = useState<{
    readonly message: string;
    readonly resyncRecommended: boolean;
    readonly restartRecommended: boolean;
  } | null>(null);
  const projectPlan = useMemo(
    () => (project ? planProjectSettingsChange(payload, project) : null),
    [payload, project],
  );
  const userPlan = useMemo(
    () => (user ? planUserSettingsChange(payload, user) : null),
    [payload, user],
  );
  const mutation = useMutation({
    mutationFn: applySettingsMutation,
    onSuccess: (result) => {
      if (result.outcome === 'validation') {
        setClientErrors(result.errors);
        setReview(null);
        setConfirmed(false);
        return;
      }
      if (result.outcome !== 'success') return;
      setNotice({
        message:
          result.scope === 'project' ? 'Project settings saved' : 'User settings saved',
        resyncRecommended: result.resyncRecommended,
        restartRecommended: result.restartRecommended,
      });
      setReview(null);
      setConfirmed(false);
      onApplied();
    },
  });

  useEffect(() => {
    setProject(editor?.project ?? null);
    setUser(editor?.user ?? null);
  }, [editor]);

  if (!editor || !project || !user) {
    return (
      <section className="border-b border-edge-subtle p-3" aria-label="Supported settings changes">
        <p className="text-xs text-state-error">
          Settings editing requires project configuration values and configuration_revision_id
          from GET /api/settings, plus user settings and user_settings_revision_id from the same
          authority. The response omitted at least one required field.
        </p>
      </section>
    );
  }

  const openProjectReview = () => {
    if (projectPlan) {
      openReview('project', projectPlan, setReview, setClientErrors, mutation.reset);
    }
  };
  const openUserReview = () => {
    if (userPlan) {
      openReview('user', userPlan, setReview, setClientErrors, mutation.reset);
    }
  };
  const apply = () => {
    if (!review) return;
    mutation.mutate({
      scope: review.scope,
      expectedRevisionId: review.plan.expectedRevisionId,
      readUrl,
      patchUrl: review.scope === 'project' ? projectPatchUrl : userPatchUrl,
      patch: review.plan.patch,
    });
  };

  return (
    <section
      className="border-b border-edge-subtle bg-surface-0 p-3"
      aria-labelledby="settings-editor-title"
    >
      <div className="mb-3 flex flex-wrap items-baseline gap-2">
        <h2 id="settings-editor-title" className="td-title">
          Supported settings changes
        </h2>
        <span className="text-2xs text-text-muted">
          validate → review → confirm against the resource revision
        </span>
      </div>

      {notice ? (
        <div
          role="status"
          className="mb-3 flex flex-wrap gap-2 border border-state-ready/40 bg-surface-1 px-3 py-2 text-xs text-text-secondary"
        >
          <strong className="font-semibold text-text-primary">{notice.message}</strong>
          {notice.resyncRecommended ? <span>Resync recommended</span> : null}
          {notice.restartRecommended ? <span>Restart recommended</span> : null}
        </div>
      ) : null}

      <div className="grid gap-3 xl:grid-cols-2">
        <ProjectSettingsFields
          values={project}
          errors={clientErrors}
          dirty={projectPlan?.outcome !== 'unchanged'}
          writable={writable.project}
          onChange={setProject}
          onReview={openProjectReview}
        />
        <UserSettingsFields
          values={user}
          errors={clientErrors}
          dirty={userPlan?.outcome !== 'unchanged'}
          writable={writable.user}
          onChange={setUser}
          onReview={openUserReview}
        />
      </div>

      <SettingsReviewDialog
        review={review}
        confirmed={confirmed}
        result={mutation.data}
        applying={mutation.isPending}
        onConfirmedChange={setConfirmed}
        onResolveConflict={() => {
          setReview(null);
          setConfirmed(false);
          mutation.reset();
          onApplied();
        }}
        onOpenChange={(open) => {
          if (!open) {
            setReview(null);
            setConfirmed(false);
            mutation.reset();
          }
        }}
        onApply={apply}
      />
    </section>
  );
}

function openReview<T extends ProjectSettingsChangeSet | UserSettingsChangeSet>(
  scope: SettingsMutationScope,
  plan: SettingsChangePlan<T>,
  setReview: (review: PendingSettingsReview | null) => void,
  setErrors: (errors: readonly SettingsValidationError[]) => void,
  resetMutation: () => void,
): void {
  resetMutation();
  if (plan.outcome === 'invalid') {
    setReview(null);
    setErrors(plan.errors);
    return;
  }
  if (plan.outcome === 'unchanged') {
    setReview(null);
    setErrors([{ field: scope, message: `No ${scope} settings have changed.` }]);
    return;
  }
  setErrors([]);
  setReview(
    scope === 'project'
      ? { scope, plan: plan as ReadyProjectPlan }
      : { scope, plan: plan as ReadyUserPlan },
  );
}

function ProjectSettingsFields({
  values,
  errors,
  dirty,
  writable,
  onChange,
  onReview,
}: {
  values: ProjectSettingsValues;
  errors: readonly SettingsValidationError[];
  dirty: boolean;
  writable: boolean;
  onChange: (values: ProjectSettingsValues) => void;
  onReview: () => void;
}) {
  return (
    <fieldset
      className="min-w-0 border border-edge-subtle bg-surface-1 p-3"
      disabled={!writable}
    >
      <legend className="px-1 text-xs font-semibold text-text-primary">Project settings</legend>
      {writable ? <EditState scope="project" dirty={dirty} /> : <ReadOnlyScope scope="project" />}
      <div className="grid gap-2 sm:grid-cols-2">
        <SettingsTextArea
          label="Include globs"
          value={values.include.join('\n')}
          error={errorFor(errors, 'include')}
          onChange={(value) => onChange({ ...values, include: globLines(value) })}
        />
        <SettingsTextArea
          label="Exclude globs"
          value={values.exclude.join('\n')}
          error={errorFor(errors, 'exclude')}
          onChange={(value) => onChange({ ...values, exclude: globLines(value) })}
        />
        <SettingsInput
          label="Maximum file size (bytes)"
          inputMode="numeric"
          value={values.max_file_size}
          error={errorFor(errors, 'max_file_size')}
          onChange={(value) => onChange({ ...values, max_file_size: value })}
        />
        <SettingsInput
          label="PR branch poll interval (seconds)"
          inputMode="numeric"
          value={values.auto_track_pr_poll_secs}
          error={errorFor(errors, 'auto_track_pr_poll_secs')}
          onChange={(value) => onChange({ ...values, auto_track_pr_poll_secs: value })}
        />
      </div>
      <div className="mt-3 grid gap-2 sm:grid-cols-2">
        <SettingsCheckbox
          label="Extract docstrings"
          checked={values.extract_docstrings}
          error={errorFor(errors, 'extract_docstrings')}
          onChange={(checked) => onChange({ ...values, extract_docstrings: checked })}
        />
        <SettingsCheckbox
          label="Track call sites"
          checked={values.track_call_sites}
          error={errorFor(errors, 'track_call_sites')}
          onChange={(checked) => onChange({ ...values, track_call_sites: checked })}
        />
        <SettingsCheckbox
          label="Honor git ignore"
          checked={values.git_ignore}
          error={errorFor(errors, 'git_ignore')}
          onChange={(checked) => onChange({ ...values, git_ignore: checked })}
        />
        <SettingsCheckbox
          label="Record telemetry timings"
          checked={values.telemetry_timings}
          error={errorFor(errors, 'telemetry_timings')}
          onChange={(checked) => onChange({ ...values, telemetry_timings: checked })}
        />
        <SettingsCheckbox
          label="Auto-track pull request branches"
          checked={values.auto_track_pr_branches}
          error={errorFor(errors, 'auto_track_pr_branches')}
          onChange={(checked) => onChange({ ...values, auto_track_pr_branches: checked })}
        />
      </div>
      {writable ? (
        <button type="button" className={`${settingsButtonClass} mt-3`} onClick={onReview}>
          Review project changes
        </button>
      ) : null}
      <FieldError error={errorFor(errors, 'project')} />
    </fieldset>
  );
}

function UserSettingsFields({
  values,
  errors,
  dirty,
  writable,
  onChange,
  onReview,
}: {
  values: UserSettingsValues;
  errors: readonly SettingsValidationError[];
  dirty: boolean;
  writable: boolean;
  onChange: (values: UserSettingsValues) => void;
  onReview: () => void;
}) {
  return (
    <fieldset
      className="min-w-0 border border-edge-subtle bg-surface-1 p-3"
      disabled={!writable}
    >
      <legend className="px-1 text-xs font-semibold text-text-primary">User settings</legend>
      {writable ? <EditState scope="user" dirty={dirty} /> : <ReadOnlyScope scope="user" />}
      <div className="grid gap-2">
        <SettingsInput
          label="Watcher debounce"
          value={values.watcher_debounce}
          error={errorFor(errors, 'watcher_debounce')}
          onChange={(value) => onChange({ ...values, watcher_debounce: value })}
        />
        <SettingsInput
          label="Extraction timeout (seconds)"
          inputMode="numeric"
          value={values.extraction_timeout_secs}
          error={errorFor(errors, 'extraction_timeout_secs')}
          onChange={(value) => onChange({ ...values, extraction_timeout_secs: value })}
        />
        <SettingsCheckbox
          label="Upload enabled"
          checked={values.upload_enabled}
          error={errorFor(errors, 'upload_enabled')}
          onChange={(checked) => onChange({ ...values, upload_enabled: checked })}
        />
      </div>
      {writable ? (
        <button type="button" className={`${settingsButtonClass} mt-3`} onClick={onReview}>
          Review user changes
        </button>
      ) : null}
      <FieldError error={errorFor(errors, 'user')} />
    </fieldset>
  );
}

function SettingsInput({
  label,
  value,
  inputMode,
  error,
  onChange,
}: {
  label: string;
  value: string;
  inputMode?: 'numeric';
  error?: string;
  onChange: (value: string) => void;
}) {
  const errorId = useId();
  return (
    <label className="grid gap-1 text-2xs text-text-secondary">
      <span>{label}</span>
      <input
        value={value}
        inputMode={inputMode}
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        onChange={(event) => onChange(event.target.value)}
        className={settingsInputClass}
      />
      <FieldError id={errorId} error={error} />
    </label>
  );
}

function SettingsTextArea({
  label,
  value,
  error,
  onChange,
}: {
  label: string;
  value: string;
  error?: string;
  onChange: (value: string) => void;
}) {
  const errorId = useId();
  return (
    <label className="grid gap-1 text-2xs text-text-secondary">
      <span>{label}</span>
      <textarea
        rows={3}
        value={value}
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? errorId : undefined}
        onChange={(event) => onChange(event.target.value)}
        className={`${settingsInputClass} h-auto min-h-16 py-1.5 font-mono`}
      />
      <FieldError id={errorId} error={error} />
    </label>
  );
}

function SettingsCheckbox({
  label,
  checked,
  error,
  onChange,
}: {
  label: string;
  checked: boolean;
  error?: string;
  onChange: (checked: boolean) => void;
}) {
  const errorId = useId();
  return (
    <div>
      <label className="flex min-h-8 items-center gap-2 text-2xs text-text-secondary">
        <input
          type="checkbox"
          checked={checked}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          onChange={(event) => onChange(event.target.checked)}
        />
        <span>{label}</span>
      </label>
      <FieldError id={errorId} error={error} />
    </div>
  );
}

function SettingsReviewDialog({
  review,
  confirmed,
  result,
  applying,
  onConfirmedChange,
  onResolveConflict,
  onOpenChange,
  onApply,
}: {
  review: PendingSettingsReview | null;
  confirmed: boolean;
  result: SettingsMutationResult | undefined;
  applying: boolean;
  onConfirmedChange: (confirmed: boolean) => void;
  onResolveConflict: () => void;
  onOpenChange: (open: boolean) => void;
  onApply: () => void;
}) {
  const scope = review?.scope ?? 'project';
  const conflicted = result?.outcome === 'conflict';
  return (
    <Dialog.Root open={review != null} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/60" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[min(36rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-5 shadow-xl">
          <div className="flex items-start justify-between gap-3">
            <div>
              <Dialog.Title className="text-base font-semibold tracking-tight">
                Review {scope} settings change
              </Dialog.Title>
              <Dialog.Description className="mt-1 text-xs text-text-muted">
                Only the validated changed fields below will be sent. The held revision is checked
                again immediately before apply.
              </Dialog.Description>
            </div>
            <Dialog.Close
              aria-label="Close settings review"
              className="rounded-[var(--radius-chip)] p-1 text-text-muted hover:bg-surface-2"
            >
              <X aria-hidden size={16} />
            </Dialog.Close>
          </div>
          {review ? (
            <div className="mt-4 grid gap-3">
              <pre className="max-h-56 overflow-auto border border-edge-subtle bg-surface-0 p-3 text-2xs text-text-secondary">
                {JSON.stringify(review.plan.patch, null, 2)}
              </pre>
              <p className="break-all font-mono text-2xs text-text-muted">
                expected revision {review.plan.expectedRevisionId}
              </p>
              <label className="flex items-start gap-2 border border-edge-subtle p-3 text-xs text-text-secondary">
                <input
                  type="checkbox"
                  checked={confirmed}
                  onChange={(event) => onConfirmedChange(event.target.checked)}
                />
                <span>
                  I confirm this change against configuration revision{' '}
                  {review.plan.expectedRevisionId}.
                </span>
              </label>
              <MutationFeedback scope={scope} result={result} />
              <div className="flex justify-end gap-2">
                <Dialog.Close className={secondarySettingsButtonClass}>Cancel</Dialog.Close>
                {conflicted ? (
                  <button
                    type="button"
                    className={settingsButtonClass}
                    onClick={onResolveConflict}
                  >
                    Load current values
                  </button>
                ) : (
                  <button
                    type="button"
                    className={settingsButtonClass}
                    disabled={!confirmed || applying}
                    onClick={onApply}
                  >
                    {applying ? `Applying ${scope} settings` : `Apply ${scope} settings`}
                  </button>
                )}
              </div>
            </div>
          ) : null}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function MutationFeedback({
  scope,
  result,
}: {
  scope: SettingsMutationScope;
  result: SettingsMutationResult | undefined;
}) {
  if (!result || result.outcome === 'success') return null;
  if (result.outcome === 'conflict') {
    return (
      <p role="alert" className="text-xs text-state-conflicting">
        Another writer saved {scope} settings after this form loaded. Your draft was based on{' '}
        {result.expectedRevisionId}; the current authority is {result.actualRevisionId ?? 'unknown'}.
        Nothing was applied.
      </p>
    );
  }
  if (result.outcome === 'validation') {
    return <ValidationErrors errors={result.errors} />;
  }
  if (result.outcome === 'unavailable') {
    return (
      <p role="alert" className="text-xs text-state-unsupported-schema">
        {result.detail}
      </p>
    );
  }
  return (
    <p role="alert" className="text-xs text-state-error">
      {result.detail}
    </p>
  );
}

function ValidationErrors({ errors }: { errors: readonly SettingsValidationError[] }) {
  if (errors.length === 0) return null;
  return (
    <ul role="alert" className="mb-3 grid gap-1 text-xs text-state-error">
      {errors.map((error, index) => (
        <li key={`${error.field}:${index}`}>
          <span className="font-mono">{error.field}:</span>{' '}
          <span>{error.message}</span>
        </li>
      ))}
    </ul>
  );
}

function errorFor(errors: readonly SettingsValidationError[], field: string): string | undefined {
  return errors.find((error) => error.field === field)?.message;
}

function FieldError({ id, error }: { id?: string; error?: string }) {
  if (!error) return null;
  return (
    <p id={id} role="alert" className="mt-1 text-2xs text-state-error">
      {error}
    </p>
  );
}

function EditState({ scope, dirty }: { scope: SettingsMutationScope; dirty: boolean }) {
  return (
    <p
      aria-live="polite"
      className={cn(
        'mb-2 text-3xs font-semibold uppercase tracking-[0.16em]',
        dirty ? 'text-accent' : 'text-text-muted',
      )}
    >
      {dirty ? `Unsaved ${scope} changes` : `Current ${scope} values`}
    </p>
  );
}

/** A scope the envelope carries no apply action for.
 *
 * This states only what the wire states — the server does not currently
 * authorize this write — rather than guessing at which authority is missing. */
function ReadOnlyScope({ scope }: { scope: SettingsMutationScope }) {
  return (
    <p
      aria-live="polite"
      className="mb-2 text-3xs font-semibold uppercase tracking-[0.16em] text-state-unsupported-schema"
    >
      Read-only · this dashboard is not authorized to apply {scope} settings
    </p>
  );
}

function globLines(value: string): string[] {
  if (value.trim() === '') return [];
  return value.split(/\r?\n/).map((line) => line.trim());
}

/* ------------------------------------------------------------------ index --*/

function SectionIndex({
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

const ORIGIN_GLYPH: Readonly<Record<OriginKind, string>> = {
  file: 'F',
  environment: 'E',
  resolved: 'R',
};

const ORIGIN_WORD: Readonly<Record<OriginKind, string>> = {
  file: 'from file',
  environment: 'process environment',
  resolved: 'daemon-resolved',
};

/** Origin as an engraved initial. Decorative — every use sits beside the word. */
function OriginMark({ origin, className }: { origin: OriginKind; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        'td-value flex size-4 shrink-0 items-center justify-center border text-3xs',
        origin === 'resolved'
          ? 'border-edge-subtle text-text-muted'
          : 'border-edge-strong text-text-secondary',
        className,
      )}
    >
      {ORIGIN_GLYPH[origin]}
    </span>
  );
}

/* ------------------------------------------------------------- origin band --*/

/**
 * The provenance headline — deliberately a statement of ORIGIN, not of
 * precedence. Each card names the source the payload gives for that group and
 * how many values it carries. The band also states, in plain text, the thing
 * the payload cannot tell us, because a surface that quietly implies per-key
 * layer attribution would be lying.
 */
function OriginBand({
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

/* ---------------------------------------------------------------- section --*/

function ConfigSectionBlock({
  section,
  rows,
  overrides,
  query,
}: {
  section: ConfigSection;
  rows: ConfigRow[];
  overrides: readonly EnvOverride[];
  query: string;
}) {
  const headingId = `settings-${section.id}-heading`;
  return (
    <section data-section={section.id} aria-labelledby={headingId} className="min-w-0">
      <header className="sticky top-0 z-10 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 border-y border-edge-subtle bg-surface-2 px-3 py-1.5">
        <OriginMark origin={section.origin} />
        <h2 id={headingId} className="text-xs font-semibold tracking-tight">
          {section.title}
        </h2>
        <span className="td-legend">{ORIGIN_WORD[section.origin]}</span>
        {section.location ? (
          <span className="td-value min-w-0 truncate text-3xs">
            {section.locationKind === 'path' ? (
              <PathText value={section.location} />
            ) : (
              <span className="text-text-secondary">{section.location}</span>
            )}
          </span>
        ) : null}
        <span className="td-value ml-auto shrink-0 text-3xs text-text-muted">
          {countSettings(rows)}
        </span>
      </header>
      <div className="px-3 py-2">
        {overrides.length > 0 ? (
          <OverrideList overrides={overrides} query={query} />
        ) : null}
        <RowGroup rows={rows} query={query} start={0} depth={0} />
      </div>
    </section>
  );
}

/* --------------------------------------------------------------- overrides --*/

/**
 * The only genuine per-value provenance on the wire: for each variable the
 * daemon reports, whether it is set in the process environment (an override in
 * force, with its literal value) or unset (so a default applies). Active first,
 * because an override in force is the thing worth finding.
 *
 * The state is carried by the word "in force" / "unset" as much as by the lamp
 * — colour never states it alone.
 */
function OverrideList({
  overrides,
  query,
}: {
  overrides: readonly EnvOverride[];
  query: string;
}) {
  const ordered = useMemo(
    () => [...overrides].sort((a, b) => Number(b.active) - Number(a.active)),
    [overrides],
  );
  return (
    <div className="mb-3">
      <h3 className="mb-1 flex items-center gap-2.5 border-b border-edge-subtle pb-1">
        <span className="td-title">Overrides</span>
        <span aria-hidden className="td-rule" />
        <span className="td-value shrink-0 text-3xs text-text-muted">
          {ordered.filter((item) => item.active).length}/{ordered.length} in force
        </span>
      </h3>
      <ul className="flex flex-col">
        {ordered.map((item) => (
          <li
            key={item.name}
            className="grid grid-cols-1 gap-x-3 gap-y-0.5 border-b border-edge-subtle/60 py-1.5 last:border-b-0 md:grid-cols-[minmax(6rem,15rem)_minmax(0,1fr)]"
          >
            <div className="flex min-w-0 items-center gap-1.5">
              <Lamp tone={item.active ? 'bg-state-ready' : 'bg-surface-3'} />
              <span className="td-value min-w-0 break-all text-2xs text-text-primary">
                <Highlight text={item.name} query={query} />
              </span>
            </div>
            <div className="flex min-w-0 flex-col gap-0.5">
              <span className="flex min-w-0 flex-wrap items-baseline gap-x-2">
                <span
                  className={cn(
                    'td-legend',
                    item.active ? 'text-text-secondary' : 'text-text-muted',
                  )}
                >
                  {item.active ? 'in force' : 'unset · default applies'}
                </span>
                {item.value != null ? (
                  <span className="td-value min-w-0 break-all text-2xs text-text-primary">
                    <Highlight text={item.value} query={query} />
                  </span>
                ) : null}
              </span>
              {item.description ? (
                <span className="text-2xs leading-snug text-text-muted">
                  <Highlight text={item.description} query={query} />
                </span>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

/* -------------------------------------------------------------------- rows --*/

/**
 * Renders one nesting level: consecutive leaf rows share a `<dl>` so the key
 * column aligns, and each nested group opens its own titled block. (A heading
 * cannot live inside a `<dl>`, so groups break the list rather than nest in it.)
 *
 * Nesting is expressed as an indent under a heading rather than as a nested
 * label/value grid — the same lesson `KeyValueTree` learned the hard way: a
 * per-level label track compounds until the value column measures 0px. Here
 * exactly one label track is ever reserved, at any depth.
 */
function RowGroup({
  rows,
  query,
  start,
  depth,
}: {
  rows: ConfigRow[];
  query: string;
  start: number;
  depth: number;
}) {
  const blocks: ReactNode[] = [];
  let leaves: ConfigRow[] = [];
  const flushLeaves = (key: string) => {
    if (leaves.length === 0) return;
    const batch = leaves;
    leaves = [];
    blocks.push(
      <dl key={key} className="flex flex-col">
        {batch.map((row) => (
          <ValueRow key={row.id} row={row} query={query} />
        ))}
      </dl>,
    );
  };

  for (let index = start; index < rows.length; index += 1) {
    const row = rows[index]!;
    if (row.depth < depth) break;
    if (row.depth > depth) continue;
    if (row.kind === 'group') {
      flushLeaves(`leaves-${row.id}`);
      blocks.push(
        <div key={row.id} className="mt-2 first:mt-0">
          <h3 className="flex items-baseline gap-2 border-b border-edge-subtle pb-1">
            <span className="td-value text-2xs font-semibold text-text-secondary">
              <Highlight text={row.label} query={query} />
            </span>
            <span className="td-value text-3xs text-text-muted">
              {row.count} {row.count === 1 ? 'value' : 'values'}
            </span>
          </h3>
          <div className="border-l border-edge-subtle pl-2.5 pt-1">
            <RowGroup rows={rows} query={query} start={index + 1} depth={depth + 1} />
          </div>
        </div>,
      );
    } else {
      leaves.push(row);
    }
  }
  flushLeaves('leaves-tail');
  return <>{blocks}</>;
}

/** One key/value pair: aligned columns on wide viewports, stacked on narrow. */
function ValueRow({ row, query }: { row: ConfigRow; query: string }) {
  return (
    <div className="grid grid-cols-1 gap-x-4 gap-y-0.5 border-b border-edge-subtle/60 py-1 last:border-b-0 md:grid-cols-[minmax(6rem,13rem)_minmax(0,1fr)] md:items-baseline">
      <dt className="td-legend min-w-0 truncate normal-case tracking-normal" title={row.id}>
        <Highlight text={row.label} query={query} />
      </dt>
      <dd className="min-w-0">
        <ValueCell row={row} query={query} />
      </dd>
    </div>
  );
}

function ValueCell({ row, query }: { row: ConfigRow; query: string }) {
  if (row.kind === 'boolean') {
    const on = row.value === true;
    return (
      <span
        className={cn(
          'td-value inline-flex items-center gap-1.5 border px-1.5 py-px text-2xs',
          on ? 'border-edge-strong text-text-primary' : 'border-edge-subtle text-text-muted',
        )}
      >
        <Lamp tone={on ? 'bg-state-ready' : 'bg-surface-3'} />
        {on ? 'true' : 'false'}
      </span>
    );
  }
  if (row.kind === 'number') {
    return (
      <span className="td-value text-2xs text-text-primary" data-cell="numeric">
        {typeof row.value === 'number' ? row.value.toLocaleString() : row.text}
      </span>
    );
  }
  if (row.kind === 'null') {
    return <span className="td-value text-2xs text-text-muted">null</span>;
  }
  if (row.kind === 'path') {
    return (
      <span className="td-value block min-w-0 break-all text-2xs">
        <PathText value={String(row.value)} query={query} />
      </span>
    );
  }
  if (row.kind === 'list') {
    const items = Array.isArray(row.value) ? row.value : [];
    if (items.length === 0) {
      return <span className="text-2xs text-text-muted">{row.text}</span>;
    }
    return (
      <span className="flex flex-wrap gap-1">
        {items.map((item, index) => (
          <span
            key={`${String(item)}-${index}`}
            className="td-value border border-edge-subtle bg-surface-2 px-1.5 py-px text-2xs text-text-secondary"
          >
            <Highlight text={String(item)} query={query} />
          </span>
        ))}
      </span>
    );
  }
  return (
    <span className="td-value block min-w-0 break-words text-2xs text-text-primary">
      <Highlight text={row.text} query={query} />
    </span>
  );
}

/** A path reads from its tail: dim the directory, keep the last segment bright. */
function PathText({ value, query = '' }: { value: string; query?: string }) {
  const { head, tail } = splitPath(value);
  return (
    <>
      {head ? (
        <span className="text-text-muted">
          <Highlight text={head} query={query} />
        </span>
      ) : null}
      <span className="text-text-primary">
        <Highlight text={tail} query={query} />
      </span>
    </>
  );
}

/** Marks every occurrence of the active filter inside a literal. */
function Highlight({ text, query }: { text: string; query: string }) {
  const needle = query.trim().toLowerCase();
  if (needle === '') return <>{text}</>;
  const parts: ReactNode[] = [];
  const haystack = text.toLowerCase();
  let cursor = 0;
  let found = haystack.indexOf(needle, cursor);
  while (found >= 0) {
    if (found > cursor) parts.push(text.slice(cursor, found));
    parts.push(
      <mark
        key={`${found}`}
        className="bg-accent/25 px-px text-text-primary underline decoration-accent decoration-1 underline-offset-2"
      >
        {text.slice(found, found + needle.length)}
      </mark>,
    );
    cursor = found + needle.length;
    found = haystack.indexOf(needle, cursor);
  }
  if (cursor < text.length) parts.push(text.slice(cursor));
  return <>{parts}</>;
}

/* Form controls, not instrument chrome: these edit and commit configuration,
 * so they take the touch minimum on their own box rather than hiding a compact
 * bezel inside a larger hit area the way the panel-header controls do. `+2px`
 * on the input is its two hairlines, so the content box lands on 44. */
const settingsInputClass =
  'h-[calc(var(--touch-target-min)+2px)] w-full rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-0 px-2 text-xs text-text-primary outline-none focus-visible:border-accent';
const settingsButtonClass =
  'inline-flex min-h-[var(--touch-target-min)] items-center justify-center rounded-[var(--radius-standard)] border border-accent/50 bg-accent/15 px-3 text-2xs font-semibold text-text-primary hover:border-accent disabled:cursor-not-allowed disabled:opacity-50';
const secondarySettingsButtonClass =
  'inline-flex min-h-[var(--touch-target-min)] items-center justify-center rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 px-3 text-2xs font-medium text-text-secondary hover:text-text-primary';
