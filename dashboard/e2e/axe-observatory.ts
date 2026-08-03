/**
 * Plan 26 canonical observations — Observatory.
 *
 * Own module rather than more of `axe-audit.ts`, for the reason
 * `axe-workspaces.ts` gives: `withoutValue` is a payload builder nothing else
 * uses, because Observatory is the only route where a whole producing source
 * can go missing while its siblings answer. The plate readings these assert
 * with are shared with Costs and live in `axe-measurements.ts`; the envelope
 * they are edited into is shared with Costs and Code and lives in
 * `axe-envelopes.ts`.
 *
 * The four states are chosen for what they put next to each other: a measured
 * zero beside a missing measurement, a failed source beside answering ones, a
 * complete read that is separately redacted, and a read model carrying no
 * measurements at all.
 */
import { DoctorEvidenceStateV1Schema } from '../src/contracts/generated.ts';
import {
  expectAbsent,
  expectContains,
  expectEqual,
  expectVisibleText,
  type Scenario,
} from './axe-harness.ts';
import { envelopeFixture } from './axe-envelopes.ts';
import {
  assertMeasurementIsSelfDescribing,
  assertMetricPlateTruth,
  plateReading,
} from './axe-measurements.ts';

const OBSERVATORY = '/api/observatory';

/**
 * One measurement turned into the projector's own unavailable reading.
 *
 * Derived from the fixture's metric so descriptor, provenance and cohort stay
 * the shapes the contract gate already checks, and it nulls the denominator
 * size and drops coverage to `unknown` exactly as
 * `application::observability` does — a plate that kept an eligible count
 * beside a missing value would be a state the projector never emits.
 */
function withoutValue(metric: Record<string, unknown>, reason: string): Record<string, unknown> {
  const coverage = metric['coverage'] as Record<string, unknown>;
  return {
    ...metric,
    value: null,
    denominator_value: null,
    unavailable_reason: reason,
    coverage: {
      ...coverage,
      state: 'unknown',
      eligible: null,
      observed: 0,
      completed: 0,
      unknown: 1,
    },
    uncertainty: { lower: null, upper: null, reason },
  };
}

/** Every `DoctorEvidenceStateV1`, from the contract rather than a copy of it —
 * the fixture carries one finding per state, so this is also the count of
 * evidence badges the scan must find, and a ninth state added in Rust has to
 * make that requirement fail rather than go unscanned. */
const DOCTOR_EVIDENCE_STATES = DoctorEvidenceStateV1Schema.options.map((option) => option.value);

/**
 * The WCAG contrast of each evidence badge's label against the background it
 * actually renders on, measured in the browser.
 *
 * Walks up for the first non-transparent background rather than assuming the
 * badge paints its own, and composites any alpha over it — a label on a
 * translucent chip is read against what shows through, not against the chip's
 * declared colour.
 */
/**
 * Contrast measurement, shared by every probe below.
 *
 * Colours resolve through a canvas rather than by parsing the string. The theme
 * is authored in `oklch()` and Chromium serializes those computed values in
 * their own colour space, so a naive "grab the first three numbers" parse reads
 * L, C and H as if they were 8-bit RGB — which scored every badge at 1.39:1
 * regardless of state while axe's own rule reported no violation. The 2d
 * context converts any CSS colour to sRGB bytes for us.
 *
 * Defines `parse`, `lum`, `over`, `backdrop` and `contrast` for the caller.
 */
const CONTRAST_PRELUDE = `
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  const parse = (value) => {
    if (value === '' || value === 'none') return null;
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillStyle = '#000000';
    ctx.fillStyle = value;
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillRect(0, 0, 1, 1);
    const d = ctx.getImageData(0, 0, 1, 1).data;
    return [d[0], d[1], d[2], d[3] / 255];
  };
  const channel = (v) => { const s = v / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; };
  const lum = ([r, g, b]) => 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
  const over = (fg, bg) => fg.slice(0, 3).map((c, i) => c * fg[3] + bg[i] * (1 - fg[3]));
  const backdrop = (el) => {
    for (let node = el; node; node = node.parentElement) {
      const c = parse(getComputedStyle(node).backgroundColor);
      if (c && c[3] > 0) return c[3] === 1 ? c.slice(0, 3) : over(c, backdrop(node.parentElement));
    }
    return [255, 255, 255];
  };
  const contrast = (el) => {
    const style = getComputedStyle(el);
    const bg = backdrop(el);
    const fgRaw = parse(style.color) ?? [0, 0, 0, 1];
    const fg = fgRaw[3] === 1 ? fgRaw.slice(0, 3) : over(fgRaw, bg);
    const [hi, lo] = [lum(fg), lum(bg)].sort((a, b) => b - a);
    return {
      fontPx: Number.parseFloat(style.fontSize),
      ratio: Number((((hi + 0.05) / (lo + 0.05))).toFixed(2)),
    };
  };
`;

/**
 * The scope sentence beside each remediation control, and whether the controls
 * it explains are actually disabled.
 *
 * Measured rather than asserted from the DOM alone, because the failure mode
 * this guards is a reason rendered too dim to read beside a control that has
 * been greyed out — which reads as a broken page rather than a scope the daemon
 * will not accept a write for.
 */
const SCOPE_NOTE_PROBE = `(() => {
  ${CONTRAST_PRELUDE}
  return Array.from(document.querySelectorAll('[data-scope-writability]')).map((note) => {
    // The controls this sentence explains: the card or dialog it sits in.
    const owner = note.closest('div') ?? document.body;
    const buttons = Array.from(owner.querySelectorAll('button'));
    return {
      state: note.getAttribute('data-scope-writability') ?? '',
      text: (note.textContent ?? '').trim(),
      disabledControls: buttons.filter((button) => button.disabled).length,
      ...contrast(note),
    };
  });
})()`;

interface ScopeNote {
  state: string;
  text: string;
  disabledControls: number;
  fontPx: number;
  ratio: number;
}

/**
 * The name the scope bar displays for the current project, and whether it is
 * presented as confirmed.
 *
 * Read from the bar rather than from the store, because the question is what a
 * reader is told: the label reaches prose about which project a control acts
 * on, so a name sourced from a URL is a spoofing surface, and a name the
 * registry has not confirmed must carry the annotation that says so.
 */
const SCOPE_LABEL_PROBE = `(() => {
  ${CONTRAST_PRELUDE}
  const value = document.querySelector('header [aria-label^="Clear project scope"] .td-value');
  if (!value) return { text: '', annotation: null, fontPx: 0, ratio: 0 };
  const annotation = document.querySelector('[data-scope-label-annotation]');
  return {
    text: (value.textContent ?? '').trim(),
    annotation: annotation ? annotation.getAttribute('data-scope-label-annotation') : null,
    ...contrast(value),
  };
})()`;

interface ScopeLabel {
  text: string;
  annotation: string | null;
  fontPx: number;
  ratio: number;
}

const BADGE_CONTRAST_PROBE = `(() => {
  ${CONTRAST_PRELUDE}
  return Array.from(document.querySelectorAll('[data-evidence-state]')).map((badge) => {
    // The innermost element carrying the label text, or the badge itself when
    // the label is a bare child of it — which is how the defective version
    // rendered, and the case that has to reach the measurement rather than
    // stopping short of it.
    const label = Array.from(badge.querySelectorAll('span')).find(
      (s) => s.querySelector('span') === null && (s.textContent ?? '').trim() !== '',
    ) ?? badge;
    return {
      state: badge.getAttribute('data-evidence-state') ?? '',
      text: (label.textContent ?? '').trim(),
      ...contrast(label),
    };
  });
})()`;

interface BadgeContrast {
  state: string;
  text: string;
  fontPx: number;
  ratio: number;
}

export const OBSERVATORY_SCENARIOS: readonly Scenario[] = [
  {
    id: 'observatory-doctor-findings',
    route: '/observatory',
    proves:
      'a populated Doctor report is actually scanned, and every evidence badge label clears WCAG AA against the surface it renders on',
    // No override: this is the shipped fixture. That is the point of the
    // scenario. `/api/doctor/findings` used to serve an empty envelope with a
    // comment saying a populated one could not pass the gate, because the badge
    // painted its label in an indicator hue that misses AA. The gate stayed
    // green by never rendering the defect. Serving the real report here means a
    // regression to that state fails on the assertion below rather than
    // quietly shrinking what the scan covers.
    overrides: {},
    // Light and dark at every matrix viewport, plus contrast-more and
    // forced-colors in both themes. `color-contrast` runs in all of them except
    // forced colors, where the OS palette replaces every declared colour and
    // `FORCED_COLORS_PROBE` measures instead.
    matrix: true,
    assert: async (page) => {
      const badges = (await page.evaluate(BADGE_CONTRAST_PROBE)) as BadgeContrast[];
      const found = badges.map((badge) => badge.state).sort();
      const expected = [...DOCTOR_EVIDENCE_STATES].sort();
      if (JSON.stringify(found) !== JSON.stringify(expected)) {
        throw new Error(
          `FALSIFIED: the Doctor report must put one badge on screen per evidence state, or the scan is measuring an emptier page than it appears to. expected ${JSON.stringify(expected)}, found ${JSON.stringify(found)}`,
        );
      }
      // Recorded before it is judged, so a failing run reports the whole set of
      // measurements rather than only the ones that tripped the threshold.
      console.log(
        `[axe] doctor evidence badge contrast @ light/1440x900: ${badges
          .map((badge) => `${badge.state} ${badge.ratio}:1 at ${badge.fontPx}px`)
          .join(', ')}`,
      );
      // 4.5:1 is the threshold that applies: the label renders at 11px, well
      // under the 18.66px-bold / 24px that would qualify it as large text. The
      // check reads the font size back rather than trusting that.
      const failures = badges.filter((badge) => {
        const large = badge.fontPx >= 24;
        return badge.ratio < (large ? 3 : 4.5);
      });
      if (failures.length > 0) {
        throw new Error(
          `FALSIFIED: evidence badge labels below WCAG AA: ${failures
            .map((f) => `${f.state} "${f.text}" ${f.ratio}:1 at ${f.fontPx}px`)
            .join(', ')}`,
        );
      }
      // A ratio is only meaningful if there is text to read. A badge that lost
      // its label would carry its state in colour alone and still score well.
      const wordless = badges.filter((badge) => badge.text === '');
      if (wordless.length > 0) {
        throw new Error(
          `FALSIFIED: these badges carry no text, so their state is colour-only: ${wordless
            .map((badge) => badge.state)
            .join(', ')}`,
        );
      }
      // The report says which families it never reached. A populated report
      // that dropped them would read as a clean bill of health for all seven.
      await expectVisibleText(
        page,
        'five of seven finding families were consulted',
        "the report's own coverage statement",
      );
    },
  },
  {
    id: 'observatory-doctor-read-only-scope',
    // A deep link into `hermes`, which the fixture registry does list but does
    // not name active (`active_project_id` is `tracedecay`), so the scope
    // resolves to `selected` and the gateway would serve it read-only. The
    // reconciliation runs against the real `/api/projects` payload rather than
    // an override, so the state under audit is one the product can reach.
    //
    // The label in the link is deliberately not what the registry calls this
    // project. Reconciliation has to replace it, and every sentence naming the
    // scope has to use the registry's name — a link that could choose the name
    // shown beside a project's settings and diagnostics is a spoofing surface,
    // and the whole point of resolving the label is that it stops being one.
    route: '/observatory?scope=hermes&scopeLabel=tracedecay%20(production)',
    proves:
      'a read-only scope shows the diagnosis with its remediation controls disabled, the reason stays legible rather than being carried by the greying alone, and the scope is named by the registry rather than by the link',
    overrides: {},
    matrix: true,
    assert: async (page) => {
      // The finding is still on screen: a scope that refuses writes is not a
      // reason to withhold what Doctor observed.
      await expectVisibleText(
        page,
        'five of seven finding families were consulted',
        "the report's coverage statement under a read-only scope",
      );

      const notes = (await page.evaluate(SCOPE_NOTE_PROBE)) as ScopeNote[];
      if (notes.length === 0) {
        throw new Error(
          'FALSIFIED: a read-only scope rendered no scope sentence, so every disabled control on this page is unexplained',
        );
      }
      const wrongState = notes.filter((note) => note.state !== 'read_only');
      if (wrongState.length > 0) {
        throw new Error(
          `FALSIFIED: a project the registry does not name active must read as read_only, not ${wrongState
            .map((note) => note.state)
            .join(', ')}`,
        );
      }
      // The remedy has to be in the words. A disabled control whose sentence
      // does not say how to enable it is indistinguishable from a broken one.
      const silent = notes.filter(
        (note) => !note.text.includes('Switch scope to the active project'),
      );
      if (silent.length > 0) {
        throw new Error(
          `FALSIFIED: the refusal names no remedy: ${silent.map((n) => n.text).join(' | ')}`,
        );
      }
      console.log(
        `[axe] doctor read-only scope notes: ${notes
          .map((note) => `${note.ratio}:1 at ${note.fontPx}px, disabled=${note.disabledControls}`)
          .join('; ')}`,
      );
      const dim = notes.filter((note) => note.ratio < (note.fontPx >= 24 ? 3 : 4.5));
      if (dim.length > 0) {
        throw new Error(
          `FALSIFIED: the reason a control is disabled is itself below WCAG AA: ${dim
            .map((note) => `${note.ratio}:1 at ${note.fontPx}px`)
            .join(', ')}`,
        );
      }
      // The sentence is worthless if the control it explains is still live.
      if (notes.every((note) => note.disabledControls === 0)) {
        throw new Error(
          'FALSIFIED: the page states the scope is read-only while offering every remediation control, so it invites a write the gateway will refuse',
        );
      }

      // The link claimed this project is called `tracedecay (production)`. The
      // registry calls it `hermes`. Nothing on the page may repeat the claim.
      const spoofed = notes.filter((note) => note.text.includes('tracedecay (production)'));
      if (spoofed.length > 0) {
        throw new Error(
          `FALSIFIED: the scope sentence names the project by the link's claim rather than the registry's entry: ${spoofed
            .map((note) => note.text)
            .join(' | ')}`,
        );
      }
      const named = notes.filter((note) => note.text.includes('hermes is not the active project'));
      if (named.length === 0) {
        throw new Error(
          `FALSIFIED: no scope sentence names the project as the registry does, so the correction cannot be observed: ${notes
            .map((note) => note.text)
            .join(' | ')}`,
        );
      }
      const label = (await page.evaluate(SCOPE_LABEL_PROBE)) as ScopeLabel;
      console.log(
        `[axe] reconciled scope label: "${label.text}" annotation=${label.annotation ?? 'none'} at ${label.ratio}:1/${label.fontPx}px`,
      );
      if (label.text.includes('tracedecay (production)')) {
        throw new Error(
          `FALSIFIED: the scope bar still displays the link's claimed label: "${label.text}"`,
        );
      }
      if (!label.text.includes('hermes')) {
        throw new Error(
          `FALSIFIED: the scope bar does not display the registry's label for this project: "${label.text}"`,
        );
      }
      // Listed in the registry, so there is nothing left to qualify. An
      // annotation here would mark a confirmed name as provisional.
      if (label.annotation !== null) {
        throw new Error(
          `FALSIFIED: a registry-confirmed label is still annotated "${label.annotation}", which presents a settled name as unverified`,
        );
      }
      if (label.ratio < (label.fontPx >= 24 ? 3 : 4.5)) {
        throw new Error(
          `FALSIFIED: the scope label naming what every control on this page acts on is below WCAG AA: ${label.ratio}:1 at ${label.fontPx}px`,
        );
      }
    },
  },
  {
    id: 'observatory-canonical',
    route: '/observatory',
    proves:
      'a measured zero and a missing measurement sit side by side on the same panel and stay distinguishable',
    overrides: {},
    assert: async (page) => {
      await assertMetricPlateTruth(page, 'the canonical observations panel');
      // A rate the projector measured as exactly zero. It is a reading, so it
      // prints as one — the em dash belongs to the plate three tiles over,
      // whose value does not exist.
      expectEqual(
        (await plateReading(page, 'feedback_denial_rate')).figure,
        '0',
        'a measured zero rate',
      );
      expectEqual(
        (await plateReading(page, 'feedback_denial_rate')).available,
        'true',
        'the measured zero is marked available',
      );
      const missing = await plateReading(page, 'feedback_revocation_propagation_p95');
      expectEqual(missing.figure, '—', 'a measurement that does not exist');
      expectEqual(missing.available, 'false', 'the missing measurement is marked unavailable');
      await expectVisibleText(page, 'no_revocation_observations', "the projector's own reason");
      // The requirement the plate exists for: the figure, the unit that scales
      // it and the population it is over must all be announced together.
      await assertMeasurementIsSelfDescribing(page, 'feedback_coverage', {
        figure: '91.27',
        unit: '%',
        denominator: 'per eligible observations · 1,884',
      });
      await assertMeasurementIsSelfDescribing(page, 'feedback_latency_p95', {
        figure: '214.8',
        unit: 'ms',
        denominator: 'per latency samples · 1,884',
      });
      await expectVisibleText(page, 'incomplete_metric_coverage', 'the omission reason, verbatim');
    },
  },
  {
    id: 'observatory-source-unreadable',
    route: '/observatory',
    proves:
      'a whole producing source that returned no measurement reads as zero-of-three measured, never as three zeroes',
    overrides: {
      [OBSERVATORY]: {
        status: 200,
        body: envelopeFixture(OBSERVATORY, (envelope, payload) => {
          const metrics = payload['metrics'] as Record<string, unknown>[];
          payload['metrics'] = metrics.map((metric) =>
            (metric['provenance'] as Record<string, unknown>)['source'] === 'observability_envelope'
              ? withoutValue(metric, 'the observability envelope store could not be opened')
              : metric,
          );
          envelope['domain_state'] = 'partial';
        }),
      },
    },
    assert: async (page) => {
      await assertMetricPlateTruth(page, 'an unreadable producing source');
      const tally = await page.evaluate(() => {
        const group = document.querySelector('[data-metric-source="observability_envelope"]');
        return Array.from(group?.querySelectorAll('span') ?? [])
          .map((span) => (span.textContent ?? '').trim())
          .find((text) => /^\d+ of \d+ measured$/.test(text));
      });
      expectEqual(tally, '0 of 3 measured', 'the unreadable source tally');
      await expectVisibleText(
        page,
        'the observability envelope store could not be opened',
        'the failure reason on every plate of the failed source',
      );
    },
  },
  {
    id: 'observatory-redacted',
    route: '/observatory',
    proves:
      'THE AUTHORIZATION AXIS — a read that is complete and separately redacted shows both, not one collapsed into the other',
    overrides: {
      [OBSERVATORY]: {
        status: 200,
        body: envelopeFixture(OBSERVATORY, (envelope) => {
          envelope['authorization'] = { outcome: 'redacted' };
          // Deliberately `ready`: the domain state and the authorization
          // outcome are independent axes, and the bug this guards against is a
          // surface that shows one chip and drops whichever axis it did not
          // pick.
          envelope['domain_state'] = 'ready';
        }),
      },
    },
    assert: async (page) => {
      const states = await page.evaluate(() =>
        Array.from(document.querySelectorAll('[data-state]')).map((chip) => ({
          state: chip.getAttribute('data-state') ?? '',
          text: (chip.textContent ?? '').replace(/\s+/g, ' ').trim(),
        })),
      );
      const redacted = states.find((chip) => chip.state === 'redacted');
      if (redacted === undefined) {
        throw new Error(
          `FALSIFIED: a redacted read rendered no redacted chip. Chips on the page: ${JSON.stringify(states.map((chip) => chip.state))}`,
        );
      }
      expectContains(redacted.text, 'read authorization', 'the redacted chip names its axis');
      if (!states.some((chip) => chip.state === 'ready')) {
        throw new Error(
          'FALSIFIED: the redaction replaced the domain state instead of sitting beside it',
        );
      }
      // Redaction is not a reason to stop reporting what WAS returned.
      await assertMetricPlateTruth(page, 'a redacted read');
    },
  },
  {
    id: 'observatory-no-metrics',
    route: '/observatory',
    proves: 'a read model carrying no measurements says so, and does not render a panel of zeroes',
    overrides: {
      [OBSERVATORY]: {
        status: 200,
        body: envelopeFixture(OBSERVATORY, (envelope, payload) => {
          payload['metrics'] = [];
          envelope['domain_state'] = 'complete_zero_findings';
        }),
      },
    },
    assert: async (page) => {
      await expectVisibleText(
        page,
        'this is a payload with no metrics, not a set of zeroes',
        'the empty read-model sentence',
      );
      await expectAbsent(page, '[data-metric]', 'no metric plates behind an empty read model');
    },
  },
];
