import type { MiddlewareHandler } from 'hono';
import { fetchData } from './data-fetch';

export const dataFetchServerMiddleware: MiddlewareHandler = async (c, next) => {
  const id = c.req.query('id');
  if (!id) {
    await next();
    return;
  }
  const data = await fetchData({ id, routeParams: c.req.param() });
  c.set('dataFetchResult', data);
  await next();
};
