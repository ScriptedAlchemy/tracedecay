import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type {
  DashboardEnvelopeV1,
  LcmTimelineBucketV1,
  LcmTimelinePayloadV1,
  LcmTokenCountProvenanceV1,
} from '../../contracts/generated.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { VolumeTimeline, type VolumeTimelineProps } from './VolumeTimeline.tsx';

type TimelineEnvelope = DashboardEnvelopeV1<LcmTimelinePayloadV1>;

function envelope(payload: LcmTimelinePayloadV1): TimelineEnvelope {
  return fixtureEnvelope(payload) as unknown as TimelineEnvelope;
}

function bucketRow(
  bucket: string,
  count: number,
  known: number,
  tokenCount: number | null,
  provenance: LcmTokenCountProvenanceV1 = 'o200k_approximate',
): LcmTimelineBucketV1 {
  return {
    bucket,
    count,
    known_message_count: known,
    unknown_message_count: count - known,
    token_count: tokenCount,
    token_count_provenance: provenance,
  };
}

function payload(overrides: Partial<LcmTimelinePayloadV1> = {}): LcmTimelinePayloadV1 {
  const buckets = overrides.buckets ?? [];
  return {
    bucket: 'day',
    buckets,
    coverage: {
      limit: 400,
      next_before_bucket: null,
      ordering: 'ascending',
      returned_buckets: buckets.length,
      total_dated_buckets: buckets.length,
      truncated: false,
    },
    exists: true,
    node_buckets: [],
    path: '/tmp/lcm.sqlite',
    session_id: null,
    storage_scope: 'project',
    undated: {
      count: 0,
      known_message_count: 0,
      unknown_message_count: 0,
      token_count: null,
      token_count_provenance: 'unavailable',
    },
    ...overrides,
  };
}

const THREE_DAYS: LcmTimelineBucketV1[] = [
  bucketRow('2026-01-03', 12, 12, 4_100),
  bucketRow('2026-01-04', 7, 5, 2_500),
  bucketRow('2026-01-05', 3, 0, null, 'unavailable'),
];

function renderTimeline(overrides: Partial<VolumeTimelineProps> = {}) {
  const props: VolumeTimelineProps = {
    read: { kind: 'ready', value: envelope(payload({ buckets: THREE_DAYS })) },
    bucket: 'day',
    window: 400,
    onBucketChange: vi.fn(),
    onWindowChange: vi.fn(),
    inspectedBucket: null,
    onInspectBucket: vi.fn(),
    ...overrides,
  };
  const view = render(<VolumeTimeline {...props} />);
  return { ...view, props };
}

describe('VolumeTimeline', () => {
  it('renders one column group per bucket and operable bucket/window radiogroups', () => {
    const { container, props } = renderTimeline();

    expect(container.querySelectorAll('svg [data-bucket]')).toHaveLength(3);

    const bucketGroup = screen.getByRole('radiogroup', { name: 'Bucket' });
    const daily = within(bucketGroup).getByRole('radio', { name: 'DAILY' });
    const hourly = within(bucketGroup).getByRole('radio', { name: 'HOURLY' });
    expect(daily.getAttribute('aria-checked')).toBe('true');
    expect(hourly.getAttribute('aria-checked')).toBe('false');
    fireEvent.click(hourly);
    expect(props.onBucketChange).toHaveBeenCalledWith('hour');

    const windowGroup = screen.getByRole('radiogroup', { name: 'Dated buckets loaded' });
    const radios = within(windowGroup).getAllByRole('radio');
    expect(radios.map((radio) => radio.textContent)).toEqual(['30', '90', '400', '2000']);
    expect(within(windowGroup).getByRole('radio', { name: '400' }).getAttribute('aria-checked')).toBe(
      'true',
    );
    fireEvent.click(within(windowGroup).getByRole('radio', { name: '2000' }));
    expect(props.onWindowChange).toHaveBeenCalledWith(2000);
  });

  it('walks buckets with the keyboard and reports each inspected key', () => {
    const { props } = renderTimeline();
    const field = screen.getByRole('group', { name: 'Message volume field' });
    const status = screen.getByRole('status');

    fireEvent.focus(field);
    expect(props.onInspectBucket).toHaveBeenLastCalledWith('2026-01-05');

    fireEvent.keyDown(field, { key: 'ArrowLeft' });
    expect(status.textContent).toContain('2026-01-04');
    expect(props.onInspectBucket).toHaveBeenLastCalledWith('2026-01-04');

    fireEvent.keyDown(field, { key: 'Escape' });
    expect(status.textContent).toContain('3 of 3 dated days loaded');
    expect(status.textContent).toContain('newest 2026-01-05');
    expect(props.onInspectBucket).toHaveBeenLastCalledWith(null);
  });

  it('prints token provenance and the unknown clause only when it applies', () => {
    renderTimeline();
    const field = screen.getByRole('group', { name: 'Message volume field' });
    const status = screen.getByRole('status');

    fireEvent.focus(field);
    fireEvent.keyDown(field, { key: 'Home' });
    expect(status.textContent).toBe('2026-01-03 · 12 messages · ~4,100 tokens · o200k approximate');

    fireEvent.keyDown(field, { key: 'ArrowRight' });
    expect(status.textContent).toBe(
      '2026-01-04 · 7 messages · ~2,500 tokens · o200k approximate · 2 unknown token counts',
    );

    fireEvent.keyDown(field, { key: 'End' });
    expect(status.textContent).toBe(
      '2026-01-05 · 3 messages · token count unavailable · 3 unknown token counts',
    );
  });

  it('holds undated messages in the footer and out of the dated total', () => {
    renderTimeline({
      read: {
        kind: 'ready',
        value: envelope(
          payload({
            buckets: THREE_DAYS,
            undated: {
              count: 41,
              known_message_count: 40,
              unknown_message_count: 1,
              token_count: 900,
              token_count_provenance: 'o200k_approximate',
            },
          }),
        ),
      },
    });

    expect(
      screen.getByText('41 undated messages are held separately from this field'),
    ).toBeTruthy();
    expect(
      screen.getByRole('img', { name: 'Message volume over 3 dated days, 22 messages' }),
    ).toBeTruthy();
  });

  it('renders three distinct chips for a missing store, an empty window, and undated-only', () => {
    const missing = renderTimeline({
      read: { kind: 'ready', value: envelope(payload({ exists: false })) },
    });
    expect(missing.container.querySelector('[data-state="unknown"]')).not.toBeNull();
    expect(screen.getByText(/LCM session store is unavailable/)).toBeTruthy();
    missing.unmount();

    const empty = renderTimeline({ read: { kind: 'ready', value: envelope(payload()) } });
    expect(empty.container.querySelector('[data-state="complete_zero_findings"]')).not.toBeNull();
    expect(screen.getByText(/no dated messages in the loaded window/)).toBeTruthy();
    empty.unmount();

    const undatedOnly = renderTimeline({
      read: {
        kind: 'ready',
        value: envelope(
          payload({
            undated: {
              count: 9,
              known_message_count: 9,
              unknown_message_count: 0,
              token_count: 120,
              token_count_provenance: 'o200k_approximate',
            },
          }),
        ),
      },
    });
    expect(undatedOnly.container.querySelector('[data-state="partial"]')).not.toBeNull();
    expect(screen.getByText(/9 undated messages only; nothing can be placed on the axis/)).toBeTruthy();
  });

  it('renders a blocked read with its detail and keeps the bucket controls mounted', () => {
    renderTimeline({
      read: { kind: 'blocked', state: 'unknown', detail: 'lcm_temporal_retrieval_not_mounted' },
    });

    expect(screen.getByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    const bucketGroup = screen.getByRole('radiogroup', { name: 'Bucket' });
    expect(within(bucketGroup).getByRole('radio', { name: 'DAILY' })).toBeTruthy();
    expect(within(bucketGroup).getByRole('radio', { name: 'HOURLY' })).toBeTruthy();
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('marks the inspected bucket when it is among the loaded keys', () => {
    const { container } = renderTimeline({ inspectedBucket: '2026-01-04' });
    expect(container.querySelector('[data-inspected-bucket="2026-01-04"]')).not.toBeNull();
  });

  it('mounts the exact-bucket table only while the details element is open', () => {
    const { container } = renderTimeline();
    const details = container.querySelector('details');
    expect(details).not.toBeNull();
    expect(screen.getByText('Exact buckets (3)')).toBeTruthy();
    expect(container.querySelector('table')).toBeNull();

    details!.open = true;
    fireEvent(details!, new Event('toggle'));

    const table = container.querySelector('table');
    expect(table).not.toBeNull();
    expect(table!.querySelectorAll('tbody tr')).toHaveLength(3);
    expect(table!.querySelectorAll('th[scope="col"]')).toHaveLength(5);
  });
});
