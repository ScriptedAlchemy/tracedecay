import { dependencyCount, fixtureGraph } from "./graph";

export interface FixtureReport {
  readonly nodeCount: number;
  readonly edgeCount: number;
}

export function buildFixtureReport(): FixtureReport {
  const nodes = fixtureGraph();
  return {
    nodeCount: nodes.length,
    edgeCount: dependencyCount(nodes),
  };
}
