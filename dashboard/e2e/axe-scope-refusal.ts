/**
 * The read-only scope refusal, on the two surfaces that can be refused.
 *
 * One claim in two places, which is why they share a module rather than each
 * joining their workspace's file. Selecting a project that is not the active
 * one makes every write on `/automations` and `/settings` refusable: the
 * dashboard gateway answers a write against a non-active project with 405, so
 * the controls disable themselves *before* dispatch and say why. The saying
 * why is the part under test here.
 *
 * A refusal is a sentence, not a chip. That is a deliberate product choice —
 * "Switch scope to the active project to make this change" is a remedy, and a
 * four-word tag cannot carry one — but it makes the refusal the longest run of
 * text on either surface, sitting beside a disabled control. Long explanatory
 * text next to a disabled control is precisely what gets clipped at 320 CSS px
 * and at 400% zoom, and a clipped remedy is worse than no remedy: the reader
 * sees a dead button and no way to revive it.
 *
 * So these two scenarios carry the full Plan 11 matrix, and they assert on
 * EVERY combination rather than once at 1440 — that the control really is
 * disabled, that the sentence naming the remedy is really on screen, and that
 * it is inside the viewport rather than running off the edge. `axe` runs over
 * all of it, including `prefers-contrast: more` and forced colors, where the
 * refusal's own token could otherwise disappear into the forced palette.
 *
 * The scope is reached by deep link, which is also the honest way to reach it:
 * `?scope=…` is what a shared link carries, and the reconciliation that turns
 * that link into a measured activation is the thing the refusal depends on.
 */
import type { Page } from '@playwright/test';
import { expectVisibleText, type Scenario } from './axe-harness.ts';

/**
 * A real, registered, non-active project, taken from the shared fixture rather
 * than stubbed here.
 *
 * `stories/fixtures/data.ts` sets `is_active: id === 'tracedecay'` and
 * `active_project_id: 'tracedecay'`, so `hermes` is non-active for the same
 * reason the daemon would say so: it is not the active id. It also means these
 * scenarios need NO route overrides at all. The fixture layer already performs
 * the gateway's own `/api/projects/{id}/{tail}` → `/api/{tail}` rewrite, so
 * every scoped read on these pages resolves to the same contract-checked body
 * the unscoped audit uses, and the surfaces reach the state under test instead
 * of an unsupported-schema panel.
 *
 * A hand-written entry body was the first attempt and was wrong twice over: it
 * had to be kept faithful to the generated contract by hand, and the override
 * glob (`**\/api/projects/{id}**`) also swallowed every scoped read beneath it,
 * so the scheduler control never rendered and there was nothing to refuse.
 */
const PROJECT_ID = 'hermes';

/** The canonical name the registry will answer with. */
const CANONICAL_LABEL = 'hermes';

/**
 * The name the link claims, which is not the project's name.
 *
 * Deep links are shared, bookmarked and edited, so the label in one is a claim
 * and not evidence. Carrying a wrong one here means each scenario also proves
 * the refusal names the project the REGISTRY identified — a refusal that
 * repeated the link's label would be telling the reader that writes are
 * blocked on a project that does not exist.
 */
const SPOOFED_LABEL = 'Scratch sandbox';

/** The sentence a refused control has to be able to say, in full. */
const REMEDY = 'Switch scope to the active project to make this change.';

function scopedRoute(path: string): string {
  return `${path}?scope=${PROJECT_ID}&scopeLabel=${encodeURIComponent(SPOOFED_LABEL)}`;
}

/**
 * Is this element inside the viewport, and did it get a real box?
 *
 * Both halves matter and they fail differently. A sentence laid out past the
 * right edge is unreadable but present; a sentence collapsed to nothing is
 * absent while `textContent` still reports it. Neither is caught by an axe
 * rule, and the second is what `expectVisibleText` alone would miss.
 */
async function assertReadable(page: Page, selector: string, tag: string): Promise<void> {
  const boxes = await page.evaluate(
    (sel) =>
      [...document.querySelectorAll(sel)].map((el) => {
        const r = el.getBoundingClientRect();
        return {
          left: r.left,
          right: r.right,
          width: r.width,
          height: r.height,
          viewport: window.innerWidth,
          text: (el.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 120),
        };
      }),
    selector,
  );
  if (boxes.length === 0) throw new Error(`${tag}: ${selector} matched nothing in the page`);
  // Every match, not the first: the refusals sit at different widths in the
  // layout and only some of them are near the edge.
  boxes.forEach((box, i) => {
    const where = `${tag}: ${selector} [${i + 1}/${boxes.length}]`;
    if (box.width < 1 || box.height < 1) {
      throw new Error(
        `${where} collapsed to ${box.width}x${box.height} — the refusal is in the DOM and on ` +
          `screen for nobody. Carrying: "${box.text}"`,
      );
    }
    // Half a pixel of tolerance for subpixel layout, and no more: this is the
    // measurement that says the remedy is reachable.
    if (box.right > box.viewport + 0.5 || box.left < -0.5) {
      throw new Error(
        `${where} runs outside the ${box.viewport}px viewport (left ${Math.round(box.left)}, ` +
          `right ${Math.round(box.right)}), so the remedy is clipped away. ` +
          `Carrying: "${box.text}"`,
      );
    }
  });
}

/**
 * The refusal names the project the registry identified, not the link.
 *
 * The deep link claims `Scratch sandbox`; the registry answers `hermes`. If
 * the link's label survived into the refusal, the reader would be told writes
 * are blocked on a project of that name — and would go looking for a project
 * that does not exist. This is the browser-side proof of the reconciliation,
 * asserted where the sentence is actually read.
 */
async function assertNamesTheRegistry(page: Page): Promise<void> {
  const body = (await page.evaluate(() => document.body.textContent)) ?? '';
  if (body.includes(SPOOFED_LABEL)) {
    throw new Error(
      `the page still shows the deep link's claimed label ${JSON.stringify(SPOOFED_LABEL)}, ` +
        `which the registry contradicted with ${JSON.stringify(CANONICAL_LABEL)}`,
    );
  }
}

/** Every control the surface offers, with whether it is disabled. */
async function controlStates(page: Page, selector: string): Promise<boolean[]> {
  return page.evaluate(
    (sel) => [...document.querySelectorAll(sel)].map((el) => (el as HTMLButtonElement).disabled),
    selector,
  );
}

export const SCOPE_REFUSAL_SCENARIOS: readonly Scenario[] = [
  {
    id: 'automations-read-only-scope',
    route: scopedRoute('/automations'),
    proves:
      'a non-active project disables the scheduler control before dispatch and keeps the remedy readable at 320px, 400% zoom, raised contrast and forced colors',
    overrides: {},
    matrix: true,
    assert: async (page) => {
      // The reading, once: refused, and refused for the stated reason.
      const note = page.locator('#scheduler-control-scope');
      const state = await note.getAttribute('data-scope-writability');
      if (state !== 'read_only') {
        throw new Error(
          `the scheduler scope note reports "${String(state)}", but a non-active project must ` +
            `read as read_only — anything else means the control is enabled for a write the ` +
            `gateway will refuse`,
        );
      }
      await assertNamesTheRegistry(page);
      await expectVisibleText(page, `${CANONICAL_LABEL} is not the active project`, 'the refusal');
      await expectVisibleText(page, REMEDY, 'the remedy');

      // Disabled BEFORE dispatch. A control that only learns from the 405 has
      // already sent the write.
      const disabled = await controlStates(page, 'button:has(> span)');
      const pauseDisabled = await page
        .getByRole('button', { name: /scheduler/i })
        .first()
        .isDisabled();
      if (!pauseDisabled) {
        throw new Error(
          `the scheduler control is enabled under a read-only scope (button states: ` +
            `${JSON.stringify(disabled)})`,
        );
      }

      // The note is the control's own description, so the refusal is announced
      // rather than merely displayed near it.
      const describedBy = await page
        .getByRole('button', { name: /scheduler/i })
        .first()
        .getAttribute('aria-describedby');
      if (describedBy !== 'scheduler-control-scope') {
        throw new Error(
          `the scheduler control points at "${String(describedBy)}" for its description, so a ` +
            `screen reader is not told why it is disabled`,
        );
      }
    },
    assertEachScan: async (page, tag) => {
      await assertReadable(page, '#scheduler-control-scope', tag);
    },
  },
  {
    id: 'settings-read-only-scope',
    route: scopedRoute('/settings'),
    proves:
      'a non-active project withdraws both settings editors with a readable reason at 320px, 400% zoom, raised contrast and forced colors',
    overrides: {},
    matrix: true,
    assert: async (page) => {
      const gates = page.locator('[data-settings-gate="read_only"]');
      const count = await gates.count();
      if (count < 1) {
        throw new Error(
          `no settings editor reported a read-only scope; the gates present are ` +
            `${JSON.stringify(
              await page.evaluate(() =>
                [...document.querySelectorAll('[data-settings-gate]')].map((el) =>
                  el.getAttribute('data-settings-gate'),
                ),
              ),
            )}`,
        );
      }
      await assertNamesTheRegistry(page);
      await expectVisibleText(page, `${CANONICAL_LABEL} is not the active project`, 'the refusal');
      await expectVisibleText(page, REMEDY, 'the remedy');
      // Nothing may still claim to be writable while the scope is refused —
      // two editors disagreeing about the same authority is the failure that
      // makes a reader trust the wrong one.
      const writable = await page.locator('[data-settings-gate="writable"]').count();
      if (writable !== 0) {
        throw new Error(
          `${writable} settings editor(s) still advertise a writable scope beside ${count} ` +
            `read-only one(s) — the same scope cannot be both`,
        );
      }
    },
    assertEachScan: async (page, tag) => {
      // The refusal must survive every width, not just be present at one. A
      // settings editor that quietly stops explaining itself at 320 is a
      // disabled control with no stated remedy.
      await assertReadable(page, '[data-settings-gate="read_only"]', tag);
    },
  },
];
