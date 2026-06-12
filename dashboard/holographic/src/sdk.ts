/**
 * Accessor over the host dashboard plugin SDK (`window.__HERMES_PLUGIN_SDK__`).
 *
 * The host exposes React, UI primitives, utilities, and `fetchJSON` here (see
 * `web/src/plugins/registry.ts` → `exposePluginSDK()`). Pulling them off the
 * window keeps the plugin bundle from re-bundling React or the Nous design
 * system — those are shared with the host SPA.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */

const SDK: any =
  (typeof window !== "undefined" && (window as any).__HERMES_PLUGIN_SDK__) || {};

const components: any = SDK.components || {};
const utils: any = SDK.utils || {};

/** Raw JSON fetch against host-relative paths (handles auth + base-path). */
export const fetchJSON: <T>(url: string, init?: RequestInit) => Promise<T> =
  SDK.fetchJSON;

export const Card: any = components.Card;
export const CardHeader: any = components.CardHeader;
export const CardTitle: any = components.CardTitle;
export const CardContent: any = components.CardContent;
export const Badge: any = components.Badge;
export const Button: any = components.Button;
export const Input: any = components.Input;

export const cn: (...args: any[]) => string =
  utils.cn || ((...a: any[]) => a.filter(Boolean).join(" "));
export const timeAgo: ((ts: number) => string) | undefined = utils.timeAgo;
export const useI18n: any = SDK.useI18n;
