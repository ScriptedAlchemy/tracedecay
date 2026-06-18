export function cn(...args: unknown[]): string {
  const out: string[] = [];
  const visit = (value: unknown): void => {
    if (Array.isArray(value)) {
      for (const v of value) visit(v);
    } else if (typeof value === "string" && value.length > 0) {
      out.push(value);
    }
  };
  for (const a of args) visit(a);
  return out.join(" ");
}
