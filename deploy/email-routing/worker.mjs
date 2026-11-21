const maxBytes = 2 * 1024 * 1024;
const encoder = new TextEncoder();

export default {
  async email(message, env) {
    if (message.rawSize > maxBytes) {
      message.setReject('Email exceeds 2 MiB.');
      return;
    }
    const raw = await new Response(message.raw).arrayBuffer();
    const timestamp = String(Math.floor(Date.now() / 1000));
    const prefix = encoder.encode(`minarbot-email-v1\n${timestamp}\n${message.to}\n`);
    const signed = new Uint8Array(prefix.length + raw.byteLength);
    signed.set(prefix);
    signed.set(new Uint8Array(raw), prefix.length);
    const key = await crypto.subtle.importKey(
      'raw', encoder.encode(env.MAIL_SECRET), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign'],
    );
    const tag = new Uint8Array(await crypto.subtle.sign('HMAC', key, signed));
    const signature = btoa(String.fromCharCode(...tag)).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
    const response = await fetch(env.MAIL_ENDPOINT, {
      method: 'POST',
      redirect: 'error',
      headers: {
        'Content-Type': 'message/rfc822',
        'X-Minarbot-Email-To': message.to,
        'X-Minarbot-Email-Timestamp': timestamp,
        'X-Minarbot-Email-Signature': signature,
      },
      body: raw,
    });
    if (response.status === 204) return;
    if ([400, 404, 413].includes(response.status)) {
      message.setReject(response.status === 404 ? 'Unknown recipient.' : 'Message rejected.');
      return;
    }
    throw new Error(`Minarbot email relay returned HTTP ${response.status}.`);
  },
};
