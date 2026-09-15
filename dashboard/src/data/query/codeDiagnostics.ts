/**
 * The Code Diagnostics broker: its snapshot, and the four controls over it.
 *
 * `code_diagnostics_api.rs` mounts a read and three writes on one resource, and
 * every one of the four answers with the SAME body — the broker's snapshot plus
 * the compare-and-set token for the settings it just reported. That is what
 * makes an honest control possible here, exactly as it does for the automation
 * scheduler: a route replying `{"ok":true}` would leave this module to assume
 * the new engine state and paint it on faith. Because each control returns the
 * reading the server took after applying it, the control seeds the cache with
 * the server's own answer and the panel never shows a refresh it has not
 * observed.
 *
 * So there is deliberately no optimistic update below. A refresh in particular
 * must never be optimistic: the whole quantity a reader takes from this surface
 * is whether the diagnostics on screen were produced by an analyzer that has
 * actually run, and a client-side "refreshing…" that outlived a request the
 * daemon dropped would be precisely a state asserted rather than measured.
 *
 * The settings write is compare-and-set. `expected_revision` is required by the
 * route — without it the broker cannot tell an edit of the settings this
 * browser read from one that would silently overwrite a writer it never saw —
 * so every mutation that touches settings carries the revision from the reading
 * it was issued against, not one re-read at dispatch time.
 */
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { z } from 'zod';

import { fetchPayloadWrite, type PayloadWriteResult } from './payload.ts';
import { usePayload } from './usePayload.ts';
import {
  scopeKey,
  scopeWritable,
  scopedQueryKey,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from '../scope/store.ts';

export const codeDiagnosticsKey = ['code-diagnostics'] as const;
export const codeDiagnosticsUrl = '/api/plugins/code-diagnostics';

const SEVERITIES = ['error', 'warning', 'information', 'hint'] as const;

const DiagnosticRow = z
  .object({
    language: z.string(),
    source: z.string(),
    file: z.string(),
    line_start: z.number(),
    severity: z.enum(SEVERITIES),
    code: z.string().nullable(),
    message: z.string(),
    enclosing_node: z.string().nullable(),
  })
  .passthrough();

/** `EngineStatus` (`analyzer/broker.rs`). `state` is the broker's own word for
 * what the engine is; this module never derives it. */
const EngineRow = z
  .object({
    language: z.string(),
    command: z.string(),
    enabled: z.boolean(),
    state: z.enum([
      'unavailable',
      'disabled',
      'inactive',
      'available',
      'ready',
      'refreshing',
      'crashed',
    ]),
    last_error: z.string().nullable(),
  })
  .passthrough();

/** `LanguageDiagnosticsSettings` (`analyzer/settings.rs`): both fields carry
 * `#[serde(default)]`, and a language the operator has never configured has no
 * entry at all — so an absent map entry means "the built-in default", which is
 * a different fact from an entry that says `enabled: false`. */
const LanguageSettings = z
  .object({
    enabled: z.boolean(),
    command_override: z.string().nullable(),
  })
  .partial()
  .passthrough();

/** `CodeDiagnosticsSettings`. Present on every response the route serves,
 * including the ones where `settings_unavailable` says the values are the
 * built-in defaults rather than the operator's file. */
const Settings = z
  .object({
    idle_backfill: z.enum(['off', 'idle']),
    languages: z.record(z.string(), LanguageSettings),
  })
  .passthrough();

const SnapshotSchema = z
  .object({
    summary: z
      .object({
        total_errors: z.number(),
        total_warnings: z.number(),
        pending_refreshes: z.number(),
        last_refresh_age_seconds: z.number().nullable(),
      })
      .passthrough(),
    engines: z.array(EngineRow),
    diagnostics: z.array(DiagnosticRow),
    settings: Settings,
    /**
     * The compare-and-set token for the settings in the same body.
     *
     * Required rather than optional: `snapshot_response` stamps it on every
     * read and every write, unconditionally, so a body without one did not come
     * from this route and must reach `unsupported_schema` instead of quietly
     * producing a settings control with no revision to send.
     */
    settings_revision: z.string(),
    settings_unavailable: z.object({ reason: z.string() }).passthrough().optional(),
  })
  .passthrough();

export type DiagnosticsSnapshot = z.infer<typeof SnapshotSchema>;
export type EngineStatus = z.infer<typeof EngineRow>;
export type Diagnostic = z.infer<typeof DiagnosticRow>;
export type IdleBackfillMode = DiagnosticsSnapshot['settings']['idle_backfill'];

export function useCodeDiagnostics() {
  return usePayload(codeDiagnosticsKey, codeDiagnosticsUrl, SnapshotSchema, {
    refetchInterval: 30_000,
  });
}

/**
 * One control the reader can ask for, named by what it does to the broker.
 *
 * The two settings commands carry the revision of the reading they were issued
 * against. Held on the command rather than read inside `mutationFn` because the
 * two moments can disagree: the 30-second poll can land a newer snapshot
 * between the click and the dispatch, and a write that silently adopted THAT
 * revision would be a compare-and-set against a state the operator never saw —
 * which is the exact overwrite `expected_revision` exists to prevent.
 */
export type DiagnosticsCommand =
  | { kind: 'refresh_all' }
  | { kind: 'refresh_language'; language: string }
  | { kind: 'set_language_enabled'; language: string; enabled: boolean; revision: string }
  | { kind: 'set_idle_backfill'; mode: IdleBackfillMode; revision: string };

/** Whether two commands address the same control, so a row can show its own
 * in-flight state and no other row borrows it. */
export function sameCommand(a: DiagnosticsCommand, b: DiagnosticsCommand): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === 'refresh_language' && b.kind === 'refresh_language') {
    return a.language === b.language;
  }
  if (a.kind === 'set_language_enabled' && b.kind === 'set_language_enabled') {
    return a.language === b.language;
  }
  return true;
}

function request(command: DiagnosticsCommand, base: string): [string, RequestInit] {
  switch (command.kind) {
    case 'refresh_all':
      return [`${base}/refresh`, { method: 'POST' }];
    case 'refresh_language':
      return [
        `${base}/refresh/${encodeURIComponent(command.language)}`,
        { method: 'POST' },
      ];
    case 'set_language_enabled':
      return [
        base,
        patch({
          expected_revision: command.revision,
          languages: { [command.language]: { enabled: command.enabled } },
        }),
      ];
    case 'set_idle_backfill':
      return [
        base,
        patch({ expected_revision: command.revision, idle_backfill: command.mode }),
      ];
    default: {
      const exhaustive: never = command;
      return exhaustive;
    }
  }
}

/** A settings patch sends only the keys it changes: `SettingsPatch` fields are
 * `#[serde(default)]`, and `languages` merges per language, so an omitted key
 * is "leave this alone" rather than "clear it". Sending the whole settings
 * object back would make every toggle a full overwrite of a document this panel
 * does not fully render. */
function patch(body: Record<string, unknown>): RequestInit {
  return {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
}

/** What a control attempt produced, including the case where there was no
 * attempt — kept apart for the same reason the scheduler control keeps it:
 * nothing was sent, so nothing changed, and the panel must not imply the broker
 * was asked and refused. */
export type DiagnosticsControlResult =
  | PayloadWriteResult<DiagnosticsSnapshot>
  | { outcome: 'not_dispatched'; writability: ScopeWritability };

/** The scope a control attempt was issued under, captured when it was issued.
 * See `useSchedulerControl` — settlement callbacks run from the CURRENT
 * options, so a refresh dispatched against project A that is still in flight
 * when the reader switches to project B would otherwise settle into B's entry. */
interface DiagnosticsDispatch {
  readonly snapshotKey: readonly unknown[];
}

/**
 * The broker's controls as one mutation.
 *
 * One mutation rather than four because all four write the same cache entry
 * with the same body, and because a reader may only ever have one control in
 * flight per panel — the commands are not independent (a refresh and a settings
 * write both re-derive the snapshot), so serialising them through a single
 * mutation is the truthful model rather than a convenience.
 */
export function useDiagnosticsControl() {
  const scope = useScope((s) => s.scope);
  const client = useQueryClient();
  const snapshotKey = scopedQueryKey(scope, codeDiagnosticsKey, codeDiagnosticsUrl);
  const writability = scopeWritable(scope);
  const mutation = useMutation<
    DiagnosticsControlResult,
    Error,
    DiagnosticsCommand,
    DiagnosticsDispatch
  >({
    mutationKey: [...codeDiagnosticsKey, scopeKey(scope)],
    onMutate: () => ({ snapshotKey }),
    mutationFn: async (command: DiagnosticsCommand) => {
      // Nothing leaves the browser unless the scope is known to accept it. The
      // buttons are disabled on this same reading, so arriving here means the
      // disable was bypassed — and dispatching anyway would trade a stated
      // reason for a 405 this layer cannot tell from a route that has gone.
      if (writability.state !== 'writable') {
        return { outcome: 'not_dispatched', writability };
      }
      const [url, init] = request(command, scopedUrl(scope, codeDiagnosticsUrl));
      return fetchPayloadWrite(url, SnapshotSchema, init);
    },
    onSuccess: (result, _command, dispatch) => {
      const target = dispatch.snapshotKey;
      // Only a genuine reading may replace the cached one; a transport failure
      // must leave the last real snapshot on screen, reported beside it.
      if (result.outcome === 'ok') {
        client.setQueryData(target, result);
        return;
      }
      if (result.outcome === 'not_dispatched') return;
      void client.invalidateQueries({ queryKey: target });
    },
  });
  return { ...mutation, writability };
}

/**
 * What a failed control is reported as, in this surface's words.
 *
 * The revision conflict is singled out because it is the one failure that is
 * not a fault: the broker refused a compare-and-set whose expected revision no
 * longer held, which means someone else wrote the analyzer settings between
 * this browser's read and its click. The remedy is "read again", not "retry",
 * and the two must not share a sentence.
 *
 * It is recognised by the detail the payload ladder itself produced for the
 * route's 409, because that ladder deliberately carries no `conflict` outcome:
 * adding one would oblige every read consumer in the dashboard to handle a
 * state a GET cannot reach. The comparison is against the ladder's own token,
 * not against a server string.
 */
export function controlFailure(result: DiagnosticsControlResult): string | null {
  switch (result.outcome) {
    case 'ok':
      return null;
    case 'not_dispatched':
      return result.writability.state === 'writable'
        ? null
        : `not sent: ${result.writability.reason}`;
    case 'read_only_scope':
      return result.refusal.detail;
    case 'error':
      return result.detail === 'HTTP 409'
        ? 'the analyzer settings changed since this reading, so the edit was refused rather than applied over a change nobody here saw; the panel has re-read them'
        : `the broker did not apply this control (${result.detail})`;
    case 'offline':
      return 'the daemon is not reachable, so nothing was applied';
    case 'unauthorized':
      return 'the daemon accepted no identity for this control';
    case 'denied':
      return 'the daemon does not permit this identity to control diagnostics';
    case 'unavailable':
      return result.reason ?? result.status;
    case 'unsupported_schema':
      return 'the broker answered with a shape this build does not understand';
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}
