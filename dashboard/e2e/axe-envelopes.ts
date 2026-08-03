/**
 * The envelope every generated read model arrives in, edited in place.
 *
 * Its own module because three surfaces need it and none of them owns it:
 * Observatory and Costs build their overrides from it directly, and
 * `axe-code-freshness.ts` builds both of its own helpers on top of it. Copying
 * it into each would be three copies of one contract, and leaving it in
 * `axe-audit.ts` would put a payload builder in a file that composes scenarios.
 *
 * `axe-workspaces.ts` has the sibling of this for the routes the fixture
 * registry does not answer at all: there the envelope is donated by another
 * fixture and only the payload is written here.
 */
import { resolveFixture } from '../stories/fixtures/data.ts';

/**
 * A checked-in envelope fixture with its envelope and payload edited in place.
 *
 * Cloned rather than constructed, for the same reason `storageFindings` is:
 * `DashboardEnvelopeV1` carries scope, version, time, watermark, coverage,
 * freshness, authorization and legal actions, and an envelope missing one of
 * them fails `DashboardEnvelopeV1Schema` and arrives as `unsupported_schema`. Every
 * scenario built on it would then render the same schema notice, scan clean,
 * and prove nothing about the state it named.
 */
export function envelopeFixture(
  pathname: string,
  edit: (envelope: Record<string, unknown>, payload: Record<string, unknown>) => void,
): Record<string, unknown> {
  const base = structuredClone(resolveFixture(pathname, '')) as Record<string, unknown>;
  const payload = base['payload'];
  if (typeof payload !== 'object' || payload === null) {
    throw new Error(`the ${pathname} fixture carries no payload object to edit`);
  }
  edit(base, payload as Record<string, unknown>);
  return base;
}
