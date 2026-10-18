/** Optional operator-provided exports. Never bundle private profile files. */
type ProfilePack = Record<string, unknown>;
declare global {
  interface Window { TRACEDECAY_PROFILE_PACK?: ProfilePack }
}

export function profileRows<T>(path: string): T[] | null {
  const value = typeof window === "undefined" ? undefined : window.TRACEDECAY_PROFILE_PACK?.[path];
  if (value === undefined) return null;
  if (!Array.isArray(value)) throw new TypeError(`Profile export must be an array: ${path}`);
  return value as T[];
}
