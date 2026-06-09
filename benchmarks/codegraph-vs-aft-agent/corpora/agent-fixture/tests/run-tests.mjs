import { readFileSync } from "node:fs";

const cart = readFileSync(new URL("../src/cart.ts", import.meta.url), "utf8");
const pricing = readFileSync(new URL("../src/pricing.ts", import.meta.url), "utf8");
const checkout = readFileSync(new URL("../src/checkout.ts", import.meta.url), "utf8");

if (!/export const MAX_CART_ITEMS = \d+;/.test(cart)) throw new Error("MAX_CART_ITEMS constant missing");
if (!/export const FREE_SHIPPING_THRESHOLD = \d+;/.test(pricing)) throw new Error("FREE_SHIPPING_THRESHOLD constant missing");
if (!/export const SALES_TAX_RATE = [0-9.]+;/.test(pricing)) throw new Error("SALES_TAX_RATE constant missing");
if (!checkout.includes("chargePayment")) throw new Error("submitOrder must charge payment");
console.log("fixture tests passed");
