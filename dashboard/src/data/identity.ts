/** Mint once per logical effect and retain the returned key for every retry. */
export function mintBrowserIdempotencyKey(
  surface: 'dashboard-doctor' | 'dashboard-settings',
): string {
  if (typeof globalThis.crypto?.randomUUID !== 'function') {
    throw new Error('secure browser idempotency identity is unavailable');
  }
  return `idempotency.${surface}.${globalThis.crypto.randomUUID()}`;
}
