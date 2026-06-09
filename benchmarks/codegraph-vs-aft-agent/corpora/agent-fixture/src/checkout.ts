import type { CartItem } from "./cart";
import { reserveInventory } from "./inventory";
import { chargePayment } from "./payments";
import { applyLoyaltyDiscount, calculateSubtotal, calculateTax, qualifiesForFreeShipping, type CustomerProfile } from "./pricing";

export interface CheckoutRequest {
  orderId: string;
  customer: CustomerProfile;
  paymentToken: string;
  items: CartItem[];
}

export interface CheckoutResult {
  orderId: string;
  status: "pending" | "paid";
  chargeId: string;
  totalCents: number;
  shippingCents: number;
}

export async function submitOrder(request: CheckoutRequest): Promise<CheckoutResult> {
  const subtotal = calculateSubtotal(request.items);
  const discounted = applyLoyaltyDiscount(subtotal, request.customer);
  const tax = calculateTax(discounted);
  const shippingCents = qualifiesForFreeShipping(discounted) ? 0 : 499;
  const totalCents = discounted + tax + shippingCents;
  reserveInventory(request.items);
  const chargeId = await chargePayment({
    orderId: request.orderId,
    totalCents,
    token: request.paymentToken,
  });
  return {
    orderId: request.orderId,
    status: "paid",
    chargeId,
    totalCents,
    shippingCents,
  };
}
