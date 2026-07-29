export interface FixtureNode {
  readonly id: string;
  readonly dependencies: readonly string[];
}

export function fixtureGraph(): readonly FixtureNode[] {
  return [
    { id: "catalog", dependencies: [] },
    { id: "report", dependencies: ["catalog"] },
  ];
}

export function dependencyCount(nodes: readonly FixtureNode[]): number {
  return nodes.reduce((count, node) => count + node.dependencies.length, 0);
}
