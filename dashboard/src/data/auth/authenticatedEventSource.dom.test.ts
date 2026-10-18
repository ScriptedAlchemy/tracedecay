import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AuthenticatedEventSource } from './authenticatedEventSource.ts';
import { installDashboardAuthentication } from './install.ts';
import { connectEvents } from '../sse/connect.ts';

const TOKEN = 'cd'.repeat(32);
const BASIC = `Basic ${btoa(`tracedecay:${TOKEN}`)}`;

/** A response whose body is fed chunk by chunk, so frames can split anywhere. */
function streamResponse(chunks: string[], init: ResponseInit = {}): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
  return new Response(body, {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
    ...init,
  });
}

/** Never-ending stream, so a connection stays open until closed. */
function openResponse(): Response {
  return new Response(new ReadableStream<Uint8Array>({ start() {} }), {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  });
}

async function eventually(check: () => void): Promise<void> {
  await vi.waitFor(check, { timeout: 2000, interval: 5 });
}

describe('AuthenticatedEventSource', () => {
  const fetchMock = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>();
  const sources: AuthenticatedEventSource[] = [];

  beforeEach(() => {
    sessionStorage.setItem('tracedecay.dashboard.access', TOKEN);
    fetchMock.mockReset();
    vi.stubGlobal('fetch', fetchMock);
  });
  afterEach(() => {
    for (const source of sources.splice(0)) source.close();
    vi.unstubAllGlobals();
    vi.useRealTimers();
    sessionStorage.clear();
  });

  function open(url = '/api/events') {
    const source = new AuthenticatedEventSource(url);
    sources.push(source);
    return source;
  }

  it('authenticates the stream with a request header and no cookie', async () => {
    fetchMock.mockImplementation(async () => openResponse());
    const source = open();
    const opened = vi.fn();
    source.onopen = opened;

    await eventually(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const headers = new Headers(fetchMock.mock.calls[0]![1]!.headers);
    expect(headers.get('authorization')).toBe(BASIC);
    expect(headers.get('accept')).toBe('text/event-stream');
    expect(fetchMock.mock.calls[0]![1]!.credentials).toBeUndefined();
    await eventually(() => expect(opened).toHaveBeenCalled());
    expect(source.readyState).toBe(1);
    source.close();
  });

  it('delivers named and default events across arbitrary chunk boundaries', async () => {
    fetchMock.mockResolvedValueOnce(
      streamResponse([
        ': keepalive\n\nevent: heart',
        'beat\ndata: {"a"',
        ':1}\r\n\r\ndata: one\r',
        '\ndata: two\n\n',
      ]),
    );
    fetchMock.mockImplementation(async () => openResponse());
    const source = open();
    const named = vi.fn();
    const plain = vi.fn();
    source.addEventListener('heartbeat', named);
    source.onmessage = plain;

    await eventually(() => expect(plain).toHaveBeenCalledTimes(1));
    expect(named.mock.calls[0]![0].data).toBe('{"a":1}');
    expect(plain.mock.calls[0]![0].data).toBe('one\ntwo');
    source.close();
  });

  it('reconnects with Last-Event-ID and the server retry hint after the stream drops', async () => {
    vi.useFakeTimers();
    fetchMock.mockResolvedValueOnce(streamResponse(['retry: 1\nid: 7\ndata: x\n\n']));
    fetchMock.mockImplementation(async () => openResponse());
    const source = open();
    const errors = vi.fn();
    source.onerror = errors;

    await vi.advanceTimersByTimeAsync(0);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(source.readyState).toBe(AuthenticatedEventSource.CONNECTING);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(new Headers(fetchMock.mock.calls[1]![1]!.headers).get('last-event-id')).toBe('7');
    expect(errors).toHaveBeenCalled();
    source.close();
  });

  it('closes for good on a refusal instead of retrying with the same credential', async () => {
    fetchMock.mockResolvedValue(new Response('{}', { status: 401 }));
    const source = open();
    const errors = vi.fn();
    source.onerror = errors;

    await eventually(() => expect(source.readyState).toBe(2));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(errors).toHaveBeenCalledTimes(1);
  });

  it('close() aborts the in-flight request', async () => {
    fetchMock.mockImplementation(async () => openResponse());
    const source = open();
    await eventually(() => expect(fetchMock).toHaveBeenCalled());

    source.close();

    expect(fetchMock.mock.calls[0]![1]!.signal!.aborted).toBe(true);
    expect(source.readyState).toBe(2);
  });
});

describe('installDashboardAuthentication', () => {
  let connection: ReturnType<typeof connectEvents> | undefined;
  afterEach(() => {
    connection?.close();
    vi.unstubAllGlobals();
    sessionStorage.clear();
    window.history.replaceState(null, '', '/');
  });

  it('routes the app event stream through an authenticated fetch', async () => {
    const fetchMock = vi.fn(async () => openResponse());
    vi.stubGlobal('fetch', fetchMock);
    window.history.replaceState(null, '', `/#token=${TOKEN}`);
    vi.stubGlobal('EventSource', globalThis.EventSource);
    installDashboardAuthentication();
    const active = connectEvents('/api/events');
    connection = active;

    await eventually(() => expect(active.state()).toBe('live'));
    const calls = fetchMock.mock.calls as unknown as Array<[string, RequestInit]>;
    expect(new Headers(calls[0]![1].headers).get('authorization')).toBe(BASIC);
    expect(window.location.hash).toBe('');
    connection.close();
  });
});
