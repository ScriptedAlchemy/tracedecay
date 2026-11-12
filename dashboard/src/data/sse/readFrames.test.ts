import { describe, expect, it } from 'vitest';
import { readSseFrames } from './readFrames.ts';

function response(chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });
}

describe('readSseFrames', () => {
  it('preserves UTF-8 and multiline data across every byte boundary', async () => {
    const bytes = new TextEncoder().encode(
      '\uFEFF: heartbeat\r\n\r\nevent: source\r\ndata: café\r\ndata: 二\r\nid: 4\r\nretry: 5\r\n\r\n',
    );
    const frames = [];
    for await (const frame of readSseFrames(response(Array.from(bytes, (byte) => Uint8Array.of(byte))))) {
      frames.push(frame);
    }
    expect(frames).toEqual([
      { type: 'message', data: '', hasData: false, retryMs: 5 },
      { type: 'source', data: 'café\n二', hasData: true, id: '4' },
    ]);
  });

  it('ignores comments and unknown fields and preserves an explicitly empty data event', async () => {
    const frames = [];
    for await (const frame of readSseFrames(response([
      new TextEncoder().encode(': heartbeat\nunknown: ignored\nretry: invalid\ndata:\n\n'),
    ]))) {
      frames.push(frame);
    }
    expect(frames).toEqual([{ type: 'message', data: '', hasData: true, id: undefined }]);
  });

  it('never dispatches an unterminated event at end of response', async () => {
    const frames = [];
    for await (const frame of readSseFrames(response([
      new TextEncoder().encode('data: incomplete\n'),
    ]))) {
      frames.push(frame);
    }
    expect(frames).toEqual([]);
  });

  it('cancels the unfinished response when its consumer stops', async () => {
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('data: first\n\n'));
      },
      cancel() {
        cancelled = true;
      },
    });
    for await (const frame of readSseFrames(body)) {
      expect(frame.data).toBe('first');
      break;
    }
    expect(cancelled).toBe(true);
    expect(body.locked).toBe(false);
  });

  it('rejects invalid UTF-8 instead of fabricating replacement characters', async () => {
    const frames = readSseFrames(response([Uint8Array.of(0xff)]));
    await expect(frames.next()).rejects.toThrow();
  });
});
