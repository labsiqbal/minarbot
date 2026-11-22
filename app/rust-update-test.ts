import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { promisify } from 'node:util';
import { verifyRelease } from './test-support/release.ts';
const run = promisify(execFile);
export async function releaseArchive(releases: string, target: string) {
  verifyRelease(releases, target);
  const directory = mkdtempSync(join(releases, '.archive-')), file = join(directory, 'release.tar.gz');
  try {
    await run('tar', ['--format=ustar', '-czf', file, '-C', join(releases, target), '.']);
    const bytes = readFileSync(file);
    return { file, sha256: createHash('sha256').update(bytes).digest('hex'), cleanup: () => rmSync(directory, { recursive: true, force: true }), size: statSync(file).size };
  } catch (error) { rmSync(directory, { recursive: true, force: true }); throw error; }
}
export async function receiveRelease(response: Response, releases: string, target: string, expected: string) {
  const bytes = Buffer.from(await response.arrayBuffer());
  if (createHash('sha256').update(bytes).digest('hex') !== expected) throw new Error('Release SHA-256 mismatch.');
  return { target, releases };
}
