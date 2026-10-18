import { describe, expect, it } from 'vitest';
import type { DashboardScope } from '../../data/scope/store.ts';
import { sessionHref } from './SessionPivots.tsx';

const ALL: DashboardScope = { kind: 'all' };
const PROJECT: DashboardScope = { kind: 'project', projectId: 'p1', label: 'P 1', activation: 'active' };

function read(href: string) {
  const url = new URL(href, 'http://dashboard.test');
  return { pathname: url.pathname, params: Object.fromEntries(url.searchParams) };
}

describe('sessionHref', () => {
  it('names a session the way each workspace reads one', () => {
    const session = { provider: 'claude', sessionId: 'abc' };
    expect(read(sessionHref('sessions', session, ALL))).toEqual({
      pathname: '/sessions',
      params: { sessionId: 'abc', sessionProvider: 'claude' },
    });
    expect(read(sessionHref('agents', session, ALL))).toEqual({
      pathname: '/agents',
      params: { sessionId: 'abc', sessionProvider: 'claude' },
    });
    expect(read(sessionHref('loom', session, ALL))).toEqual({
      pathname: '/loom',
      params: { loomSession: '["claude","abc"]' },
    });
  });

  it('carries the project scope and nothing else from the workspace it leaves', () => {
    expect(read(sessionHref('agents', { provider: 'codex', sessionId: 'abc' }, PROJECT))).toEqual({
      pathname: '/agents',
      params: { scope: 'p1', scopeLabel: 'P 1', sessionId: 'abc', sessionProvider: 'codex' },
    });
    expect(read(sessionHref('loom', { provider: 'codex', sessionId: 'abc' }, PROJECT)).params).toEqual({
      scope: 'p1',
      scopeLabel: 'P 1',
      loomSession: '["codex","abc"]',
    });
  });

  it('links an unrecorded provider to the lane Loom draws and to an id-only selection elsewhere', () => {
    const session = { provider: '', sessionId: 'abc' };
    expect(read(sessionHref('loom', session, ALL)).params).toEqual({ loomSession: '["unknown","abc"]' });
    expect(read(sessionHref('sessions', session, ALL)).params).toEqual({ sessionId: 'abc' });
  });

  it('links a provider-less id to Loom by the bare id Delivery has always sent', () => {
    expect(sessionHref('loom', { provider: null, sessionId: 'session.alpha.1' }, ALL)).toBe(
      '/loom?loomSession=session.alpha.1',
    );
    expect(read(sessionHref('agents', { provider: null, sessionId: 'abc' }, ALL)).params).toEqual({
      sessionId: 'abc',
    });
  });
});
