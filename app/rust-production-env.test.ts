import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { generateKeyPairSync } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { request } from 'node:https';
import { join, resolve } from 'node:path';
import { test } from 'node:test';
import { rustBinary } from './rust-binary.ts';
import { freePort, stop } from './test-support/test-process.ts';

const root = resolve(import.meta.dirname, '..');

test('Rust runtime source contains no unfinished port markers', () => {
  const source = join(root, 'crates/minarbot-server/src');
  const hits = readdirSync(source, { recursive: true }).filter(name => statSync(join(source, name.toString())).isFile())
    .filter(name => /not ported/i.test(readFileSync(join(source, name.toString()), 'utf8')));
  assert.deepEqual(hits, []);
});

test('Rust accepts every production environment name', { timeout: 90000 }, async () => {
  const directory = mkdtempSync('/dev/shm/b102-env-'); chmodSync(directory, 0o700);
  const id = '00000000-0000-4000-8000-000000000001';
  const binary = rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
  try {
    mkdirSync(join(directory, 'workspace'));
    execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(directory, 'key'), '-out', join(directory, 'cert'), '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=IP:127.0.0.1'], { stdio: 'ignore' });
    const hubKey = generateKeyPairSync('ed25519').publicKey.export({ type: 'spki', format: 'der' }).toString('base64url');
    const values: Record<string, string> = {
      PATH: '/usr/bin:/bin', PORT: String(await freePort()), MINARBOT_DATA_DIR: directory,
      MINARBOT_AGENT_DESKTOP: '1', MINARBOT_AGENT_DESKTOP_MAX: '3', MINARBOT_AGENT_DESKTOP_IDLE_MS: '1500', MINARBOT_AGENT_DESKTOP_GEOMETRY: '1440x900',
      MINARBOT_ALLOWED_ORIGINS: 'https://minarbot.example.test', MINARBOT_CONCURRENCY: '2', MINARBOT_MAX_TOKENS: '1024',
      MINARBOT_PROVIDER: 'openai-compatible', MINARBOT_PROVIDER_ACCOUNTS: '[]', MINARBOT_PROJECT_ROOT: join(directory, 'projects'),
      MINARBOT_WORKSPACES: JSON.stringify([{ agent: id, directory: join(directory, 'workspace'), write: true }]),
      MINARBOT_STREAM: '1', MINARBOT_STREAM_TERMINATION: 'marker', MINARBOT_INSTALLATION_ID: id,
      MINARBOT_SCRIPT_TOKEN: 'fixture-token-abcdefghijklmnopqrstuvwxyz', MINARBOT_SCRIPT_DIRECTORY: join(directory, 'scripts'),
      MINARBOT_RELEASES_DIR: join(directory, 'releases'), MINARBOT_LAUNCHER_SOCKET: join(directory, 'launcher.sock'), MINARBOT_UPDATE_TRIAL: id,
      COMPOSIO_API_KEY: 'dummy-env-contract', RESEND_API_KEY: 'dummy-mail-key', MINARBOT_MAIL_ENDPOINT: 'http://127.0.0.1:45981/emails', MINARBOT_MAIL_FROM: 'fixture@example.test',
      MINARBOT_HUB_ORIGIN: 'https://127.0.0.1:45982', MINARBOT_HUB_SIGNING_PUBLIC_KEY: hubKey,
      MINARBOT_ORIGIN: 'http://127.0.0.1:45983', MINARBOT_SERVER_URL: 'http://127.0.0.1:45983', MINARBOT_CANONICAL_DB: join(directory, 'canonical.sqlite'),
      CLERK_SECRET_KEY: 'sk_test_fixture', CLERK_PUBLISHABLE_KEY: 'pk_test_fixture', CLERK_ISSUER: 'https://fixture.clerk.accounts.dev', CLERK_AUDIENCE: 'minarbot',
      MINARBOT_TLS_CERT: join(directory, 'cert'), MINARBOT_TLS_KEY: join(directory, 'key'),
    };
    for (const prefix of ['OPENAI', 'ANTHROPIC', 'GEMINI']) {
      values[`${prefix}_BASE_URL`] = 'http://127.0.0.1:45984/v1'; values[`${prefix}_API_KEY`] = 'dummy-provider-key'; values[`${prefix}_MODEL`] = 'fixture';
    }
    for (const prefix of ['IMAGE', 'TRANSCRIPTION']) {
      values[`MINARBOT_${prefix}_BASE_URL`] = 'http://127.0.0.1:45985/v1'; values[`MINARBOT_${prefix}_API_KEY`] = 'dummy-media-key'; values[`MINARBOT_${prefix}_MODEL`] = 'fixture';
    }
    const names = new Set<string>(['MINARBOT_TLS_CERT', 'MINARBOT_TLS_KEY', 'MINARBOT_SERVER_URL', 'MINARBOT_ORIGIN', 'MINARBOT_CANONICAL_DB', 'CLERK_SECRET_KEY', 'CLERK_PUBLISHABLE_KEY', 'CLERK_ISSUER', 'CLERK_AUDIENCE']);
    const excluded = /(?:\.test\.ts$|-it\.ts$|^rust-|^vite(?:\.[^.]+)?\.config\.ts$|^(?:cli|slack-runner)\.ts$)/;
    for (const name of readdirSync(import.meta.dirname).filter(name => name.endsWith('.ts') && !excluded.test(name))) {
      const source = readFileSync(join(import.meta.dirname, name), 'utf8');
      for (const match of source.matchAll(/process\.env\.([A-Z][A-Z0-9_]*)/g)) names.add(match[1]);
    }
    assert.deepEqual([...names].filter(name => !(name in values)), [], 'New runtime env reads need a startup fixture');
    const env = { ...Object.fromEntries([...names].map(name => [name, values[name]])), MINARBOT_COMPOSIO: 'http://127.0.0.1:45986' };
    for (const mode of ['openai-compatible', 'anthropic', 'gemini', 'canonical']) {
      const port = await freePort(), data = join(directory, mode); mkdirSync(data);
      const child = spawn(binary, mode === 'canonical' ? ['--mode=canonical'] : [], { env: { ...env, HOME: directory, TMPDIR: directory, PORT: String(port), MINARBOT_DATA_DIR: data, MINARBOT_APPEARANCE_CLIENT: mode === 'anthropic' ? 'leptos' : 'react', MINARBOT_PROVIDER: mode === 'canonical' ? 'openai-compatible' : mode }, stdio: ['ignore', 'ignore', 'pipe'] });
      const closed = new Promise(ready => child.once('close', ready));
      let failure = ''; child.stderr.on('data', value => { failure = (failure + value).slice(-4096); });
      try {
        const deadline = Date.now() + 10000;
        let status = 0;
        while (Date.now() < deadline) {
          assert.equal(child.exitCode, null, failure);
          try {
            status = await new Promise<number>((ready, reject) => {
              const req = request(`https://127.0.0.1:${port}${mode === 'canonical' ? '/api/session' : '/api/environment'}`, { ca: readFileSync(join(directory, 'cert')), timeout: 500 }, res => { res.resume(); res.on('end', () => ready(res.statusCode ?? 0)); });
              req.on('error', reject); req.on('timeout', () => req.destroy()); req.end();
            });
          } catch {}
          if (status) break;
          await new Promise(ready => setTimeout(ready, 50));
        }
        assert.equal(status, mode === 'canonical' ? 426 : 200, `${mode}: ${failure}`);
        assert.equal(failure.match(/Ignoring deprecated MINARBOT_APPEARANCE_CLIENT/g)?.length, 1, failure);
      } finally { await stop(child, closed); }
    }
    console.log(`Runtime env contract: ${names.size} names accepted across 4 startup modes`);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
