import { execFileSync, spawnSync, type SpawnSyncOptionsWithStringEncoding } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { rustBinary } from '../rust-binary.ts';

const root = fileURLToPath(new URL('../../', import.meta.url));
const buildEnv = { ...process.env };
let fixtureBinary = buildEnv.MINARBOT_XTASK_FIXTURE_BINARY;

export function fixtureRunner() {
  if (fixtureBinary) return fixtureBinary;
  const args = ['test', '-p', 'xtask', '--bin', 'xtask', '--no-run', '--message-format=json'];
  const messages = execFileSync('cargo', args, {
    cwd: root, env: buildEnv, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'],
  });
  for (const line of messages.trim().split('\n')) {
    const message = JSON.parse(line);
    if (message.reason === 'compiler-artifact' && message.target.name === 'xtask' && message.profile.test && message.executable) {
      return fixtureBinary = String(message.executable);
    }
  }
  throw new Error('Cargo did not build the xtask fixture runner.');
}

export function xtask() {
  return rustBinary('xtask', buildEnv.MINARBOT_XTASK_BINARY);
}

type FixtureInput = { args: string[]; build?: object; value?: unknown; publicKey?: string; reply?: string };

export function runFixture(input: FixtureInput, options: SpawnSyncOptionsWithStringEncoding = { encoding: 'utf8' }) {
  return spawnSync(fixtureRunner(), ['--ignored', '--exact', 'app_fixture', '--nocapture'], {
    cwd: root, ...options, env: { ...process.env, ...options.env, MINARBOT_XTASK_FIXTURE: JSON.stringify(input) },
  });
}

function fixture<T>(input: FixtureInput, env?: NodeJS.ProcessEnv): T {
  const scratch = mkdtempSync(join(tmpdir(), 'minarbot-xtask-fixture-'));
  try {
    const reply = join(scratch, 'reply.json');
    const result = runFixture({ ...input, reply }, { encoding: 'utf8', env });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(result.stderr || result.stdout);
    return JSON.parse(readFileSync(reply, 'utf8'));
  } finally { rmSync(scratch, { recursive: true, force: true }); }
}

export type ReleaseMetadata = { id: string; version: string; commit: string; issues?: number[]; created: string; node: string; notes: string; files: Record<string, string> };
type Comment = { body: string };
type BuildOptions = {
  source?: string;
  repository?: string;
  releases: string;
  allowDirty?: boolean;
  build?: boolean;
  install?: boolean;
  issues?: unknown;
  notes?: string;
  readIssueComments?: (issue: number) => readonly Comment[];
};

export function buildRelease({ readIssueComments, ...build }: BuildOptions): ReleaseMetadata {
  if (!readIssueComments || !Array.isArray(build.issues)) {
    return fixture({ args: ['build'], build });
  }
  const scratch = mkdtempSync(join(tmpdir(), 'minarbot-issue-comments-'));
  try {
    const reports = Object.fromEntries(build.issues.map(issue => {
      try { return [issue, { comments: readIssueComments(issue) }]; }
      catch { return [issue, null]; }
    }));
    writeFileSync(join(scratch, 'gh'), '#!' + process.execPath + '\n' +
      'const report = ' + JSON.stringify(reports) + '[process.argv[4]];\n' +
      'if (!report) process.exit(1); console.log(JSON.stringify(report));\n', { mode: 0o700 });
    return fixture({ args: ['build'], build }, { PATH: scratch + ':' + process.env.PATH });
  } finally { rmSync(scratch, { recursive: true, force: true }); }
}
export function parseRelease(value: unknown): ReleaseMetadata { return fixture({ args: ['parse'], value }); }
export function verifyRelease(directory: string, id: string): ReleaseMetadata { return fixture({ args: ['verify', directory, id] }); }
export function fileManifest(directory: string): Record<string, string> { return fixture({ args: ['manifest', directory] }); }
export function selectRelease(directory: string, name: string, id: string) { fixture({ args: ['select', directory, name, id] }); }
export function selectedRelease(directory: string, name = 'current'): string | undefined {
  return fixture<string | null>({ args: ['selection', directory, name] }) ?? undefined;
}
export function latestRelease(directory: string): ReleaseMetadata | undefined {
  try { return parseRelease(JSON.parse(readFileSync(join(directory, 'feed/latest.json'), 'utf8'))); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
}
