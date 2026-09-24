import { describe, expect, it } from 'vitest';
import {
  deliveryHref,
  readDeliveryLocation,
  writeDeliveryLocation,
} from './deliveryLocation.ts';

describe('deliveryLocation', () => {
  it('defaults to the inbox field with nothing selected', () => {
    const location = readDeliveryLocation(new URLSearchParams(''));
    expect(location.mode).toBe('inbox');
    expect(location.layout).toBe('field');
    expect(location.pullRequest).toBeNull();
    expect(location.attention).toBeNull();
    expect(location.status).toBeNull();
    expect(location.unresolvedOnly).toBe(false);
  });

  it('round-trips every addressable field through the URL', () => {
    const written = writeDeliveryLocation(new URLSearchParams(''), {
      mode: 'review',
      layout: 'table',
      renderer: 'lanes',
      project: 'project.alpha',
      pullRequest: 'project.alpha:github:42',
      umbrella: 'shared_work_objective:work.retry-backoff',
      attention: 'ci_failure',
      status: 'draft',
      provider: 'stale',
      unresolvedOnly: true,
      evidence: 'anchor.ci.42',
      episode: 'checks:check.integration',
      thread: 'review.R1',
      check: 'check.integration',
      lane: 'current',
    });
    const location = readDeliveryLocation(written);
    expect(location).toEqual({
      mode: 'review',
      layout: 'table',
      renderer: 'lanes',
      project: 'project.alpha',
      pullRequest: 'project.alpha:github:42',
      umbrella: 'shared_work_objective:work.retry-backoff',
      attention: 'ci_failure',
      status: 'draft',
      provider: 'stale',
      unresolvedOnly: true,
      evidence: 'anchor.ci.42',
      episode: 'checks:check.integration',
      thread: 'review.R1',
      check: 'check.integration',
      lane: 'current',
    });
  });

  it('rejects values outside the generated enums instead of guessing', () => {
    const location = readDeliveryLocation(
      new URLSearchParams('mode=merge&attention=risk_score&status=shipped&provider=green&lane=ok&renderer=sun'),
    );
    expect(location.mode).toBe('inbox');
    expect(location.renderer).toBeNull();
    expect(location.attention).toBeNull();
    expect(location.status).toBeNull();
    expect(location.provider).toBeNull();
    expect(location.lane).toBeNull();
  });

  it('writes defaults as absence so the canonical inbox URL stays bare', () => {
    const current = new URLSearchParams('mode=journey&layout=table&pr=x');
    const next = writeDeliveryLocation(current, { mode: 'inbox', layout: 'field' });
    expect(next.get('mode')).toBeNull();
    expect(next.get('layout')).toBeNull();
    expect(next.get('pr')).toBe('x');
    expect(deliveryHref(new URLSearchParams(''), { mode: 'inbox' })).toBe('/delivery');
  });

  it('leaves keys the patch does not name untouched', () => {
    const current = new URLSearchParams('pr=project.alpha%3Agithub%3A42&attention=ci_failure');
    const next = writeDeliveryLocation(current, { mode: 'journey' });
    expect(next.get('pr')).toBe('project.alpha:github:42');
    expect(next.get('attention')).toBe('ci_failure');
    expect(next.get('mode')).toBe('journey');
  });
});
