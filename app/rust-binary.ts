import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const built = new Set<string>();
const buildEnvironment = { ...process.env };

// Builds the binary through Cargo before a parity test starts it.
export function rustBinary(name: 'minarbot-server' | 'minarbot-launcher' | 'xtask', override?: string): string {
  if (override) return override;
  if (!built.has(name)) {
    const args = ['build', '--quiet', '--package', name, '--bin', name];
    execFileSync('cargo', args, {
      cwd: root,
      env: buildEnvironment,
      stdio: ['ignore', 'ignore', 'inherit'],
    });
    built.add(name);
  }
  return resolve(root, buildEnvironment.CARGO_TARGET_DIR ?? 'target', 'debug', name);
}
