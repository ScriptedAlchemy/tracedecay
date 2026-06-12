import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import type { HolographicGraphNode } from "./types";
import { KIND_ORDER } from "./associationGraphTypes";
import { colorOf, displayEntityType } from "./associationGraphUtils";
import type { Neighbor } from "./associationGraphTypes";
import { NUM_BADGE } from "./ui";

export function NodeDetailPanel({
  selected,
  degreeMap,
  groupedNeighbors,
  selectedNeighbors,
  factNeighborCount,
  onSelect,
  onClear,
  onFrame,
}: {
  selected: HolographicGraphNode;
  degreeMap: Map<string, number>;
  groupedNeighbors: Record<string, Neighbor[]>;
  selectedNeighbors: Neighbor[];
  factNeighborCount: number;
  onSelect: (id: string) => void;
  onClear: () => void;
  onFrame: () => void;
}) {
  return (
    <Card className="flex flex-col overflow-hidden lg:sticky lg:top-4 lg:max-h-[calc(100vh-2rem)] lg:self-start">
      <CardHeader className="shrink-0">
        <CardTitle>Node</CardTitle>
      </CardHeader>
      <CardContent className="min-h-0 flex-1 overflow-y-auto">
        <div className="flex flex-col gap-3">
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span
                  className="inline-block h-2.5 w-2.5 shrink-0 rounded-full"
                  style={{ backgroundColor: colorOf(selected.kind) }}
                />
                <span className="min-w-0 break-words text-sm text-foreground">
                  {selected.label}
                </span>
              </div>
              <p className="mt-1 font-mono-ui text-xs text-text-tertiary">
                {selected.kind} · degree {degreeMap.get(selected.id) ?? 0}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-1">
              <Button
                ghost
                size="xs"
                className="text-muted-foreground hover:text-foreground"
                onClick={onFrame}
                title="Zoom to this node and its connections"
              >
                Frame
              </Button>
              <Button
                ghost
                size="xs"
                className="text-muted-foreground hover:text-foreground"
                onClick={onClear}
              >
                Clear
              </Button>
            </div>
          </div>
          {selected.kind === "fact" && (
            <>
              <div className="flex flex-wrap gap-2">
                {selected.category && (
                  <Badge tone="secondary" className={NUM_BADGE}>
                    {selected.category}
                  </Badge>
                )}
                <Badge tone="outline" className={NUM_BADGE}>
                  trust {Number(selected.trust_score ?? 0).toFixed(2)}
                </Badge>
                <Badge tone="outline" className={NUM_BADGE}>
                  {selected.has_hrr ? "HRR" : "no HRR"}
                </Badge>
              </div>
              <p className="max-h-64 overflow-y-auto whitespace-pre-wrap text-sm leading-relaxed text-foreground">
                {selected.content}
              </p>
            </>
          )}
          {selected.kind === "entity" && (
            <div className="flex flex-wrap gap-2">
              {displayEntityType(selected.entity_type) && (
                <Badge tone="secondary" className={NUM_BADGE}>
                  {displayEntityType(selected.entity_type)}
                </Badge>
              )}
              <Badge tone="outline" className={NUM_BADGE}>
                {factNeighborCount} facts
              </Badge>
            </div>
          )}
          {selected.kind === "bank" && (
            <div className="flex flex-wrap gap-2">
              {selected.category && (
                <Badge tone="secondary" className={NUM_BADGE}>
                  {selected.category}
                </Badge>
              )}
              <Badge tone="outline" className={NUM_BADGE}>
                dim {selected.dim ?? "unknown"}
              </Badge>
              <Badge tone="outline" className={NUM_BADGE}>
                {selected.fact_count ?? factNeighborCount} facts bundled
              </Badge>
            </div>
          )}
          {selected.kind === "category" && (
            <Badge tone="outline" className={NUM_BADGE}>
              {selected.fact_count ?? factNeighborCount} facts
            </Badge>
          )}

          {selectedNeighbors.length > 0 && (
            <div className="flex flex-col gap-3 border-t border-border pt-3">
              <span className="font-mondwest text-display text-xs tracking-[0.12em] text-text-secondary">
                Connections ({selectedNeighbors.length})
              </span>
              {KIND_ORDER.map((kind) => {
                const items = groupedNeighbors[kind];
                if (!items || items.length === 0) return null;
                return (
                  <div key={kind} className="flex flex-col gap-1.5">
                    <div className="flex items-center gap-1.5">
                      <span
                        className="inline-block h-2 w-2 rounded-full"
                        style={{ backgroundColor: colorOf(kind) }}
                      />
                      <span className="font-mono-ui text-xs text-text-tertiary">
                        {kind} · {items.length}
                      </span>
                    </div>
                    <div className="flex flex-col gap-1">
                      {items.slice(0, 14).map((nb) => (
                        <button
                          key={nb.id}
                          type="button"
                          onClick={() => onSelect(nb.id)}
                          className="group flex min-w-0 items-center border border-border bg-background/40 px-2 py-1 text-left hover:border-primary/60"
                        >
                          <span className="min-w-0 flex-1 truncate text-xs text-text-secondary group-hover:text-foreground">
                            {nb.kind === "fact" && nb.content
                              ? `${nb.label}  ${nb.content}`
                              : nb.label}
                          </span>
                        </button>
                      ))}
                      {items.length > 14 && (
                        <span className="px-2 text-xs text-text-tertiary">
                          +{items.length - 14} more
                        </span>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
