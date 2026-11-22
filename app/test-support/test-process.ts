import { execFileSync, type ChildProcess } from 'node:child_process';
import { rustBinary } from '../rust-binary.ts';

export function freePort(): number {
  const executable = rustBinary('xtask', process.env.MINARBOT_XTASK_BINARY);
  return Number(execFileSync(executable, ['test-port'], { encoding: 'utf8' }));
}

export async function stop(child: ChildProcess, closed: Promise<unknown>) {
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  const timer = setTimeout(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL'); }, 8000);
  try { await closed; } finally { clearTimeout(timer); }
}
