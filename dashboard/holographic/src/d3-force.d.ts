/**
 * Local type surface for `d3-force`.
 *
 * `d3-force` ships no type declarations and `@types/d3-force` is intentionally
 * not a dashboard dependency, so without this ambient declaration the
 * `SimulationNodeDatum`/`SimulationLinkDatum` bases that `SimNode`/`SimLink`
 * extend (see `./associationGraphTypes`) resolve to nothing and the layout
 * mutators (`node.x`, `node.vx`, `link.source`, ...) lose their fields. Only the
 * node/link datums are typed precisely — those are what the holographic renderer
 * reads and mutates. The simulation *drivers* (force builders / `Simulation`)
 * are declared permissively: they are exercised only in
 * `./associationGraphLayout`, are untyped at runtime, and keeping them loose
 * avoids second-guessing d3's fluent, heavily-generic builder chain. This file
 * is type-only and adds no runtime code.
 */
declare module "d3-force" {
  export interface SimulationNodeDatum {
    index?: number;
    x?: number;
    y?: number;
    vx?: number;
    vy?: number;
    fx?: number | null;
    fy?: number | null;
  }

  export interface SimulationLinkDatum<NodeDatum extends SimulationNodeDatum> {
    source: NodeDatum | string | number;
    target: NodeDatum | string | number;
    index?: number;
  }

  export type Simulation<NodeDatum, LinkDatum> = any;

  export function forceSimulation<N extends SimulationNodeDatum>(
    nodes?: N[],
  ): Simulation<N, unknown>;
  export function forceManyBody<N extends SimulationNodeDatum>(): any;
  export function forceLink<
    NodeDatum extends SimulationNodeDatum,
    LinkDatum extends SimulationLinkDatum<NodeDatum>,
  >(links?: LinkDatum[]): any;
  export function forceCenter(x?: number, y?: number): any;
  export function forceCollide<N extends SimulationNodeDatum>(): any;
  export function forceX<N extends SimulationNodeDatum>(x?: number): any;
  export function forceY<N extends SimulationNodeDatum>(y?: number): any;
}
