/**
 * Branch-aware code-index freshness — Code. Five server states, four of which
 * are only reachable by overriding the route.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives: `freshness` and `mountedWorktree` are payload
 * builders nothing else uses. They are what makes the five states legible as
 * five — an absent registry, an attached registry with nothing mounted, a mount
 * still indexing, a sealed generation with incomplete coverage, and a mount
 * that is ready and separately unauthorized all differ by a field or two inside
 * the same envelope, which is exactly the way they get confused.
 *
 * The envelope itself is shared with Observatory and Costs and lives in
 * `axe-envelopes.ts`.
 */
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  type Scenario,
} from './axe-harness.ts';
import { envelopeFixture } from './axe-envelopes.ts';

const FRESHNESS = '/api/code-index/freshness';

/** The freshness fixture's own mounted worktree, with fields replaced. */
function mountedWorktree(over: Record<string, unknown>): Record<string, unknown> {
  const fixture = envelopeFixture(FRESHNESS, () => {});
  const payload = fixture['payload'] as Record<string, unknown>;
  const worktrees = payload['worktrees'] as Record<string, unknown>[];
  return { ...worktrees[0]!, ...over };
}

/** A freshness read in one of the route's five states. */
function freshness(spec: {
  state: string;
  note: string;
  worktrees?: unknown[];
  authorization?: 'authorized' | 'denied' | 'redacted' | 'unauthorized';
}): Record<string, unknown> {
  return envelopeFixture(FRESHNESS, (envelope, payload) => {
    envelope['domain_state'] = spec.state;
    envelope['authorization'] = { outcome: spec.authorization ?? 'authorized' };
    payload['worktrees'] = spec.worktrees ?? [];
    payload['note'] = spec.note;
  });
}

export const CODE_FRESHNESS_SCENARIOS: readonly Scenario[] = [
  {
    id: 'code-freshness-fresh',
    route: '/code',
    proves: 'a sealed generation with complete coverage names the source reference it is a picture of',
    overrides: {},
    assert: async (page) => {
      const panel = page.locator('[data-index-freshness]');
      expectEqual(
        (await panel.getAttribute('data-index-freshness')) ?? '',
        'ready',
        'the freshness domain state',
      );
      await expectVisibleText(
        page,
        'refs/heads/codex/tracedecay-total-redesign-plan',
        'the branch the generation was sealed against',
      );
      await expectAbsent(
        page,
        '[data-state="unauthorized"]',
        'no authorization chip on an authorized read',
      );
    },
  },
  {
    id: 'code-freshness-unsupported',
    route: '/code',
    proves:
      'no daemon scheduler registry at all is `unsupported` — there is no generation to report, and no fresh badge is drawn',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'unsupported',
          note: 'no daemon-owned scheduler registry is attached to this dashboard, so no sealed generation can be reported',
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'unsupported',
        'the freshness domain state',
      );
      await expectVisibleText(
        page,
        'no daemon-owned scheduler registry is attached',
        "the route's own note, which is the only thing separating this from an unmounted project",
      );
      await expectAbsent(page, '[data-worktree-staleness]', 'no worktree readings to show');
    },
  },
  {
    id: 'code-freshness-no-mount',
    route: '/code',
    proves:
      'a registry that is attached with nothing mounted for this project is `unknown` — the same empty list, a different claim',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'unknown',
          note: 'a scheduler registry is attached but holds no mounted scheduler for this project',
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'unknown',
        'the freshness domain state',
      );
      await expectVisibleText(
        page,
        'holds no mounted scheduler for this project',
        'the note that distinguishes this from an absent registry',
      );
    },
  },
  {
    id: 'code-freshness-indexing',
    route: '/code',
    proves: 'a mount that is still indexing is `loading`, not stale and not ready',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'loading',
          note: 'a scheduler is mounted and indexing; no generation has been sealed yet',
          worktrees: [
            mountedWorktree({
              latest_generation_id: null,
              snapshot_content_identity: null,
              sealed_at_micros: null,
              staleness_state: null,
              coverage: 'indexing',
              hook_hint_count: 41,
            }),
          ],
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'loading',
        'the freshness domain state',
      );
      // Every absent identity field says it is absent. None of them become an
      // epoch date or an empty cell.
      await expectVisibleText(page, 'no sealed generation yet', 'the unsealed generation');
      await expectVisibleText(page, 'not reported', 'the unreported staleness and stamps');
      await expectAbsent(page, 'text=1970-01-01', 'no epoch date standing in for an absent stamp');
    },
  },
  {
    id: 'code-freshness-incomplete-coverage',
    route: '/code',
    proves:
      'a sealed generation whose coverage is incomplete is `partial` — the generation exists and does not cover everything',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'partial',
          note: 'the sealed generation exists and the scheduler reports incomplete coverage of it',
          worktrees: [
            mountedWorktree({
              coverage: 'incomplete',
              staleness_state: 'stale',
              source_reference: 'refs/heads/master',
              hook_hint_count: 128,
            }),
          ],
        }),
      },
    },
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-index-freshness]').getAttribute('data-index-freshness')) ?? '',
        'partial',
        'the freshness domain state',
      );
      expectEqual(
        (await page.locator('[data-worktree-staleness]').first().getAttribute(
          'data-worktree-staleness',
        )) ?? '',
        'stale',
        'the worktree staleness',
      );
      // The branch-aware part: a generation sealed against master while the
      // checkout is elsewhere is stale in a way no node count reveals.
      await expectVisibleText(page, 'refs/heads/master', 'the reference the generation is of');
      await expectVisibleText(page, 'incomplete', 'the coverage shortfall');
    },
  },
  {
    id: 'code-freshness-unauthorized',
    route: '/code',
    proves:
      'THE AUTHORIZATION AXIS — a mount that is ready and separately unauthorized shows both states',
    overrides: {
      [FRESHNESS]: {
        status: 200,
        body: freshness({
          state: 'ready',
          authorization: 'unauthorized',
          note: 'the mount is sealed and current; the asking identity is not authorized for its contents',
          worktrees: [mountedWorktree({})],
        }),
      },
    },
    assert: async (page) => {
      const panel = page.locator('[data-index-freshness]');
      expectEqual(
        (await panel.getAttribute('data-index-freshness')) ?? '',
        'ready',
        'the freshness domain state is untouched by the authorization outcome',
      );
      const unauthorized = panel.locator('[data-state="unauthorized"]').first();
      if ((await unauthorized.count()) === 0) {
        throw new Error('FALSIFIED: an unauthorized read rendered no unauthorized chip');
      }
      expectContains(
        (await unauthorized.textContent()) ?? '',
        'read authorization',
        'the unauthorized chip names its axis',
      );
      if ((await panel.locator('[data-state="ready"]').count()) === 0) {
        throw new Error(
          'FALSIFIED: the authorization outcome replaced the domain state instead of joining it',
        );
      }
    },
  },
];
