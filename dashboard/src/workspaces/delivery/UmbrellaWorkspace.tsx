import type { DeliveryContext } from './DeliveryPage.tsx';

export function UmbrellaWorkspace({ context }: { context: DeliveryContext }) {
  return <div data-stub="umbrella">{context.umbrellas.umbrellas.length}</div>;
}
