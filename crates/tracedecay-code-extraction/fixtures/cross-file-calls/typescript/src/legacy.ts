export function normalize(text: string): string {
  return text.toLowerCase();
}

export function oldFormat(value: string): string {
  return "<" + normalize(value) + ">";
}
