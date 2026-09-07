import { resolveFixture } from '../../stories/fixtures/data.ts';

/**
 * Replaces only the payload of a canonical fixture envelope for DOM tests.
 *
 * The envelope fields remain sourced from the dashboard's fixture authority,
 * so tests do not invent time, coverage, authorization, or version claims to
 * exercise an unrelated payload branch.
 */
export function fixtureEnvelope(
  payload: unknown,
  domainState = 'ready',
): Record<string, unknown> {
  const envelope = resolveFixture('/api/storage/telemetry') as Record<string, unknown>;
  return { ...envelope, domain_state: domainState, payload };
}
