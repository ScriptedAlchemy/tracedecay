import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const TOKEN = 'ab'.repeat(32);
const BASIC = `Basic ${btoa(`tracedecay:${TOKEN}`)}`;

async function freshCredential() {
  vi.resetModules();
  return import('./credential.ts');
}

function sentHeaders(fetchMock: ReturnType<typeof vi.fn>): Headers {
  const init = fetchMock.mock.calls.at(-1)?.[1] as RequestInit | undefined;
  return new Headers(init?.headers);
}

describe('dashboard credential', () => {
  const fetchMock = vi.fn(async () => new Response('{}'));

  beforeEach(() => {
    sessionStorage.clear();
    window.history.replaceState(null, '', '/');
    fetchMock.mockClear();
    vi.stubGlobal('fetch', fetchMock);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('moves the launch token from the fragment into page storage and out of the address bar', async () => {
    window.history.replaceState(null, '', `/brain?scope=x#token=${TOKEN}`);
    const { adoptLaunchCredential } = await freshCredential();

    adoptLaunchCredential();

    expect(window.location.href).toBe('http://localhost:3000/brain?scope=x');
    expect(sessionStorage.getItem('tracedecay.dashboard.access')).toBe(TOKEN);
    expect(document.cookie).toBe('');
  });

  it('sends the credential as a Basic header on same-origin requests only', async () => {
    window.history.replaceState(null, '', `/#token=${TOKEN}`);
    const { adoptLaunchCredential, dashboardFetch } = await freshCredential();
    adoptLaunchCredential();

    await dashboardFetch('/api/capabilities', { headers: { accept: 'application/json' } });
    expect(sentHeaders(fetchMock).get('authorization')).toBe(BASIC);
    expect(sentHeaders(fetchMock).get('accept')).toBe('application/json');

    await dashboardFetch('http://127.0.0.1:9/api/capabilities');
    expect(fetchMock).toHaveBeenLastCalledWith('http://127.0.0.1:9/api/capabilities', undefined);

    await dashboardFetch('/api/capabilities', { headers: { authorization: 'Bearer other' } });
    expect(sentHeaders(fetchMock).get('authorization')).toBe('Bearer other');
  });

  it('authenticates requests through the shared JSON transport', async () => {
    window.history.replaceState(null, '', `/#token=${TOKEN}`);
    const { adoptLaunchCredential } = await freshCredential();
    const { fetchJsonResponse } = await import('../query/responseBody.ts');
    adoptLaunchCredential();

    expect((await fetchJsonResponse('/api/settings')).outcome).toBe('response');
    expect(sentHeaders(fetchMock).get('authorization')).toBe(BASIC);
    expect(sentHeaders(fetchMock).get('accept')).toBe('application/json');
  });

  it('survives a reload through sessionStorage', async () => {
    sessionStorage.setItem('tracedecay.dashboard.access', TOKEN);
    const { dashboardFetch } = await freshCredential();

    await dashboardFetch('/api/capabilities');

    expect(sentHeaders(fetchMock).get('authorization')).toBe(BASIC);
  });

  it('never stores a malformed fragment token and still clears it from the address bar', async () => {
    window.history.replaceState(null, '', '/#token=not-a-token&view=a');
    const { adoptLaunchCredential, dashboardFetch } = await freshCredential();

    adoptLaunchCredential();
    await dashboardFetch('/api/capabilities');

    expect(window.location.hash).toBe('#view=a');
    expect(sessionStorage.getItem('tracedecay.dashboard.access')).toBeNull();
    expect(fetchMock).toHaveBeenLastCalledWith('/api/capabilities', undefined);
  });

  it('sends nothing when the page holds no credential (Hermes embed proxy supplies it)', async () => {
    const { dashboardFetch } = await freshCredential();

    await dashboardFetch('/api/capabilities', { method: 'POST' });

    expect(fetchMock).toHaveBeenLastCalledWith('/api/capabilities', { method: 'POST' });
  });
});
