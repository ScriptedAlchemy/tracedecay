/**
 * The dashboard's access credential.
 *
 * Data shape: one 64-hex launch token per listener, delivered once in the URL
 * fragment of the launch redirect (`/#token=…`). A fragment is never sent over
 * the wire or in a `Referer`. The token then lives in this origin's
 * `sessionStorage` (an origin includes its port, so concurrent dashboards
 * never share it) and rides every same-origin request as
 * `Authorization: Basic base64("tracedecay:<token>")`.
 *
 * It is deliberately never a cookie: browsers scope cookies by host, not port,
 * so a cookie would be replayed to every other service on the same loopback
 * host.
 */

const STORAGE_KEY = 'tracedecay.dashboard.access';
const FRAGMENT_KEY = 'token';
const BASIC_USER = 'tracedecay';
const TOKEN_SHAPE = /^[0-9a-f]{64}$/;

/** Fallback when the browser refuses `sessionStorage` (the token then lasts
 * until the page reloads, and the person re-opens the launch URL). */
let memoryToken: string | null = null;

function pageStorage(): Storage | null {
  try {
    return globalThis.sessionStorage ?? null;
  } catch {
    return null;
  }
}

/** Moves a launch token from the URL fragment into page storage and removes it
 * from the address bar and history entry. Idempotent: a page with no token in
 * its fragment is left alone. */
export function adoptLaunchCredential(): void {
  const location = globalThis.location;
  if (location === undefined) return;
  const fragment = new URLSearchParams(location.hash.replace(/^#/, ''));
  const token = fragment.get(FRAGMENT_KEY);
  if (token === null) return;
  fragment.delete(FRAGMENT_KEY);
  const rest = fragment.toString();
  globalThis.history.replaceState(
    globalThis.history.state,
    '',
    `${location.pathname}${location.search}${rest === '' ? '' : `#${rest}`}`,
  );
  if (!TOKEN_SHAPE.test(token)) return;
  memoryToken = token;
  try {
    pageStorage()?.setItem(STORAGE_KEY, token);
  } catch {
    // Quota or policy refusal: the in-memory copy still serves this page.
  }
}

function currentToken(): string | null {
  if (memoryToken !== null) return memoryToken;
  try {
    const stored = pageStorage()?.getItem(STORAGE_KEY) ?? null;
    return stored !== null && TOKEN_SHAPE.test(stored) ? stored : null;
  } catch {
    return null;
  }
}

function isSameOrigin(url: string): boolean {
  const location = globalThis.location;
  if (location === undefined) return false;
  try {
    return new URL(url, location.href).origin === location.origin;
  } catch {
    return false;
  }
}

/** `fetch` that authenticates same-origin requests with the dashboard
 * credential. Cross-origin requests and pages without a credential (the
 * Hermes embed, whose proxy supplies it) go out untouched. */
export function dashboardFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const token = currentToken();
  const url = input instanceof Request ? input.url : String(input);
  if (token === null || !isSameOrigin(url)) return fetch(input, init);
  const headers = new Headers(input instanceof Request ? input.headers : undefined);
  new Headers(init?.headers).forEach((value, name) => headers.set(name, value));
  if (!headers.has('authorization')) {
    headers.set('authorization', `Basic ${btoa(`${BASIC_USER}:${token}`)}`);
  }
  return fetch(input, { ...init, headers });
}
