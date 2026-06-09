import { withRetry } from "./retry";

export interface PaymentRequest {
  orderId: string;
  totalCents: number;
  token: string;
}

export async function chargePayment(request: PaymentRequest): Promise<string> {
  return withRetry(async () => {
    if (!request.token) throw new Error("missing payment token");
    return `charge_${request.orderId}_${request.totalCents}`;
  }, { attempts: 3, delayMs: 25 });
}
