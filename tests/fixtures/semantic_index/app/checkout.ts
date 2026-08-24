// Storefront checkout client for the demo fixture codebase.

export interface CartLine {
  sku: string;
  quantity: number;
}

export interface CheckoutQuote {
  totalCents: number;
  reservedSkus: string[];
}

/** Sum cart quantities so the backend discount tier can be previewed. */
export function countCartUnits(lines: CartLine[]): number {
  return lines.reduce((total, line) => total + line.quantity, 0);
}

/** Render a quote returned by the storefront backend. */
export function formatCheckoutQuote(quote: CheckoutQuote): string {
  const dollars = (quote.totalCents / 100).toFixed(2);
  return `${quote.reservedSkus.length} SKUs reserved, total $${dollars}`;
}
