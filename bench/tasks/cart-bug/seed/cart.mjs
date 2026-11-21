export function total(items) { return Math.round(items.reduce((sum, item) => sum + item.price, 0)); }
