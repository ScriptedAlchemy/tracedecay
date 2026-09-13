/** Hermes same-origin mount for the embedded dashboard iframe. */
export const HERMES_EMBED_BASENAME = '/api/plugins/tracedecay/embed';

/**
 * Basename for the dashboard router when Hermes proxies the SPA.
 *
 * The embed path must be an exact prefix (`/embed` or `/embed/...`). A sibling
 * such as `/embed-evil` is a different route and must not inherit the embed
 * public path or router basename.
 */
export function dashboardRouterBasename(pathname: string): string | undefined {
  if (pathname === HERMES_EMBED_BASENAME || pathname.startsWith(`${HERMES_EMBED_BASENAME}/`)) {
    return HERMES_EMBED_BASENAME;
  }
  return undefined;
}

/**
 * Point Rspack/webpack async chunks at the Hermes embed prefix so lazy
 * workspace imports stay on the same-origin proxy.
 */
export function applyEmbeddedPublicPath(basename: string | undefined): void {
  if (basename === undefined) return;
  (
    globalThis as typeof globalThis & { __webpack_public_path__?: string }
  ).__webpack_public_path__ = `${basename}/`;
}
