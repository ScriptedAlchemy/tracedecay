import { dashboardFetch } from './credential.ts';

type Listener = (event: MessageEvent<string>) => void;

const DEFAULT_RETRY_MS = 3000;

/**
 * The slice of the browser `EventSource` the dashboard uses, built on `fetch`
 * so each connection can carry the `Authorization` header that native
 * `EventSource` cannot send. Like the native class, a response that is not a
 * `200 text/event-stream` closes the source for good, while a dropped stream
 * reconnects (after the server's `retry:` hint) and resumes with
 * `Last-Event-ID`.
 */
export class AuthenticatedEventSource {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;

  readonly url: string;
  readyState: number = AuthenticatedEventSource.CONNECTING;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onmessage: Listener | null = null;

  private readonly listeners = new Map<string, Set<Listener>>();
  private readonly abort = new AbortController();
  private lastEventId = '';
  private retryMs = DEFAULT_RETRY_MS;

  constructor(url: string) {
    this.url = url;
    void this.run();
  }

  addEventListener(type: string, listener: Listener): void {
    const set = this.listeners.get(type) ?? new Set<Listener>();
    set.add(listener);
    this.listeners.set(type, set);
  }

  close(): void {
    this.readyState = AuthenticatedEventSource.CLOSED;
    this.abort.abort();
  }

  private get closed(): boolean {
    return this.readyState === AuthenticatedEventSource.CLOSED;
  }

  private async run(): Promise<void> {
    while (!this.closed) {
      let response: Response | null = null;
      try {
        const headers = new Headers({ accept: 'text/event-stream', 'cache-control': 'no-cache' });
        if (this.lastEventId !== '') headers.set('last-event-id', this.lastEventId);
        response = await dashboardFetch(this.url, { headers, signal: this.abort.signal });
      } catch {
        // Transport failure: fall through to the reconnect path below.
      }
      if (this.closed) return;
      if (response !== null) {
        const contentType = response.headers.get('content-type') ?? '';
        if (response.status !== 200 || response.body === null || !contentType.startsWith('text/event-stream')) {
          this.fail();
          return;
        }
        this.readyState = AuthenticatedEventSource.OPEN;
        this.onopen?.(new Event('open'));
        await this.consume(response.body);
        if (this.closed) return;
      }
      this.readyState = AuthenticatedEventSource.CONNECTING;
      this.onerror?.(new Event('error'));
      await new Promise((resolve) => setTimeout(resolve, this.retryMs));
    }
  }

  private fail(): void {
    this.readyState = AuthenticatedEventSource.CLOSED;
    this.onerror?.(new Event('error'));
  }

  private async consume(body: ReadableStream<Uint8Array>): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let pending = '';
    let event = { type: '', data: [] as string[] };
    const line = (text: string) => {
      if (text === '') {
        if (event.data.length > 0) this.dispatch(event.type || 'message', event.data.join('\n'));
        event = { type: '', data: [] };
        return;
      }
      if (text.startsWith(':')) return;
      const colon = text.indexOf(':');
      const field = colon === -1 ? text : text.slice(0, colon);
      let value = colon === -1 ? '' : text.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      if (field === 'data') event.data.push(value);
      else if (field === 'event') event.type = value;
      else if (field === 'id' && !value.includes('\0')) this.lastEventId = value;
      else if (field === 'retry' && /^\d+$/.test(value)) this.retryMs = Number(value);
    };
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) return;
        pending += decoder.decode(value, { stream: true });
        // A trailing CR may be the first half of a CRLF; hold it back.
        const complete = pending.endsWith('\r') ? pending.slice(0, -1) : pending;
        const lines = complete.split(/\r\n|\n|\r/);
        pending = lines.pop()! + pending.slice(complete.length);
        for (const text of lines) line(text.replace(/^﻿/, ''));
      }
    } catch {
      // A dropped or aborted stream ends this connection; run() decides what next.
    }
  }

  private dispatch(type: string, data: string): void {
    const event = new MessageEvent<string>(type, { data, lastEventId: this.lastEventId });
    if (type === 'message') this.onmessage?.(event);
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}
