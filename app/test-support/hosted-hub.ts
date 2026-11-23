import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmodSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';

type Config = {
  domain: string;
  accountsHost: string;
  dataPath: string;
  mailboxKeyPath: string;
  trustedGatewayAddresses?: string[];
  identity: { mode: 'stub'; tokensPath: string } | { mode: 'clerk'; issuer: string; jwksPath: string; audience?: string };
  web?: { distPath: string; developmentIdentity?: boolean };
  nodeLink?: { signingKeyPath: string };
};

export async function startRustHub(binary: string, dir: string, config: Config, port = 0) {
  chmodSync(config.dataPath, 0o600);
  const file = join(dir, 'hub-config.json');
  writeFileSync(file, JSON.stringify({ ...config, port }), { mode: 0o600 });
  const child = spawn(binary, ['--hub'], {
    env: { ...process.env, MINARBOT_HUB_CONFIG: file },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const closed = once(child, 'close');
  const close = async () => {
    if (child.exitCode === null && child.signalCode === null && child.pid !== undefined) child.kill('SIGTERM');
    await closed.catch(() => {});
  };
  let diagnostic = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { if (diagnostic.length < 4096) diagnostic += chunk; });
  const lines = createInterface({ input: child.stdout });
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const ready = new Promise<number>((resolve, reject) => {
      timer = setTimeout(() => reject(new Error('Hosted hub did not listen')), 10000);
      lines.on('line', line => {
        const address = /^Minarbot hub: http:\/\/127\.0\.0\.1:(\d+)$/.exec(line);
        if (address) resolve(Number(address[1]));
      });
    });
    return { port: await Promise.race([
      ready,
      closed.then(() => { throw new Error('Hosted hub exited before readiness: ' + diagnostic); }),
    ]), close };
  } catch (error) {
    await close();
    throw error;
  } finally {
    clearTimeout(timer);
    lines.close();
  }
}
