export const FREE_SHIPPING_THRESHOLD = 7500;
export const SALES_TAX_RATE = 0.08;

export interface CustomerProfile {
  id: string;
  loyaltyTier: "standard" | "silver" | "gold";
}

export interface PriceLine {
  sku: string;
  quantity: number;
  unitPriceCents: number;
}

export function normalizeSku(sku: string): string {
  return sku.trim().toUpperCase();
}

export function applyLoyaltyDiscount(subtotalCents: number, profile: CustomerProfile): number {
  if (profile.loyaltyTier === "gold") return Math.round(subtotalCents * 0.9);
  if (profile.loyaltyTier === "silver") return Math.round(subtotalCents * 0.95);
  return subtotalCents;
}

export function calculateSubtotal(lines: PriceLine[]): number {
  return lines.reduce((sum, line) => sum + line.quantity * line.unitPriceCents, 0);
}

export function calculateTax(totalCents: number): number {
  return Math.round(totalCents * SALES_TAX_RATE);
}

export function qualifiesForFreeShipping(totalCents: number): boolean {
  return totalCents >= FREE_SHIPPING_THRESHOLD;
}
