import { normalizeSku, type PriceLine } from "./pricing";

export const MAX_CART_ITEMS = 25;

export interface CartItem extends PriceLine {
  addedAt: string;
}

export function addItem(cart: CartItem[], incoming: PriceLine, now = new Date()): CartItem[] {
  if (cart.length >= MAX_CART_ITEMS) {
    throw new Error(`Cart cannot contain more than ${MAX_CART_ITEMS} items`);
  }
  const sku = normalizeSku(incoming.sku);
  const existing = cart.find((item) => item.sku === sku);
  if (existing) {
    return cart.map((item) => item.sku === sku ? { ...item, quantity: item.quantity + incoming.quantity } : item);
  }
  return [...cart, { ...incoming, sku, addedAt: now.toISOString() }];
}

export function removeItem(cart: CartItem[], sku: string): CartItem[] {
  const normalized = normalizeSku(sku);
  return cart.filter((item) => item.sku !== normalized);
}
