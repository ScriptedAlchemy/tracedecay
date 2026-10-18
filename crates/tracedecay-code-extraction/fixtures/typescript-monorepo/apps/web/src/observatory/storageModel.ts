export function formatBytes(bytes: number): string {
  return `${bytes} B`;
}

export function storageFindingLabel(kind: string): string {
  return kind.toUpperCase();
}

export function StoreBadge(props: { label: string }): string {
  return props.label;
}
