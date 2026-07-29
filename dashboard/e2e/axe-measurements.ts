/**
 * How a metric plate is read, and what it must never print.
 *
 * Its own module because Observatory and Costs both assert against it and
 * neither owns it: the plate is one contract rendered on two routes, and a
 * check that drifted between the two copies would be a check that passes on the
 * route nobody changed. Nothing here builds a payload — these are readings and
 * the invariants over them, which is why they are not in `axe-envelopes.ts`.
 *
 * What they measure is the reading rather than the markup. Axe cannot tell
 * whether a unit reached the accessibility tree beside the figure it scales, or
 * whether a group header's tally agrees with its own plates; no rule fires,
 * because nothing in the markup is malformed.
 */
import type { Page } from '@playwright/test';

/**
 * The invariants every Plan 26 metric plate must hold, checked against what the
 * read actually carried rather than against a count written into this file.
 *
 * Asserting "7 of 9 measured" as a literal would pass for the wrong reasons the
 * moment a fixture changed. What matters is internal consistency: each group
 * header must agree with its own plates, and no plate may print a figure its
 * value does not support.
 */
export async function assertMetricPlateTruth(page: Page, what: string): Promise<void> {
  const report = await page.evaluate(() => {
    const problems: string[] = [];
    let plates = 0;
    let unavailable = 0;
    for (const group of Array.from(document.querySelectorAll('[data-metric-source]'))) {
      const source = group.getAttribute('data-metric-source') ?? '?';
      const inGroup = Array.from(group.querySelectorAll('[data-metric]'));
      let measured = 0;
      for (const plate of inGroup) {
        plates += 1;
        const id = plate.getAttribute('data-metric') ?? '?';
        const figure = (plate.querySelector('[data-cell="numeric"]')?.textContent ?? '').trim();
        if (plate.getAttribute('data-metric-available') === 'true') {
          measured += 1;
          if (figure === '—' || figure === '') {
            problems.push(`${id}: carries a value but printed ${JSON.stringify(figure)}`);
          }
          continue;
        }
        unavailable += 1;
        // The whole point of the contract: a measurement that does not exist
        // must not become a zero, an empty string, or any other figure.
        if (figure !== '—') {
          problems.push(`${id}: has no value but printed ${JSON.stringify(figure)}`);
        }
        const chip = plate.querySelector('[data-state="unknown"]');
        if (chip === null) {
          problems.push(`${id}: has no value and no unknown chip`);
          continue;
        }
        const reason = (chip.parentElement?.textContent ?? '').replace(/unknown/i, '').trim();
        if (reason === '') problems.push(`${id}: has no value and no reason`);
      }
      const tally = Array.from(group.querySelectorAll('span'))
        .map((span) => (span.textContent ?? '').trim())
        .find((text) => /^\d+ of \d+ measured$/.test(text));
      const expected = `${measured} of ${inGroup.length} measured`;
      if (tally !== expected) {
        problems.push(`${source}: header ${JSON.stringify(tally)} disagrees with its plates (${expected})`);
      }
    }
    return { problems, plates, unavailable };
  });
  if (report.plates === 0) throw new Error(`${what}: no metric plates rendered at all`);
  if (report.problems.length > 0) throw new Error(`${what}: ${report.problems.join(' | ')}`);
  console.log(
    `[axe]              ${what}: ${report.plates} plates, ${report.unavailable} without a value`,
  );
}

/**
 * The text one plate exposes to assistive technology, in order, with
 * `aria-hidden` and undisplayed subtrees removed.
 *
 * This is the measurable form of "programmatically associated, not merely
 * visually adjacent". A unit drawn by CSS, or hidden from the accessibility
 * tree, or lifted out of the list item that owns the figure, leaves a screen
 * reader announcing a bare number — and no axe rule reports it, because
 * nothing in the markup is malformed.
 */
async function exposedPlateText(page: Page, metric: string): Promise<string[]> {
  return page.evaluate((id) => {
    const plate = document.querySelector(`[data-metric="${id}"]`);
    if (plate === null) return [];
    const parts: string[] = [];
    // Depth-first with an explicit stack, children pushed in reverse so they
    // pop in document order. Not a recursive inner function: esbuild's
    // `keepNames` rewrites a named function expression into a call to a
    // `__name` helper that does not exist inside the page, and the evaluate
    // then dies with `ReferenceError: __name is not defined`.
    const stack: Node[] = [plate];
    while (stack.length > 0) {
      const node = stack.pop()!;
      if (node.nodeType === Node.TEXT_NODE) {
        const text = (node.textContent ?? '').trim();
        if (text !== '') parts.push(text);
        continue;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) continue;
      const element = node as Element;
      if (element.getAttribute('aria-hidden') === 'true') continue;
      if (getComputedStyle(element).display === 'none') continue;
      const children = Array.from(element.childNodes);
      for (let i = children.length - 1; i >= 0; i -= 1) stack.push(children[i]!);
    }
    return parts;
  }, metric);
}

/** One plate's printed figure, and whether the wire gave it a value at all. */
export async function plateReading(
  page: Page,
  metric: string,
): Promise<{ figure: string; available: string }> {
  return page.evaluate((id) => {
    const plate = document.querySelector(`[data-metric="${id}"]`);
    if (plate === null) return { figure: '(no plate)', available: '(no plate)' };
    return {
      figure: (plate.querySelector('[data-cell="numeric"]')?.textContent ?? '').trim(),
      available: plate.getAttribute('data-metric-available') ?? '(unset)',
    };
  }, metric);
}

/** A figure, its unit and its labelled denominator must all reach assistive
 * technology, in that order, from inside the one list item that is the
 * measurement. */
export async function assertMeasurementIsSelfDescribing(
  page: Page,
  metric: string,
  expected: { figure: string; unit: string; denominator: string },
): Promise<void> {
  const parts = await exposedPlateText(page, metric);
  if (parts.length === 0) throw new Error(`${metric}: no plate on the page to read`);
  const at = (needle: string) => parts.findIndex((part) => part.includes(needle));
  const figure = at(expected.figure);
  const unit = at(expected.unit);
  const denominator = at(expected.denominator);
  const term = at('denominator');
  const missing = [
    figure < 0 ? `figure ${JSON.stringify(expected.figure)}` : '',
    unit < 0 ? `unit ${JSON.stringify(expected.unit)}` : '',
    denominator < 0 ? `denominator ${JSON.stringify(expected.denominator)}` : '',
    term < 0 ? 'the word "denominator"' : '',
  ].filter((entry) => entry !== '');
  if (missing.length > 0) {
    throw new Error(
      `${metric}: ${missing.join(', ')} never reached the accessibility tree. ` +
        `Exposed: ${JSON.stringify(parts)}`,
    );
  }
  if (unit < figure) {
    throw new Error(
      `${metric}: the unit is announced before its figure, so the reading arrives out of order. ` +
        `Exposed: ${JSON.stringify(parts)}`,
    );
  }
  if (term > denominator) {
    throw new Error(
      `${metric}: the denominator value is announced before the term that names it. ` +
        `Exposed: ${JSON.stringify(parts)}`,
    );
  }
}
