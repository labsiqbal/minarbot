import { rustHubFacade } from './rust-hub-test-bridge.ts';
import type { HubLinkOptions } from './rust-hub-test-bridge.ts';
import { connectorsTestBridge } from './rust-connectors-test-bridge.ts';
import type { DesktopOptions } from './test-support/desktop-types.ts';
import { computerTestBridge } from './rust-computer-test-bridge.ts';
import type { PeerTokenIssuer } from './test-support/peer-token-issuer.ts';
import { spawn } from 'node:child_process';
import { rustBinary } from './rust-binary.ts';
import type { AgentSessionIssuer } from './test-support/harness-types.ts';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { harnessTestBridge, pdfTestBridge, rustMcpFacade, type PdfFault } from './rust-harness-test-bridge.ts';
import { ProtocolError } from './shared/protocol.ts';
import { fixPath } from './t3/path.ts';

type Config = { appearanceClient?: 'react' | 'leptos'; testMemoryExtraction?: boolean; pdfFault?: PdfFault; testPathPrefix?: string; testCanonicalBridge?: { origin: string; token: string; clerk: boolean; mail: boolean; webhook: boolean }; hubLink?: HubLinkOptions; composio?: { baseURL?: string; timeout?: number }; mailApiKey?: string; mailEndpoint?: string; scriptDirectory?: string; agentLimits?: unknown; imageProvider?: unknown; transcriptionProvider?: unknown; scriptToken?: string; installationId?: string; mailFrom?: string; mailOrigin?: string; testKeyring?: Record<string, string>; testKeyringLocked?: boolean; updates?: { releasesDir?: string; launcherSocket?: string; testReleaseBaseUrl?: string; testReleasePublicKey?: string }; desktop?: DesktopOptions; peerTokenIssuer?: PeerTokenIssuer; machines?: { now?: () => number; healthIntervalMs?: number }; baseURL?: string; apiKey?: string; defaultModel?: string; provider?: string; accounts?: unknown; timeout?: number; concurrency?: number; workspaces?: unknown; projectRoot?: string; harness?: { issuer?: AgentSessionIssuer; executables?: Partial<Record<import('./shared/runtime.ts').HarnessRuntime, string>>; mcpURL?: string }; stream?: boolean; streamTermination?: string; maxTokens?: number; database: string; port?: number; host?: string; tls?: { cert: string; key: string }; allowedOrigins?: string[]; dist?: string; auth?: { secretKey: string; publishableKey: string; issuer: string; origin: string; development?: boolean; audience?: string; jwtKey?: string; apiUrl?: string }; fixedClock?: number };
export async function startRustServer(options: Config, attempt = 0): Promise<{ origin: string; close: () => Promise<void>; crash: () => Promise<void>; refreshMachines: () => Promise<void>; mcpServer: ReturnType<typeof rustMcpFacade>; hubLink?: ReturnType<typeof rustHubFacade> }> {
  await fixPath();
  if (options.concurrency !== undefined && (!Number.isSafeInteger(options.concurrency) || options.concurrency < 1 || options.concurrency > 32)) throw new Error('MINARBOT_CONCURRENCY must be 1..32');
  const supported = new Set(['appearanceClient', 'testMemoryExtraction', 'pdfFault', 'testPathPrefix', 'testCanonicalBridge', 'hubLink', 'composio', 'mailApiKey', 'mailEndpoint', 'agentLimits', 'imageProvider', 'transcriptionProvider', 'scriptToken', 'scriptDirectory', 'installationId', 'mailFrom', 'mailOrigin', 'testKeyring', 'testKeyringLocked', 'updates', 'desktop', 'peerTokenIssuer', 'machines', 'database', 'port', 'host', 'tls', 'allowedOrigins', 'dist', 'auth', 'fixedClock', 'baseURL', 'apiKey', 'defaultModel', 'provider', 'accounts', 'timeout', 'concurrency', 'workspaces', 'projectRoot', 'harness', 'stream', 'streamTermination', 'maxTokens']);
  for (const key of Object.keys(options)) if (!supported.has(key)) throw new Error(`not ported: option ${key}`);
  for (const key of Object.keys(options.hubLink ?? {})) if (!['origin', 'hubSigningPublicKey', 'development', 'entry', 'fetch', 'webSocket', 'reconnectMs', 'now'].includes(key)) throw new Error(`not ported: option hubLink.${key}`);
  const binary = rustBinary('minarbot-server', process.env.MINARBOT_RUST_BINARY);
  const bridge = await computerTestBridge(options);
  const connectors = await connectorsTestBridge(options).catch(async error => { await bridge.close(); throw error; });
  const pdfBridge = await pdfTestBridge(options.pdfFault);
  const harnessBridge = await harnessTestBridge(options.harness as Parameters<typeof harnessTestBridge>[0]);
  const { peerTokenIssuer: unusedIssuer, machines, desktop, testPathPrefix, pdfFault: unusedPdfFault, ...serializable } = options;
  let config: string;
  try { config = JSON.stringify({ ...serializable, hubLink: options.hubLink ? { origin: options.hubLink.origin, hubSigningPublicKey: options.hubLink.hubSigningPublicKey, development: options.hubLink.development, reconnectMs: options.hubLink.reconnectMs } : undefined, harness: options.harness ? { ...options.harness, issuer: undefined } : undefined, testHarnessBridge: harnessBridge.config, testPdfBridge: pdfBridge.config, desktop: desktop ? { ...desktop, runtime: undefined, now: undefined, testRuntime: !!desktop.runtime } : undefined, machines: machines ? { healthIntervalMs: machines.healthIntervalMs } : undefined, testConnectorBridge: connectors.config, testComputerBridge: bridge.config }, (key, value) => { if (typeof value === 'function') throw new Error(`not ported: function option ${key}`); return value; }); } catch (error) { await bridge.close(); await connectors.close(); await harnessBridge.close(); await pdfBridge.close(); throw error; }
  const root = fileURLToPath(new URL('../', import.meta.url));
  const child = spawn(binary, ['--test-config'], { cwd: root, env: { ...process.env, ...(testPathPrefix ? { PATH: testPathPrefix + ':' + process.env.PATH } : {}), MINARBOT_TEST_SERVER: 'rust', MINARBOT_TEST_PARENT: String(process.pid) }, stdio: ['pipe', 'pipe', 'pipe'] });
  const exited = new Promise<void>(closed => child.once('close', () => closed()));
  let stderr = '', stdout = '';
  child.stderr.setEncoding('utf8'); child.stderr.on('data', value => { stderr = (stderr + value).slice(-8192); });
  child.stdout.setEncoding('utf8');
  child.stdin.on('error', () => {});
  child.stdin.end(JSON.stringify({ ...JSON.parse(config), database: resolve(options.database), port: options.port ?? 0 }));
  let closing: Promise<void> | undefined;
  const close = () => closing ??= (async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
    const timeout = setTimeout(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); }, 5000);
    try { await exited; } finally { clearTimeout(timeout); await bridge.close(); await connectors.close(); await harnessBridge.close(); await pdfBridge.close(); }
  })();
  const startupOutput = () => `\nstdout tail:\n${stdout.trimEnd()}\nstderr tail:\n${stderr.trimEnd()}`;
  try {
    const origin = await new Promise<string>((ready, reject) => {
      const timeout = setTimeout(() => reject(new Error(`Rust test server readiness timed out after 120 s.${startupOutput()}`)), 120_000);
      child.once('error', error => {
        clearTimeout(timeout);
        reject(Object.assign(new Error(`Rust test server could not start: ${error.message}${startupOutput()}`, { cause: error }), { code: (error as NodeJS.ErrnoException).code }));
      });
      void exited.then(() => {
        clearTimeout(timeout);
        const error = new Error(`Rust test server exited (${child.signalCode ?? child.exitCode}).${startupOutput()}`);
        if (stderr.includes('Another canonical server is still running.')) Object.assign(error, { code: 'canonical_server_busy' });
        else if (stderr.includes('Address already in use')) Object.assign(error, { code: 'EADDRINUSE' });
        reject(error);
      });
      child.stdout.on('data', value => {
        stdout = (stdout + value).slice(-8192);
        const match = /^Minarbot: (https?:\/\/[^\r\n]+)\r?\n/m.exec(stdout);
        if (match) { clearTimeout(timeout); ready(match[1]); }
      });
    });
    const readOutput = () => ({ stdout, stderr });
    serverOutput.set(origin, readOutput);
    void exited.then(() => {
      if (serverOutput.get(origin) === readOutput) serverOutput.delete(origin);
    });
    const crash = async () => {
      if (child.exitCode !== null || child.signalCode !== null || !child.kill('SIGKILL')) throw new Error('Rust fixture was not running.');
      await exited;
      if (child.signalCode !== 'SIGKILL') throw new Error('Rust fixture did not exit with SIGKILL.');
      await close();
    };
    return { origin, close, crash, hubLink: options.hubLink ? rustHubFacade(resolve(options.database), origin, bridge.config.token) : undefined, mcpServer: rustMcpFacade(resolve(options.database)), refreshMachines: async () => { const response = await fetch(origin + '/api/machines', { headers: { 'X-Minarbot-Test-Refresh': bridge.config.token } }); if (!response.ok) throw new Error(await response.text()); } };
  } catch (error) {
    await close().catch(() => {});
    if (error instanceof Error && 'code' in error && error.code === 'canonical_server_busy') {
      if (attempt < 20) {
        await new Promise(resolve => setTimeout(resolve, 100));
        return startRustServer(options, attempt + 1);
      }
      throw new ProtocolError(409, 'canonical_server_busy', 'Another canonical server is still running.');
    }
    throw error;
  }
}

const serverOutput = new Map<string, () => { stdout: string; stderr: string }>();

export function rustServerOutput(origin: string) {
  return serverOutput.get(origin)?.() ?? { stdout: '', stderr: '' };
}
