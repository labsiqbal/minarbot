import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmodSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { ProtocolError } from '../shared/protocol.ts';

export type HostedHubConfig = {
  domain: string;
  accountsHost: string;
  dataPath: string;
  mailboxKeyPath: string;
  trustedGatewayAddresses?: string[];
  identity: { mode: 'stub'; tokensPath: string } | { mode: 'clerk'; issuer: string; jwksPath: string; audience?: string };
  web?: { distPath: string; developmentIdentity?: boolean };
  nodeLink?: { signingKeyPath: string };
};
export type SessionIdentity = { issuer: string; subject: string; verifiedMailboxes: string[]; sessionId: string; expiresAt: number };
export type PostInput = { requestId: string; sectionId: string; text: string; mentions: string[] };
type Principal = { nodeId: string; accountId: string; nodeGeneration: number; connectionGeneration: number };
type Control = {
  now: number;
  post?: { projectId: string; accountId: string; input: PostInput };
  withdraw_node?: { principal: Principal; input: { projectId: string; participationId: string; revision: number } };
};

function hubEnvironment(dir: string, config: HostedHubConfig, port: number) {
  const file = join(dir, 'hub-config.json');
  writeFileSync(file, JSON.stringify({ ...config, port }), { mode: 0o600 });
  return { ...process.env, MINARBOT_HUB_CONFIG: file };
}

export function hubCommand(binary: string, dir: string, config: HostedHubConfig, ...args: string[]) {
  return execFileSync(binary, ['--hub', ...args], {
    env: hubEnvironment(dir, config, 0), encoding: 'utf8', timeout: 10000, stdio: ['ignore', 'pipe', 'pipe'],
  });
}

export async function startRustHub(binary: string, dir: string, config: HostedHubConfig, port = 0, testFixture = false) {
  if (existsSync(config.dataPath)) chmodSync(config.dataPath, 0o600);
  const child = spawn(binary, testFixture ? ['--hub', 'test-fixture'] : ['--hub'], {
    env: { ...hubEnvironment(dir, config, port), ...(testFixture ? { MINARBOT_TEST_SERVER: 'rust' } : {}) },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const closed = once(child, 'close');
  const close = async () => {
    child.stdin.end();
    if (child.exitCode === null && child.signalCode === null && child.pid !== undefined) child.kill('SIGTERM');
    await closed.catch(() => {});
  };
  let diagnostic = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { if (diagnostic.length < 4096) diagnostic += chunk; });
  const lines = createInterface({ input: child.stdout });
  child.stdin.on('error', () => {});
  let controlling = false;
  async function control(input: Control) {
    if (!testFixture || !child.stdin.writable || child.exitCode !== null || child.signalCode !== null || controlling) {
      throw new Error('Hosted hub fixture control is unavailable.');
    }
    controlling = true;
    try {
      const reply = once(lines, 'line', { signal: AbortSignal.any([AbortSignal.timeout(10000), stopped.signal]) });
      child.stdin.write(JSON.stringify(input) + '\n');
      const [line] = await reply;
      const { result, error } = JSON.parse(line);
      if (error) throw new ProtocolError(error.status, error.code, error.message);
      return result;
    } finally {
      controlling = false;
    }
  }
  const stopped = new AbortController();
  lines.once('close', () => stopped.abort());
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const ready = new Promise<number>((resolve, reject) => {
      timer = setTimeout(() => reject(new Error('Hosted hub did not listen')), 10000);
      lines.on('line', line => {
        const match = /^Minarbot hub: http:\/\/127\.0\.0\.1:(\d+)$/.exec(line);
        if (match) resolve(Number(match[1]));
      });
    });
    const port = await Promise.race([ready, closed.then(() => { throw new Error('Hosted hub exited before readiness: ' + diagnostic); })]);
    return { port, close, control };
  } catch (error) {
    await close();
    throw error;
  } finally {
    clearTimeout(timer);
    if (!testFixture) lines.close();
  }
}
