import { Hono } from 'hono';
import { runTreeshake } from '../../treeshake';

export const treeshakeRoutes = new Hono();

treeshakeRoutes.post('/', async (c) => {
  const body = await c.req.json();
  return c.json(await runTreeshake(body));
});
