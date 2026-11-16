import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityReadResultV1,
} from '../../contracts/generated.ts';
import { formatMicrosUtc } from '../../ui/format.ts';
import type { WorkResult } from '../../workspaces/work/workApi.ts';
import { ProximityPanel } from './ProximityPanel.tsx';
import { proximityLabel } from './proximity.ts';

const scope = {
  project_id: 'project.proximity-panel',
  repository_id: 'repository.proximity-panel',
  worktree_id: 'worktree.proximity-panel',
  branch_ref: 'refs/heads/proximity-panel',
  head_commit_id: 'commit.proximity-panel',
};

const encounter: FeedbackProximityEncounterV1 = {
  encounter_id: `sha256:${'b'.repeat(64)}`,
  scope,
  interval: { start: 1_700_000_000_000_000, end: 1_700_000_060_000_000 },
  participants: [
    {
      source: { provider: 'provider.cursor', session_id: 'session.proximity-panel' },
      agent_id: 'agent.proximity-panel',
      worktree_id: 'worktree.proximity-panel',
      worktree_root: '/workspace',
      branch_ref: 'refs/heads/proximity-panel',
      head_revision: 'commit.proximity-panel',
      access: 'write',
      activity: { start: 1_700_000_000_000_000, end: 1_700_000_060_000_000 },
      address: {
        scope,
        file: 'file.proximity-panel',
        span: { start_byte: 0, end_byte: 5 },
        symbol: null,
      },
    },
  ],
  relation: { relation_kind: 'overlapping_edit', warning_class: 'same_file' },
  observed_at: 1_700_000_000_000_000,
  expires_at: 1_700_000_600_000_000,
  coverage: 'complete',
};

const complete: WorkResult<FeedbackProximityReadResultV1> = {
  outcome: 'value',
  value: {
    state: 'complete',
    page: {
      scope,
      source_generation: 'generation.proximity-panel',
      observed_at: encounter.observed_at,
      expires_at: encounter.expires_at,
      encounters: [encounter],
    },
  },
};

function renderPanel(result: WorkResult<FeedbackProximityReadResultV1>) {
  return render(
    <ProximityPanel result={result} selectedId={null} onSelect={() => undefined} />,
  );
}

describe('ProximityPanel', () => {
  it('renders the encounter interval and relation label without a score', () => {
    const { container } = renderPanel(complete);
    const text = container.textContent ?? '';

    expect(text).toContain(formatMicrosUtc(encounter.interval.start));
    expect(text).toContain(formatMicrosUtc(encounter.interval.end));
    expect(text).toContain(proximityLabel(encounter.relation));

    const scored = [...container.querySelectorAll('*')].filter((element) =>
      /risk|score|threshold/i.test(
        [...element.childNodes]
          .filter((node) => node.nodeType === Node.TEXT_NODE)
          .map((node) => node.textContent ?? '')
          .join(' '),
      ),
    );
    expect(
      scored.map((element) => element.textContent),
      'proximity is evidence, not a risk score',
    ).toEqual([]);
  });
});
