import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type {
  DashboardEnvelopeV1,
  LoomSessionRowV1,
  LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import type { ReadState } from '../../ui/ReadSection.tsx';
import { bucketKeyFor } from './model.ts';
import { SessionIndex, type SessionIndexProps } from './SessionIndex.tsx';

type Envelope = DashboardEnvelopeV1<LoomTemporalPayloadV1>;

const START = 1_746_782_220;

function row(over: Partial<LoomSessionRowV1> = {}): LoomSessionRowV1 {
  return {
    session_id: 'rs_alpha',
    provider: 'codex',
    title: 'Alpha title',
    started_at: START,
    ended_at: START + 3_600,
    last_message_at: START + 3_500,
    messages: 40,
    models: [{ model: 'gpt-5' }],
    is_subagent: false,
    edited_files_recorded: true,
    ...over,
  };
}

function payload(over: Partial<LoomTemporalPayloadV1> = {}): LoomTemporalPayloadV1 {
  return {
    available: true,
    sessions: [],
    total: 0,
    commits: [],
    edited_files: [],
    branch_spans: [],
    source_statuses: [],
    temporal_refresh: {
      state: 'ready',
      active_generations: 1,
      latest_activated_at_micros: null,
      authority: 'x',
    },
    ...over,
  };
}

function envelopeFor(value: LoomTemporalPayloadV1): Envelope {
  return fixtureEnvelope(value) as unknown as Envelope;
}

function readyRead(value: LoomTemporalPayloadV1): ReadState<Envelope> {
  return { kind: 'ready', value: envelopeFor(value) };
}

function fullPage(count = 25): LoomSessionRowV1[] {
  return Array.from({ length: count }, (_, index) =>
    row({
      session_id: `rs_${String(index).padStart(3, '0')}`,
      started_at: START - index * 600,
      ended_at: START - index * 600 + 300,
      messages: 10 + index,
    }),
  );
}

function renderIndex(over: Partial<SessionIndexProps> = {}) {
  const props: SessionIndexProps = {
    read: readyRead(payload({ sessions: fullPage(), total: 2_847 })),
    page: 1,
    rows: 25,
    onPageChange: vi.fn(),
    onRowsChange: vi.fn(),
    selection: null,
    onSelect: vi.fn(),
    onInspectRow: vi.fn(),
    inspectedBucket: null,
    bucket: 'day',
    ...over,
  };
  const utils = render(<SessionIndex {...props} />);
  return { ...utils, props };
}

function rowWrapper(sessionId: string): HTMLElement {
  const wrapper = document.querySelector<HTMLElement>(`[data-session-row="${sessionId}"]`);
  if (!wrapper) throw new Error(`no row wrapper for ${sessionId}`);
  return wrapper;
}

function rowButton(sessionId: string): HTMLButtonElement {
  const button = rowWrapper(sessionId).querySelector('button');
  if (!button) throw new Error(`no row button for ${sessionId}`);
  return button;
}

describe('SessionIndex', () => {
  it('renders each cell from the recorded fields and the page bounds', () => {
    const sessions = [
      row({ session_id: 'rs_ended', provider: 'codex', models: [{ model: 'gpt-5' }] }),
      row({
        session_id: 'rs_open',
        provider: 'claude',
        title: null,
        ended_at: null,
        last_message_at: START + 900,
        models: [{ model: null }],
        is_subagent: true,
      }),
      row({
        session_id: 'rs_undated',
        provider: 'cursor',
        started_at: null,
        ended_at: null,
        last_message_at: null,
        models: [{ model: 'sonnet' }, { model: null }],
      }),
      ...fullPage(22),
    ];
    renderIndex({ read: readyRead(payload({ sessions, total: 2_847 })) });

    expect(screen.getByText('1–25 of 2,847')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toBe('Page 1 of 114');

    const ended = rowWrapper('rs_ended');
    expect(ended.textContent).toContain('codex');
    expect(ended.textContent).toContain('gpt-5');
    expect(ended.textContent).toContain('Alpha title');
    expect(ended.textContent).toMatch(/\d{4}-\d{2}-\d{2} \d{2}:\d{2}→ \d{4}-\d{2}-\d{2} \d{2}:\d{2} · ended/);
    // A plain session carries no kind tag; only the recorded subagent flag is worded.
    expect(ended.textContent).not.toContain('subagent');

    const open = rowWrapper('rs_open');
    expect(open.textContent).toContain('claude');
    expect(open.textContent).toContain('model unrecorded');
    expect(open.textContent).toContain('untitled');
    expect(open.textContent).toMatch(/→ open · last \d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    expect(open.textContent).toContain('subagent');

    const undated = rowWrapper('rs_undated');
    expect(undated.textContent).toContain('start unrecorded');
    expect(undated.textContent).toContain('sonnet +1 unrecorded');
  });

  it('selects on click, deselects the selected row, and marks it pressed', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    const { rerender, props } = renderIndex({ onSelect });

    await user.click(rowButton('rs_001'));
    expect(onSelect).toHaveBeenLastCalledWith({ provider: 'codex', sessionId: 'rs_001' });

    rerender(
      <SessionIndex {...props} selection={{ provider: 'codex', sessionId: 'rs_001' }} />,
    );
    expect(rowButton('rs_001').getAttribute('aria-pressed')).toBe('true');
    expect(rowButton('rs_000').getAttribute('aria-pressed')).toBe('false');

    await user.click(rowButton('rs_001'));
    expect(onSelect).toHaveBeenLastCalledWith(null);
  });

  it('reports hover as inspection and dims rows outside the inspected bucket', () => {
    const onInspectRow = vi.fn();
    const sessions = fullPage(3);
    const { rerender, props } = renderIndex({
      onInspectRow,
      read: readyRead(payload({ sessions, total: 3 })),
    });

    fireEvent.mouseEnter(rowWrapper('rs_001'));
    expect(onInspectRow).toHaveBeenLastCalledWith(sessions[1]);
    fireEvent.mouseLeave(rowWrapper('rs_001'));
    expect(onInspectRow).toHaveBeenLastCalledWith(null);
    expect(props.onSelect).not.toHaveBeenCalled();

    const other = bucketKeyFor(START - 30 * 86_400, 'day');
    rerender(<SessionIndex {...props} inspectedBucket={other} />);
    expect(rowWrapper('rs_000').className).toContain('opacity-40');

    rerender(<SessionIndex {...props} inspectedBucket={bucketKeyFor(START, 'day')} />);
    expect(rowWrapper('rs_000').className).not.toContain('opacity-40');
  });

  it('pages with real limit/offset controls', async () => {
    const user = userEvent.setup();
    const onPageChange = vi.fn();
    const onRowsChange = vi.fn();
    renderIndex({ onPageChange, onRowsChange });

    const pager = screen.getByRole('navigation', { name: 'Session pages' });
    expect(pager).toBeTruthy();
    expect((screen.getByRole('button', { name: 'First page' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(
      (screen.getByRole('button', { name: 'Previous page' }) as HTMLButtonElement).disabled,
    ).toBe(true);

    await user.click(screen.getByRole('button', { name: 'Next page' }));
    expect(onPageChange).toHaveBeenLastCalledWith(2);
    await user.click(screen.getByRole('button', { name: 'Last page' }));
    expect(onPageChange).toHaveBeenLastCalledWith(114);

    await user.selectOptions(screen.getByLabelText(/rows per page/i), '100');
    expect(onRowsChange).toHaveBeenLastCalledWith(100);
  });

  it('moves focus between rows with the arrow keys', () => {
    renderIndex();
    const first = rowButton('rs_000');
    first.focus();
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(rowButton('rs_001'));
  });

  it('renders the three empty readings as distinct chips', async () => {
    const user = userEvent.setup();

    const unavailable = renderIndex({
      read: readyRead(payload({ available: false })),
    });
    expect(document.querySelector('[data-state="unknown"]')).toBeTruthy();
    expect(screen.getByText(/reported its session store unavailable/)).toBeTruthy();
    unavailable.unmount();

    const zero = renderIndex({ read: readyRead(payload({ sessions: [], total: 0 })) });
    expect(document.querySelector('[data-state="complete_zero_findings"]')).toBeTruthy();
    zero.unmount();

    const onPageChange = vi.fn();
    renderIndex({
      read: readyRead(payload({ sessions: [], total: 2_847 })),
      page: 200,
      onPageChange,
    });
    const chip = document.querySelector('[data-state="partial"]');
    expect(chip).toBeTruthy();
    expect(chip?.textContent).toContain('page 200 is past the last page (114)');
    await user.click(screen.getByRole('button', { name: 'Go to last page' }));
    expect(onPageChange).toHaveBeenLastCalledWith(114);
  });

  it('renders a blocked read as its chip and keeps the rows-per-page select', () => {
    renderIndex({
      read: { kind: 'blocked', state: 'offline', detail: 'daemon unreachable' },
    });
    expect(screen.getByText('Offline')).toBeTruthy();
    expect(screen.getByText(/daemon unreachable/)).toBeTruthy();
    expect(screen.getByLabelText(/rows per page/i)).toBeTruthy();
    expect(document.querySelector('[data-session-row]')).toBeNull();
  });

  it('prints the envelope omission reasons verbatim', () => {
    const envelope = envelopeFor(payload({ sessions: fullPage(), total: 2_847 }));
    envelope.coverage = {
      ...envelope.coverage,
      completeness: 'partial',
      omitted: 3,
      omission_reasons: ['session_store_page_bounded_by_limit'],
    };
    renderIndex({ read: { kind: 'ready', value: envelope } });
    expect(screen.getByText('session_store_page_bounded_by_limit')).toBeTruthy();
    expect(screen.getByText(/3 omitted/)).toBeTruthy();
  });
});
