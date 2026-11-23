import { createPublicKey, sign, verify, type KeyObject } from 'node:crypto';
import { base64url, canonicalJson, closedObject, ProtocolError, publicKey, sha256 } from '../shared/protocol.ts';

// Version 1 signatures must verify across the rename and rollback.
export function signedNodeMessage(body: unknown, key: KeyObject, direction: 'hub' | 'node') {
    const keyId = sha256(createPublicKey(key).export({ type: 'spki', format: 'der' }));
    return { v: 1, keyId, body, signature: sign(null, Buffer.from(`minarbot.${direction}.v1\n${canonicalJson(body)}`), key).toString('base64url') };
}
export function verifiedNodeMessage(value: unknown, key: string, direction: 'hub' | 'node'): Record<string, unknown> {
    const envelope = closedObject(value, ['v', 'keyId', 'body', 'signature']), parsed = publicKey(key, 'ed25519');
    if (envelope.v !== 1 || envelope.keyId !== sha256(parsed.key.export({ type: 'spki', format: 'der' })) || !verify(null, Buffer.from(`minarbot.${direction}.v1\n${canonicalJson(envelope.body)}`), parsed.key, base64url(envelope.signature, 64)))
        throw new ProtocolError(401, 'invalid_signature', 'Invalid node link proof.');
    if (!envelope.body || typeof envelope.body !== 'object' || Array.isArray(envelope.body))
        throw new ProtocolError(400, 'invalid_frame', 'Invalid node link body.');
    return envelope.body as Record<string, unknown>;
}
