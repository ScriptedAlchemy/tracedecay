import type { z } from 'zod';

/**
 * A parser for one wire payload, as the fetch seams accept it.
 *
 * The input side is `unknown` on purpose. Every caller hands these a decoded
 * HTTP body, which is untyped by definition — and `z.ZodType<T>` defaults its
 * input parameter to `T`, which quietly demands that the schema already accept
 * a well-formed value. Generated contracts are built from `z.lazy` references,
 * whose input type degrades to `unknown` under that default, so any generated
 * schema with a nested `$ref` fails to satisfy `z.ZodType<T>` even though it
 * parses that exact payload correctly at runtime.
 *
 * Constraining only the output keeps the type that matters — what the surface
 * receives — and stops a schema being rejected for the shape of the bytes it
 * was always going to be handed.
 */
export type WireSchema<T> = z.ZodType<T, z.ZodTypeDef, unknown>;
