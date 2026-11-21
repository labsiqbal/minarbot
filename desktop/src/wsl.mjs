import { execFile } from 'node:child_process';
import { join } from 'node:path';
import { promisify } from 'node:util';

export const localServer = 'http://127.0.0.1:4317/';

export async function startWindowsServer(resources, version) {
  const run = promisify(execFile);
  try {
    await run('powershell.exe', [
      '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
      '-File', join(resources, 'install.ps1'), '-NoOpen',
      '-InstallerPath', join(resources, 'install.sh'),
      '-ArchivePath', join(resources, 'minarbot-linux.tar.gz'), '-Tag', 'v' + version,
    ], { windowsHide: true, timeout: 195000 });
  } catch (error) {
    if (/wsl --install -d Ubuntu/.test(error.stderr ?? '')) throw new Error('Install WSL 2 and open Ubuntu once to create your Linux user.');
    if (/sudo apt install curl python3 openssl/.test(error.stderr ?? '')) throw new Error('In Ubuntu, run sudo apt update and sudo apt install curl python3 openssl.');
    throw new Error('Could not start Minarbot in WSL. Check Ubuntu network access and port 4317.');
  }
}
