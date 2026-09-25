import type { MiddlewareHandler } from 'hono';

export const requestLogger: MiddlewareHandler = async (c, next) => {
  const started = Date.now();
  await next();
  console.log(`${c.req.method} ${c.req.path} ${Date.now() - started}ms`);
};
