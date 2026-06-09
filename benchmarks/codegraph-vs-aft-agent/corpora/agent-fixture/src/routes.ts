import { addItem, removeItem } from "./cart";
import { submitOrder } from "./checkout";

export const routes = {
  "POST /cart/items": addItem,
  "DELETE /cart/items/:sku": removeItem,
  "POST /checkout": submitOrder,
};
