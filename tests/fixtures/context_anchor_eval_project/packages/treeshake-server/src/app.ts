import { Hono } from 'hono';
import { healthRoutes } from './http/routes/health';
import { treeshakeRoutes } from './http/routes/treeshake';
import { requestLogger } from './http/middlewares/logger';
import { requestId } from './http/middlewares/request-id';

export function createApp(): Hono {
  const app = new Hono();
  app.use('*', requestId);
  app.use('*', requestLogger);
  app.route('/health', healthRoutes);
  app.route('/treeshake', treeshakeRoutes);
  return app;
}
