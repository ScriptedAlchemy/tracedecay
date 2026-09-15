import type { PackLoomEvent } from '../data/pack';
import { BOUNDS } from './journey';
import { SOURCE, SOURCE_ID, type JourneySession } from './source';

export const PIVOT_SURFACES = ['sessions', 'agents', 'work', 'code', 'delivery'] as const;
export type PivotSurface = typeof PIVOT_SURFACES[number];
const pivotKeys = ['loom_pivot', 'loom_target_session', 'loom_target_event', 'loom_return'];
export function isPivotSurface(surface: string): surface is PivotSurface {
  return PIVOT_SURFACES.some(value => value === surface);
}

/** Destination selection is separate from the saved Loom viewport and focus. */
export function loomPivotUrl(surface: PivotSurface, sessionId: string, eventId?: string) {
  const url = new URL(location.href);
  if (!url.searchParams.has('loom_return')) {
    const origin = new URL(url);
    pivotKeys.forEach(key => origin.searchParams.delete(key));
    origin.searchParams.set('surface', 'loom');
    url.searchParams.set('loom_return', origin.pathname + origin.search);
  }
  url.searchParams.set('surface', surface); url.searchParams.delete('state');
  url.searchParams.set('loom_pivot', '1');
  url.searchParams.set('loom_source', SOURCE_ID);
  if (SOURCE.page) url.searchParams.set('loom_page', SOURCE.page);
  url.searchParams.set('loom_target_session', sessionId);
  if (eventId) url.searchParams.set('loom_target_event', eventId);
  else url.searchParams.delete('loom_target_event');
  return url.pathname + url.search;
}

export type LoomPivot = {
  kind: 'ready'; surface: PivotSurface; session: JourneySession;
  event: PackLoomEvent | null; cutoff: number | null; returnUrl: string;
} | {kind:'unavailable'; reason:string; returnUrl:string};

export function resolveLoomPivot(surface: string): LoomPivot | null {
  const current = new URL(location.href), params = current.searchParams;
  if (!isPivotSurface(surface) || !params.has('loom_pivot')) return null;
  const fallback = new URL(current);
  pivotKeys.forEach(key => fallback.searchParams.delete(key));
  fallback.searchParams.set('surface', 'loom'); fallback.searchParams.set('loom_source', SOURCE_ID);
  if (SOURCE.page) fallback.searchParams.set('loom_page', SOURCE.page);
  let returnUrl = fallback.pathname + fallback.search;
  const saved = params.get('loom_return');
  if (saved) {
    try {
      const origin = new URL(saved, current.origin);
      if (origin.origin === current.origin && origin.pathname === current.pathname && origin.searchParams.get('surface') === 'loom' && (origin.searchParams.get('loom_source') ?? 'mac') === SOURCE_ID) {
        pivotKeys.forEach(key => origin.searchParams.delete(key));
        returnUrl = origin.pathname + origin.search;
      }
    } catch { /* Invalid return addresses use the local Loom route above. */ }
  }
  const unavailable = (reason: string): LoomPivot => ({kind:'unavailable', reason, returnUrl});
  if (params.get('loom_pivot') !== '1' || (params.get('loom_source') ?? 'mac') !== SOURCE_ID) return unavailable('The requested source is not loaded. No other profile has been substituted.');
  if (SOURCE.design && params.get('loom_page') !== SOURCE.page) return unavailable('The requested design page is not loaded. Return to Loom to choose its loaded page.');
  let cutoff: number | null = null;
  if (params.get('loom_replay') === '1') {
    const value = params.get('loom_time'); cutoff = value === null || value.trim() === '' ? NaN : Number(value);
    if (!Number.isFinite(cutoff) || cutoff < BOUNDS[0] || cutoff > BOUNDS[1]) return unavailable('The replay cursor is outside the loaded source.');
  }
  const session = SOURCE.sessions.find(row => row.id === params.get('loom_target_session'));
  if (!session) return unavailable('This session is not present in the selected source page.');
  if (cutoff !== null && (session.startedTs === null || session.startedTs > cutoff)) return unavailable('This session is not revealed at the retained replay cursor.');
  const eventId = params.get('loom_target_event');
  const event = eventId ? SOURCE.events.find(row => row.id === eventId) : null;
  if (eventId && !event) return unavailable('This event is not present in the selected source page.');
  if (event && event.sessionId !== session.id) return unavailable('The requested event does not belong to this session.');
  if (event && cutoff !== null && (event.ts === null || event.ts > cutoff)) return unavailable('This event is not revealed at the retained replay cursor.');
  return {kind:'ready', surface, session, event:event ?? null, cutoff, returnUrl};
}
