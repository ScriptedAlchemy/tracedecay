/**
 * A recording stand-in for the browser `EventSource`, for tests that own the
 * connection lifecycle: every construction is kept on `instances`, every
 * `close()` is counted, and frames are pushed by hand with `emit`.
 */
type Listener = (event: MessageEvent<string>) => void;

export class FakeEventSource {
  static readonly CLOSED = 2;
  static instances: FakeEventSource[] = [];

  readonly listeners = new Map<string, Listener[]>();
  readyState = 1;
  closeCalls = 0;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: Listener | null = null;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  /** Sources constructed so far that have not been closed. */
  static open(): FakeEventSource[] {
    return FakeEventSource.instances.filter((source) => source.readyState !== FakeEventSource.CLOSED);
  }

  addEventListener(name: string, listener: Listener) {
    const listeners = this.listeners.get(name) ?? [];
    listeners.push(listener);
    this.listeners.set(name, listeners);
  }

  emit(name: string, data: unknown) {
    const event = { data: JSON.stringify(data) } as MessageEvent<string>;
    for (const listener of this.listeners.get(name) ?? []) listener(event);
  }

  close() {
    this.closeCalls += 1;
    this.readyState = FakeEventSource.CLOSED;
  }
}
