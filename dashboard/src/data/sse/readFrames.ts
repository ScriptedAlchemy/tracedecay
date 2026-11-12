import { createParser } from 'eventsource-parser';

export interface SseFrame {
  readonly type: string;
  readonly data: string;
  readonly hasData: boolean;
  readonly id?: string;
  readonly retryMs?: number;
}

/** Parse one response without reconnecting. The caller owns cancellation. */
export async function* readSseFrames(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<SseFrame> {
  const reader = body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const frames: SseFrame[] = [];
  const parser = createParser({
    onEvent: ({ event, data, id }) => {
      frames.push({ type: event || 'message', data, hasData: true, id });
    },
    onRetry: (retryMs) => {
      frames.push({ type: 'message', data: '', hasData: false, retryMs });
    },
  });
  try {
    for (;;) {
      const { done, value } = await reader.read();
      parser.feed(decoder.decode(value, { stream: !done }));
      for (const frame of frames.splice(0)) yield frame;
      if (done) return;
    }
  } finally {
    try {
      await reader.cancel();
    } finally {
      reader.releaseLock();
    }
  }
}
