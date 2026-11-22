export type MachineInstall = { source: string; token: string; expiresAt: number };

export function machineInstallCommand(install: MachineInstall, name?: string, origin?: string) {
  const quote = (value: string) => "'" + value.replaceAll("'", "'\\''") + "'";
  const source = quote(install.source);
  const script = quote(install.source + '/api/install/script');
  const args = ' --source ' + source + ' --token ' + quote(install.token);
  return 'curl -fsSL ' + script + ' | sh -s --' + args
    + (name ? ' --name ' + quote(name) : '')
    + (origin?.startsWith('https://') ? ' --public-origin ' + quote(origin) : '');
}
