import { hopped, relayed, relayedMissing, renamed } from "./hops";

export function consumeHops(): void {
  hopped();
  renamed();
  relayed();
  relayedMissing();
}
