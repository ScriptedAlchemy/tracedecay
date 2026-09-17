import { describe, expect, it } from 'vitest';

import type { DeliveryAttentionItemV1 } from '../../contracts/generated.ts';
import {
  attentionNeedsOperator,
  codeNextSteps,
  compareHeadSearch,
  pullRequestHasOperatorAttention,
  rankAttention,
  settledAttentionReason,
} from './deliveryReading.ts';

function item(
  source: DeliveryAttentionItemV1['source'],
  state: DeliveryAttentionItemV1['state'],
  coverage: DeliveryAttentionItemV1['coverage'] = 'unsupported',
): DeliveryAttentionItemV1 {
  return {
    id: source,
    project_id: 'project.alpha',
    pull_request_id: '42',
    source,
    state,
    evidence: [],
    coverage,
    observed_at_micros: null,
  };
}

describe('delivery attention experience', () => {
  it('treats only active and denied attention as operator work', () => {
    expect(attentionNeedsOperator(item('ci_failure', 'active', 'complete'))).toBe(true);
    expect(attentionNeedsOperator(item('overlapping_edit', 'denied', 'denied'))).toBe(true);
    expect(attentionNeedsOperator(item('overlapping_edit', 'unavailable', 'unsupported'))).toBe(
      false,
    );
    expect(attentionNeedsOperator(item('ci_failure', 'clear', 'complete'))).toBe(false);
  });

  it('does not treat an unsupported source as a match just because the row carries it', () => {
    const attention = [
      item('ci_failure', 'active', 'complete'),
      item('overlapping_edit', 'unavailable', 'unsupported'),
    ];
    expect(pullRequestHasOperatorAttention(attention, 'ci_failure')).toBe(true);
    expect(pullRequestHasOperatorAttention(attention, 'overlapping_edit')).toBe(false);
  });

  it('leads with work and keeps unreadable sources in server order behind it', () => {
    const ranked = rankAttention([
      item('overlapping_edit', 'unavailable', 'unsupported'),
      item('ci_failure', 'active', 'complete'),
      item('confirmed_conflict', 'denied', 'denied'),
    ]);
    expect(ranked.map((entry) => entry.source)).toEqual([
      'ci_failure',
      'confirmed_conflict',
      'overlapping_edit',
    ]);
  });

  it('says an unsupported source is unavailable, not that an authority is unmounted', () => {
    expect(settledAttentionReason('unavailable', 'unsupported')).toBe(
      'Not available for this pull request.',
    );
    expect(settledAttentionReason('unavailable', 'unsupported')).not.toMatch(/mounted|authority/i);
  });
});

describe('delivery code next steps', () => {
  it('prefills Compare with the indexed head and does not open Shared Code without a symbol', () => {
    const steps = codeNextSteps('/code', {
      branch_ref: 'refs/heads/feature/delivery',
      indexed_head_commit_id: 'a'.repeat(40),
    });
    expect(steps.compare?.label).toBe('Compare this head');
    expect(steps.compare?.href).toBe(
      `/code?${compareHeadSearch('feature/delivery', 'a'.repeat(40)).toString()}`,
    );
    expect(steps.compare?.href).not.toBe('/code?view=compare');
    expect(steps.selectSymbol.href).toBe('/code');
    expect(steps.selectSymbol.href).not.toContain('view=shared-code');
    expect(steps.selectSymbol.detail).toMatch(/does not open a blocked Shared Code view/);
  });

  it('keeps a selected project on the Code jump', () => {
    const steps = codeNextSteps('/code?scope=project.alpha&scopeLabel=alpha', {
      branch_ref: 'refs/heads/main',
      indexed_head_commit_id: 'b'.repeat(40),
    });
    const href = steps.compare?.href ?? '';
    const params = new URL(href, 'http://local.invalid').searchParams;
    expect(params.get('scope')).toBe('project.alpha');
    expect(params.get('view')).toBe('compare');
    expect(params.get('head')).toBe('main');
    expect(params.get('head_revision')).toBe('b'.repeat(40));
    expect(steps.selectSymbol.href).toBe('/code?scope=project.alpha&scopeLabel=alpha');
  });
});
