import type { MiddlewareHandler } from 'hono';

export const requestId: MiddlewareHandler = async (c, next) => {
  c.set('requestId', crypto.randomUUID());
  await next();
};
