export function isExpired(expiresAt, now) { return expiresAt == null ? false : now > expiresAt; }
