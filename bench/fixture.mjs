import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
export async function listen(server) {
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
    return `http://127.0.0.1:${server.address().port}`;
}
export async function fixtureSite() {
    const submissions = {}, steps = [], sessions = new Map();
    const server = createServer(async (req, res) => {
        try {
            const url = new URL(req.url, 'http://fixture.test');
            const match = /^\/(booking|ticket)(?:\/(review|confirm))?$/.exec(url.pathname);
            if (!match) {
                res.writeHead(404);
                res.end();
                return;
            }
            const [, kind, step] = match;
            const send = html => { res.setHeader('Content-Type', 'text/html'); res.end(`<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Local task fixture</title><style>body{font:22px system-ui;margin:40px;max-width:700px}label{display:block;margin:24px 0 6px}input,select,button{font:inherit;padding:12px}button{display:block;margin-top:24px}</style><body>${html}</body></html>`); };
            if (!step && req.method === 'GET') {
                if (sessions.size >= 32) sessions.delete(sessions.keys().next().value);
                const token = randomUUID();
                sessions.set(token, { kind });
                send(`<h1>${kind === 'booking' ? 'Book a room' : 'New ticket'}</h1><form method="POST" action="/${kind}/review"><input type="hidden" name="token" value="${token}"><label for="choice">${kind === 'booking' ? 'Room' : 'Priority'}</label><select id="choice" name="choice">${(kind === 'booking' ? ['Quiet room', 'Team room'] : ['Low', 'High']).map(v => `<option>${v}</option>`).join('')}</select><label for="text">${kind === 'booking' ? 'Purpose' : 'Title'} (required)</label><input id="text" name="text" required maxlength="200"><button type="submit">Continue</button></form>`);
                return;
            }
            if (!step || req.method !== 'POST') {
                res.writeHead(405);
                res.end();
                return;
            }
            let body = '';
            for await (const part of req) {
                body += part;
                if (body.length > 8192)
                    throw Error('Too large');
            }
            const params = new URLSearchParams(body), token = params.get('token'), session = sessions.get(token);
            if (!session || session.kind !== kind) {
                res.writeHead(403);
                res.end();
                return;
            }
            if (step === 'review') {
                const choice = params.get('choice'), text = params.get('text');
                const choices = kind === 'booking' ? ['Quiet room', 'Team room'] : ['Low', 'High'];
                if (!choices.includes(choice) || !text?.trim() || text.length > 200) throw Error('Invalid form fields');
                session.choice = choice;
                session.text = text;
                session.reviewed = true;
                steps.push('review');
                const escape = s => String(s).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('"', '&quot;');
                send(`<h1>Review ${kind}</h1><p>${escape(session.choice)}: ${escape(session.text)}</p><form method="POST" action="/${kind}/confirm"><input type="hidden" name="token" value="${token}"><button type="submit">Confirm</button></form>`);
            }
            else {
                if (!session.reviewed) {
                    res.writeHead(409);
                    res.end();
                    return;
                }
                submissions[kind] = { choice: session.choice, text: session.text, confirmed: true };
                steps.push('confirm');
                sessions.delete(token);
                send('<h1>Confirmed</h1><p>Saved successfully.</p>');
            }
        }
        catch {
            res.writeHead(400);
            res.end('Invalid fixture request');
        }
    });
    const origin = await listen(server);
    return { origin, publicOrigin: `http://job-bench.test:${server.address().port}`, port: server.address().port, submissions, steps, close: async () => { server.closeAllConnections(); await new Promise(r => server.close(r)); } };
}
