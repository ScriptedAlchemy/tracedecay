import { AuthenticatedEventSource } from './authenticatedEventSource.ts';
import { adoptLaunchCredential } from './credential.ts';

/** Page-start wiring: adopt the launch credential, then make every
 * `new EventSource(url)` in the app an authenticated, fetch-based stream.
 * Installing it on the global keeps the stream's consumers on the standard
 * `EventSource` surface instead of threading a constructor through them. */
export function installDashboardAuthentication(): void {
  adoptLaunchCredential();
  globalThis.EventSource = AuthenticatedEventSource as unknown as typeof EventSource;
}
