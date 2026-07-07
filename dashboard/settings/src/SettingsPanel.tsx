import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "../../lib/sdk";
import { EmptyState, ErrorPanel } from "../../lib/primitives";
import { api } from "./api";
import type {
  FieldErrors,
  ProjectSettingsPatch,
  SettingsPayload,
  UserSettingsPatch,
} from "./types";

interface ErrorWithBody {
  body?: {
    validation_errors?: Array<{ field?: unknown; message?: unknown }>;
  };
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function fieldErrorsFrom(err: unknown): FieldErrors {
  const errors = (err as ErrorWithBody)?.body?.validation_errors;
  if (!Array.isArray(errors)) return {};
  return Object.fromEntries(
    errors
      .filter((e) => typeof e.field === "string" && typeof e.message === "string")
      .map((e) => [e.field as string, e.message as string]),
  );
}

function globsToText(globs: string[]): string {
  return globs.join("\n");
}

function textToGlobs(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

export interface ProjectDraft {
  includeText: string;
  excludeText: string;
  maxFileSize: string;
  extractDocstrings: boolean;
  trackCallSites: boolean;
  gitIgnore: boolean;
  telemetryTimings: boolean;
}

export interface UserDraft {
  uploadEnabled: boolean;
  watcherDebounce: string;
  extractionTimeoutSecs: string;
}

function projectDraftFrom(settings: SettingsPayload): ProjectDraft {
  const config = settings.project.config;
  return {
    includeText: globsToText(config.include),
    excludeText: globsToText(config.exclude),
    maxFileSize: String(config.max_file_size),
    extractDocstrings: config.extract_docstrings,
    trackCallSites: config.track_call_sites,
    gitIgnore: config.git_ignore,
    telemetryTimings: config.telemetry.timings,
  };
}

function userDraftFrom(settings: SettingsPayload): UserDraft {
  return {
    uploadEnabled: settings.user.upload_enabled,
    watcherDebounce: settings.user.watcher_debounce,
    extractionTimeoutSecs: String(settings.user.extraction_timeout_secs),
  };
}

function sameStringList(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((value, index) => value === b[index]);
}

export function projectPatchFrom(
  draft: ProjectDraft,
  saved: ProjectDraft | null,
): ProjectSettingsPatch {
  const patch: ProjectSettingsPatch = {};
  const include = textToGlobs(draft.includeText);
  const exclude = textToGlobs(draft.excludeText);
  const maxFileSize = Number(draft.maxFileSize);

  if (!saved || !sameStringList(include, textToGlobs(saved.includeText))) {
    patch.include = include;
  }
  if (!saved || !sameStringList(exclude, textToGlobs(saved.excludeText))) {
    patch.exclude = exclude;
  }
  if (!saved || maxFileSize !== Number(saved.maxFileSize)) {
    patch.max_file_size = maxFileSize;
  }
  if (!saved || draft.extractDocstrings !== saved.extractDocstrings) {
    patch.extract_docstrings = draft.extractDocstrings;
  }
  if (!saved || draft.trackCallSites !== saved.trackCallSites) {
    patch.track_call_sites = draft.trackCallSites;
  }
  if (!saved || draft.gitIgnore !== saved.gitIgnore) {
    patch.git_ignore = draft.gitIgnore;
  }
  if (!saved || draft.telemetryTimings !== saved.telemetryTimings) {
    patch.telemetry = { timings: draft.telemetryTimings };
  }

  return patch;
}

export function userPatchFrom(draft: UserDraft, saved: UserDraft | null): UserSettingsPatch {
  const patch: UserSettingsPatch = {};
  const watcherDebounce = draft.watcherDebounce.trim();
  const extractionTimeoutSecs = Number(draft.extractionTimeoutSecs);

  if (!saved || draft.uploadEnabled !== saved.uploadEnabled) {
    patch.upload_enabled = draft.uploadEnabled;
  }
  if (!saved || watcherDebounce !== saved.watcherDebounce.trim()) {
    patch.watcher_debounce = watcherDebounce;
  }
  if (!saved || extractionTimeoutSecs !== Number(saved.extractionTimeoutSecs)) {
    patch.extraction_timeout_secs = extractionTimeoutSecs;
  }

  return patch;
}

function sameDraft<T>(a: T | null, b: T | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export default function SettingsPanel() {
  const [settings, setSettings] = useState<SettingsPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadErrorText, setLoadErrorText] = useState("");

  const [projectDraft, setProjectDraft] = useState<ProjectDraft | null>(null);
  const [projectSaved, setProjectSaved] = useState<ProjectDraft | null>(null);
  const [projectSaving, setProjectSaving] = useState(false);
  const [projectError, setProjectError] = useState("");
  const [projectFieldErrors, setProjectFieldErrors] = useState<FieldErrors>({});
  const [resyncRecommended, setResyncRecommended] = useState(false);

  const [userDraft, setUserDraft] = useState<UserDraft | null>(null);
  const [userSaved, setUserSaved] = useState<UserDraft | null>(null);
  const [userSaving, setUserSaving] = useState(false);
  const [userError, setUserError] = useState("");
  const [userFieldErrors, setUserFieldErrors] = useState<FieldErrors>({});
  const [restartRecommended, setRestartRecommended] = useState(false);

  const applySettings = useCallback((payload: SettingsPayload) => {
    setSettings(payload);
    const project = projectDraftFrom(payload);
    const user = userDraftFrom(payload);
    setProjectDraft(project);
    setProjectSaved(project);
    setUserDraft(user);
    setUserSaved(user);
    setProjectFieldErrors({});
    setUserFieldErrors({});
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setLoadErrorText("");
    try {
      applySettings(await api.getSettings());
    } catch (err) {
      setLoadErrorText(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [applySettings]);

  useEffect(() => {
    load();
  }, [load]);

  const projectDirty = useMemo(
    () => !sameDraft(projectDraft, projectSaved),
    [projectDraft, projectSaved],
  );
  const userDirty = useMemo(() => !sameDraft(userDraft, userSaved), [userDraft, userSaved]);

  const updateProject = useCallback((patch: Partial<ProjectDraft>) => {
    setProjectDraft((current) => (current ? { ...current, ...patch } : current));
  }, []);

  const updateUser = useCallback((patch: Partial<UserDraft>) => {
    setUserDraft((current) => (current ? { ...current, ...patch } : current));
  }, []);

  const saveProject = useCallback(async () => {
    if (!projectDraft) return;
    setProjectSaving(true);
    setProjectError("");
    setProjectFieldErrors({});
    try {
      const payload = await api.patchProject(projectPatchFrom(projectDraft, projectSaved));
      applySettings(payload);
      setResyncRecommended(Boolean(payload.resync_recommended));
    } catch (err) {
      setProjectError(errorMessage(err));
      setProjectFieldErrors(fieldErrorsFrom(err));
    } finally {
      setProjectSaving(false);
    }
  }, [applySettings, projectDraft, projectSaved]);

  const saveUser = useCallback(async () => {
    if (!userDraft) return;
    setUserSaving(true);
    setUserError("");
    setUserFieldErrors({});
    try {
      const payload = await api.patchUser(userPatchFrom(userDraft, userSaved));
      applySettings(payload);
      setRestartRecommended(Boolean(payload.restart_recommended));
    } catch (err) {
      setUserError(errorMessage(err));
      setUserFieldErrors(fieldErrorsFrom(err));
    } finally {
      setUserSaving(false);
    }
  }, [applySettings, userDraft, userSaved]);

  const discardProject = useCallback(() => {
    setProjectDraft(projectSaved);
    setProjectError("");
    setProjectFieldErrors({});
  }, [projectSaved]);

  const discardUser = useCallback(() => {
    setUserDraft(userSaved);
    setUserError("");
    setUserFieldErrors({});
  }, [userSaved]);

  if (loading && !settings) {
    return <EmptyState>Loading settings...</EmptyState>;
  }
  if (loadErrorText && !settings) {
    return <ErrorPanel error={loadErrorText} onRetry={load} />;
  }
  if (!settings || !projectDraft || !userDraft) {
    return <EmptyState>No settings available.</EmptyState>;
  }

  return (
    <div className="tdst-root">
      {loadErrorText && <ErrorPanel error={loadErrorText} onRetry={load} />}

      {resyncRecommended && (
        <div className="tdst-banner tdst-banner-warn" role="status">
          <span>
            Indexing settings changed — a re-sync is required before the graph reflects them.
            Run <code>tracedecay sync</code> (or wait for the next watcher pass).
          </span>
          <Button size="sm" onClick={() => setResyncRecommended(false)}>
            Dismiss
          </Button>
        </div>
      )}

      {restartRecommended && (
        <div className="tdst-banner tdst-banner-warn" role="status">
          <span>
            Saved. Watcher/extraction settings are read at startup — restart the tracedecay
            daemon (or your editor&apos;s MCP session) for them to take effect.
          </span>
          <Button size="sm" onClick={() => setRestartRecommended(false)}>
            Dismiss
          </Button>
        </div>
      )}

      <div className="tdst-grid">
        <Card>
          <CardHeader>
            <CardTitle>Indexing</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="tdst-section-hint">
              Project config: <code>{settings.project.config_path}</code>
            </p>
            {projectError && <p className="tdst-error">{projectError}</p>}

            <label className="tdst-field">
              <span className="tdst-field-label">Include globs (one per line)</span>
              <textarea
                rows={4}
                value={projectDraft.includeText}
                onChange={(event) => updateProject({ includeText: event.currentTarget.value })}
                disabled={projectSaving}
                aria-label="Include globs"
              />
              <span className="tdst-field-hint">
                Paths indexed despite hidden-directory or gitignore filters, e.g.{" "}
                <code>.github/**</code>
              </span>
              {projectFieldErrors.include && (
                <span className="tdst-field-error">{projectFieldErrors.include}</span>
              )}
            </label>

            <label className="tdst-field">
              <span className="tdst-field-label">Exclude globs (one per line)</span>
              <textarea
                rows={8}
                value={projectDraft.excludeText}
                onChange={(event) => updateProject({ excludeText: event.currentTarget.value })}
                disabled={projectSaving}
                aria-label="Exclude globs"
              />
              {projectFieldErrors.exclude && (
                <span className="tdst-field-error">{projectFieldErrors.exclude}</span>
              )}
            </label>

            <label className="tdst-field tdst-field-inline">
              <span className="tdst-field-label">Max file size (bytes)</span>
              <input
                type="number"
                min={1}
                value={projectDraft.maxFileSize}
                onChange={(event) => updateProject({ maxFileSize: event.currentTarget.value })}
                disabled={projectSaving}
                aria-label="Max file size"
              />
              {projectFieldErrors.max_file_size && (
                <span className="tdst-field-error">{projectFieldErrors.max_file_size}</span>
              )}
            </label>

            <label className="tdst-toggle">
              <input
                type="checkbox"
                checked={projectDraft.extractDocstrings}
                onChange={(event) =>
                  updateProject({ extractDocstrings: event.currentTarget.checked })
                }
                disabled={projectSaving}
              />
              <span>Extract docstrings</span>
            </label>

            <label className="tdst-toggle">
              <input
                type="checkbox"
                checked={projectDraft.trackCallSites}
                onChange={(event) => updateProject({ trackCallSites: event.currentTarget.checked })}
                disabled={projectSaving}
              />
              <span>Track call sites</span>
            </label>

            <label className="tdst-toggle">
              <input
                type="checkbox"
                checked={projectDraft.gitIgnore}
                onChange={(event) => updateProject({ gitIgnore: event.currentTarget.checked })}
                disabled={projectSaving}
              />
              <span>Respect .gitignore during indexing</span>
            </label>

            <label className="tdst-toggle">
              <input
                type="checkbox"
                checked={projectDraft.telemetryTimings}
                onChange={(event) =>
                  updateProject({ telemetryTimings: event.currentTarget.checked })
                }
                disabled={projectSaving}
                aria-label="Capture timing telemetry"
              />
              <span>Capture timing telemetry</span>
            </label>

            <p className="tdst-section-hint">
              Changing these requires a re-sync before the code graph reflects them.
            </p>

            <div className="tdst-actions">
              <Button onClick={saveProject} disabled={!projectDirty || projectSaving}>
                {projectSaving ? "Saving..." : "Save"}
              </Button>
              <Button onClick={discardProject} disabled={!projectDirty || projectSaving}>
                Discard
              </Button>
              {projectDirty && <Badge>Unsaved changes</Badge>}
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>User &amp; Uploads</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="tdst-section-hint">
              User config: <code>{settings.user.config_path}</code>
            </p>
            {userError && <p className="tdst-error">{userError}</p>}

            <label className="tdst-toggle">
              <input
                type="checkbox"
                checked={userDraft.uploadEnabled}
                onChange={(event) => updateUser({ uploadEnabled: event.currentTarget.checked })}
                disabled={userSaving}
              />
              <span>Upload token counts to the worldwide counter</span>
            </label>

            <label className="tdst-field tdst-field-inline">
              <span className="tdst-field-label">Watcher debounce</span>
              <input
                value={userDraft.watcherDebounce}
                onChange={(event) => updateUser({ watcherDebounce: event.currentTarget.value })}
                disabled={userSaving}
                placeholder="2s"
                aria-label="Watcher debounce"
              />
              {userFieldErrors.watcher_debounce && (
                <span className="tdst-field-error">{userFieldErrors.watcher_debounce}</span>
              )}
            </label>

            <label className="tdst-field tdst-field-inline">
              <span className="tdst-field-label">Extraction timeout (seconds)</span>
              <input
                type="number"
                min={1}
                value={userDraft.extractionTimeoutSecs}
                onChange={(event) =>
                  updateUser({ extractionTimeoutSecs: event.currentTarget.value })
                }
                disabled={userSaving}
                aria-label="Extraction timeout"
              />
              {userFieldErrors.extraction_timeout_secs && (
                <span className="tdst-field-error">
                  {userFieldErrors.extraction_timeout_secs}
                </span>
              )}
            </label>

            <p className="tdst-section-hint">
              Watcher debounce and extraction timeout take effect after a daemon restart.
            </p>

            <div className="tdst-actions">
              <Button onClick={saveUser} disabled={!userDirty || userSaving}>
                {userSaving ? "Saving..." : "Save"}
              </Button>
              <Button onClick={discardUser} disabled={!userDirty || userSaving}>
                Discard
              </Button>
              {userDirty && <Badge>Unsaved changes</Badge>}
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Automation</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="tdst-kv">
              <span className="tdst-kv-key">Enabled</span>
              <span>{settings.automation.enabled ? "yes" : "no"}</span>
              <span className="tdst-kv-key">Backend</span>
              <span>{settings.automation.backend}</span>
              <span className="tdst-kv-key">Host mode</span>
              <span>{settings.automation.host_mode}</span>
            </div>
            <p className="tdst-section-hint">
              Self-improvement automation has a dedicated editor in the Holographic Memory tab
              (Curation &rarr; Automation config).
            </p>
            <div className="tdst-actions">
              <a className="tdst-link-button" href="?tab=holographic">
                Open automation config
              </a>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Storage (read-only)</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="tdst-kv tdst-kv-paths">
              <span className="tdst-kv-key">Project root</span>
              <code>{settings.storage.project_root}</code>
              <span className="tdst-kv-key">Storage mode</span>
              <code>{settings.storage.storage_mode}</code>
              <span className="tdst-kv-key">Store root</span>
              <code>{settings.storage.store_root}</code>
              <span className="tdst-kv-key">Graph DB</span>
              <code>{settings.storage.graph_db}</code>
              <span className="tdst-kv-key">Memory DB</span>
              <code>{settings.storage.memory_db}</code>
              <span className="tdst-kv-key">LCM DB ({settings.storage.lcm_scope})</span>
              <code>{settings.storage.lcm_db}</code>
              <span className="tdst-kv-key">Savings DB</span>
              <code>{settings.storage.savings_db}</code>
            </div>
            <p className="tdst-section-hint">
              <code>.tracedecay/</code> marker dir is{" "}
              {settings.project.tracedecay_dir_gitignored ? "" : "NOT "}
              covered by .gitignore. Manage with <code>tracedecay gitignore</code>.
            </p>
          </CardContent>
        </Card>

        <Card className="tdst-env-card">
          <CardHeader>
            <CardTitle>Environment (read-only)</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="tdst-kv">
              <span className="tdst-kv-key">Version</span>
              <span>
                {settings.version.version} ({settings.version.channel})
                {settings.version.cached_latest_version &&
                  settings.version.cached_latest_version !== settings.version.version && (
                    <Badge className="tdst-update-badge">
                      latest: {settings.version.cached_latest_version}
                    </Badge>
                  )}
              </span>
              <span className="tdst-kv-key">Global accounting</span>
              <span>
                {settings.environment.global_accounting_enabled ? "enabled" : "disabled"} (
                {settings.environment.global_accounting_mode})
              </span>
              <span className="tdst-kv-key">Pricing</span>
              <span>{settings.environment.pricing_offline ? "offline" : "online"}</span>
            </div>
            <p className="tdst-section-hint">
              Environment variables are read at process startup and cannot be edited here. Switch
              update channels with <code>tracedecay channel</code>.
            </p>
            <div className="tdst-env-list">
              {settings.environment.variables.map((variable) => (
                <div key={variable.name} className="tdst-env-row">
                  <div className="tdst-env-head">
                    <code>{variable.name}</code>
                    {variable.active ? (
                      <Badge className="tdst-env-set">set{variable.value ? `: ${variable.value}` : ""}</Badge>
                    ) : (
                      <Badge className="tdst-env-unset">unset</Badge>
                    )}
                  </div>
                  <small>{variable.description}</small>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
