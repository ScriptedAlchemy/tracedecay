import type { z } from 'zod';

/** Wire parsers accept an unknown HTTP body and constrain the decoded output. */
export type WireSchema<T> = z.ZodType<T, unknown>;
