import type { CartItem } from "./cart";

export interface Reservation {
  sku: string;
  quantity: number;
  reservationId: string;
}

export function reserveInventory(items: CartItem[]): Reservation[] {
  return items.map((item) => ({
    sku: item.sku,
    quantity: item.quantity,
    reservationId: `reserve_${item.sku}_${item.quantity}`,
  }));
}
