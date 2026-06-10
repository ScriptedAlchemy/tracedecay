/* eslint-disable @typescript-eslint/no-explicit-any */

const SDK: any =
  (typeof window !== "undefined" && (window as any).__HERMES_PLUGIN_SDK__) || {};

const components: any = SDK.components || {};
const utils: any = SDK.utils || {};

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

export const timeAgo: (ts: number) => string =
  utils.timeAgo || ((ts: number) => new Date(ts * 1000).toLocaleString());
