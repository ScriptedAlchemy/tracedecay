/**
 * The thirteenth channel, now that its data plane is open — Work.
 *
 * Work reads nine mounted routes, so this file replaces the inventory of a
 * closed gate with evidence about a board drawn from real payloads. Two
 * scenarios, because this surface has two branches that must never look alike:
 * a snapshot the daemon answered, and a refusal it returned instead. The old
 * gate could only ever scan one branch, which is how a defect in the other one
 * survived; both are scanned here.
 *
 * The fixtures below are the daemon's own `HttpJsonEnvelope` — `kind`/`value`,
 * the outcome tag, then the packet holding the payload. Written out rather than
 * simplified, because a fixture that skipped the wrapper would pass while the
 * real wire failed.
 */
import { expectAbsent, expectEqual, expectVisibleText, type Scenario } from './axe-harness.ts';
import { workGraphRead } from '../src/test/workGraphFixture.ts';

/** One task at each recorded gate, so every stage group has content and the
 * densest layout is the one that gets scanned. */
const PROJECTIONS = [
  { taskId: 'task.parse', title: 'Parse the manifest' },
  {
    taskId: 'task.index',
    title: 'Index the workspace',
    acceptedProposal: 'proposal.index',
    dependencies: ['task.parse'],
  },
  {
    taskId: 'task.resolve',
    title: 'Resolve the dependency graph',
    acceptedProposal: 'proposal.resolve',
    acceptedAt: 1_800_000_000_000_000,
    dependencies: ['task.parse', 'task.index'],
  },
  {
    taskId: 'task.compact',
    title: 'Compact the session archive',
    acceptedProposal: 'proposal.compact',
    acceptedAt: 1_800_000_000_000_000,
    executionAdmittedAt: 1_800_000_000_000_000,
  },
  {
    taskId: 'task.publish',
    title: 'Publish the read model',
    acceptedProposal: 'proposal.publish',
    acceptedAt: 1_800_000_000_000_000,
    executionAdmittedAt: 1_800_000_000_000_000,
  },
];

function envelope(payload: unknown) {
  return {
    kind: 'success',
    value: {
      binding_id: 'binding.http.work.views',
      contract: { schema_id: 'schema.work.views.result', schema_revision: 1 },
      request_id: 'request.axe',
      scope: {
        project_id: 'project.tracedecay',
        repository_id: 'repository.tracedecay',
        worktree_id: 'worktree.primary',
        reference: null,
        scope_digest: 'sha256:scope',
      },
      outcome: { outcome: 'evidence', value: { payload } },
    },
  };
}

const GRAPH_READ = envelope(workGraphRead({ tasks: PROJECTIONS, version: 21 }));

export const WORK_SCENARIOS: readonly Scenario[] = [
  {
    id: 'work-board',
    route: '/work',
    proves:
      'the thirteenth channel draws its board from the snapshot the daemon returned, states how much of it it is showing, and offers a command only where the recorded state allows one',
    overrides: { '/api/work/views': { status: 200, body: GRAPH_READ } },
    // A dense ruled board across five stage groups is exactly the shape that
    // traps content in a collapsed scroller at 400% zoom, so this surface
    // carries the matrix for /work.
    matrix: true,
    assert: async (page) => {
      expectEqual(
        (await page.locator('[data-work-authority]').getAttribute('data-work-authority')) ?? '',
        'read',
        'the Work authority reading',
      );
      await expectVisibleText(page, 'Publish the read model', 'a task the snapshot returned');

      // Every task the fixture supplied must be on the board. A board that
      // dropped one would still look plausible.
      const drawn = await page.evaluate(() =>
        Array.from(document.querySelectorAll('[data-work-task]')).map(
          (row) => row.getAttribute('data-work-task') ?? '',
        ),
      );
      const expected = [
        'task.parse',
        'task.index',
        'task.resolve',
        'task.compact',
        'task.publish',
      ];
      for (const taskId of expected) {
        if (!drawn.includes(taskId)) {
          throw new Error(`FALSIFIED: the board omits ${taskId}, drawing ${JSON.stringify(drawn)}`);
        }
      }
      if (drawn.length !== expected.length) {
        throw new Error(
          `FALSIFIED: the board drew ${drawn.length} tasks from a snapshot of ${expected.length}`,
        );
      }

      // Each stage group is present, so an empty stage reads as empty rather
      // than as a stage this build does not know about.
      const stages = await page.evaluate(() =>
        Array.from(document.querySelectorAll('[data-work-stage]')).map(
          (group) => group.getAttribute('data-work-stage') ?? '',
        ),
      );
      for (const stage of [
        'proposal_open',
        'proposal_accepted',
        'task_accepted',
        'execution_admitted',
        'evidence_terminal',
      ]) {
        if (!stages.includes(stage)) throw new Error(`the board omits the ${stage} group`);
      }

      // The coverage reading is the claim about completeness. A board without
      // one is a fraction presented as a whole.
      await expectVisibleText(page, '5 of 5', 'the coverage reading');

      // The three commands whose inputs no generated contract supplies must be
      // named as gaps, never drawn as controls that could only fail.
      await expectAbsent(
        page,
        'button:has-text("Attach runtime evidence")',
        'no control for a command whose inputs this build cannot source',
      );
      await expectAbsent(
        page,
        'button:has-text("Accept proposal")',
        'no proposal control without a proposal inventory',
      );
    },
  },
  {
    id: 'work-unavailable',
    route: '/work',
    proves:
      'a Work runtime that refuses is drawn as the refusal it was, and never as an empty board',
    overrides: {
      '/api/work/views': { status: 503, body: { kind: 'problem', value: { problem: {} } } },
    },
    assert: async (page) => {
      await expectVisibleText(page, 'Work runtime is unavailable', 'the refusal reading');
      // The whole point. An unavailable runtime and a board of zero tasks are
      // different facts, and the first must not borrow the second's appearance.
      await expectAbsent(page, '[data-work-board]', 'no board drawn from a refusal');
      await expectAbsent(page, '[data-work-task]', 'no task drawn from a refusal');
      expectEqual(
        (await page.locator('[data-work-authority]').getAttribute('data-work-authority')) ?? '',
        'unread',
        'the Work authority reading under refusal',
      );
    },
  },
];
