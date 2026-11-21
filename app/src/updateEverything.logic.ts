// Adapted from T3 updateEverything.logic.ts (MIT). See app/t3/provenance.json.
import type { PairedMachine } from '../shared/machines';
import type { ReleaseInfo, UpdateAttempt, UpdateDownload, UpdateSource, UpdateState } from '../shared/updates';

export type PeerState = {
  machine: string;
  available: boolean | ReleaseInfo;
  launcherAvailable?: boolean;
  runningTurns?: number;
  current?: { version: string };
  lastAttempt?: UpdateAttempt;
  reason?: string;
  fanOutAttempt?: UpdateDownload & { id: string; state: string };
};
export type UpdateMachine = {
  id: string;
  name: string;
  current: string;
  target: string | null;
  source?: UpdateSource;
  running: number;
  local: boolean;
  skipped: string | null;
};
export type RunMachine = UpdateMachine & { status: string; reason?: string; attemptId?: string };
export type UpdateCommand = { id: string; version: string; source?: UpdateSource; machines: string[]; startedAt?: number; submitted?: boolean };
export type UpdateRun = { rows: RunMachine[]; commands: UpdateCommand[] };

export const phases: Record<string, string> = {
  waiting: 'Waiting', submitting: 'Preparing update', accepted: 'Preparing update',
  downloading: 'Downloading', uncertain: 'Checking status', reconnecting: 'Reconnecting', 'backing-up': 'Backing up workspace',
  starting: 'Restarting', verifying: 'Verifying server health', restoring: 'Restoring workspace snapshot',
  done: 'Updated', 'rolled-back': 'Rolled back', failed: 'Update failed', unconfirmed: 'Update unconfirmed',
};
export const terminalUpdate = (status?: string) => status === 'done' || status === 'rolled-back' || status === 'failed';
export const settled = (row: RunMachine) => !!row.skipped || terminalUpdate(row.status) || row.status === 'unconfirmed';
export const runActive = (run: UpdateRun | null) => run?.rows.some(row => !settled(row)) ?? false;

function newer(candidate: string, current: string): boolean {
  const parts = /^(\d+)\.(\d+)\.(\d+)(-.+)?$/;
  const a = parts.exec(candidate), b = parts.exec(current);
  if (!a || !b) return false;
  for (let i = 1; i <= 3; i++) {
    if (BigInt(a[i]) !== BigInt(b[i])) return BigInt(a[i]) > BigInt(b[i]);
  }
  return !a[4] && !!b[4];
}

export function availableUpdates(state: UpdateState) {
  const primary = state.available;
  if (!primary) return [];
  const other = primary.source === 'GitHub' ? state.feed : state.github;
  return other && newer(other.version, state.current.version) ? [primary, other] : [primary];
}

export function downloadLabel(download: UpdateDownload) {
  const percent = download.totalBytes && Math.floor(100 * (download.bytesTransferred ?? 0) / download.totalBytes);
  return percent == null || percent === 0 ? 'Downloading' : `Downloading ${Math.min(percent, 100)}%`;
}

export function collectServerUpdateTargets(state: UpdateState, machines: readonly PairedMachine[], peers: readonly PeerState[], localName: string) {
  const localFeed = state.feed ?? state.available;
  const feed = state.github && (!localFeed || !newer(localFeed.version, state.github.version)) ? state.github : localFeed;
  const local: UpdateMachine = {
    id: 'local', name: localName, current: state.current.version, target: state.available?.version ?? null, source: state.available?.source,
    running: state.runningTurns, local: true,
    skipped: !state.launcherAvailable ? 'Launcher unavailable'
      : state.lastAttempt && !terminalUpdate(state.lastAttempt.status) ? 'Already updating'
      : state.available ? null : 'Up to date',
  };
  const remotes = machines.map(machine => {
    const peer = peers.find(row => row.machine === machine.id);
    const current = peer?.current?.version ?? machine.version;
    let skipped = null;
    if (machine.status === 'unauthorized') skipped = 'Access revoked';
    else if (machine.status === 'stale') skipped = 'Status stale';
    else if (machine.status === 'offline') skipped = 'Offline';
    else if (!machine.allowUpdates) skipped = 'Remote updates off';
    else if (feed && !newer(feed.version, current)) skipped = 'Up to date';
    else if (peer?.lastAttempt && !terminalUpdate(peer.lastAttempt.status)) skipped = 'Already updating';
    else if (!peer?.available || !peer.launcherAvailable || !feed) skipped = peer?.reason ?? 'Updates unavailable';
    return {
      id: machine.id, name: machine.name, current, target: skipped ? null : feed!.version, source: feed?.source,
      running: peer?.runningTurns ?? machine.agents.filter(agent => agent.status === 'running').length,
      local: false, skipped,
    };
  });
  return [local, ...remotes];
}

export function describeUpdateEverything(rows: readonly UpdateMachine[]) {
  const targets = rows.filter(row => !row.skipped);
  const running = targets.reduce((count, row) => count + row.running, 0);
  return {
    count: targets.length,
    version: targets[0]?.target,
    warning: `${running} running ${running === 1 ? 'turn' : 'turns'} will be interrupted and can resume after the restart.`,
  };
}

export function describeUpdateCapsule(run: UpdateRun | null, reconnecting: boolean) {
  if (!run) return { label: 'Update available', attention: false };
  if (reconnecting && runActive(run)) return { label: 'Reconnecting…', attention: false };
  const machines = run.rows.filter(row => !row.skipped);
  const done = machines.filter(row => row.status === 'done').length;
  if (runActive(run)) return { label: `${done} of ${machines.length} updated`, attention: false };
  const failed = machines.filter(row => row.status === 'failed' || row.status === 'rolled-back').length;
  if (failed) return { label: `${failed} update${failed === 1 ? '' : 's'} failed`, attention: true };
  const unconfirmed = machines.length - done;
  if (unconfirmed) return { label: `${unconfirmed} update${unconfirmed === 1 ? '' : 's'} unconfirmed`, attention: true };
  return { label: `${done} of ${machines.length} updated`, attention: false };
}

export function planUpdateRun(rows: UpdateMachine[], createId: () => string): UpdateRun {
  const targets = rows.filter(row => !row.skipped);
  const local = targets.find(row => row.local);
  const peers = targets.filter(row => !row.local);
  const commands: UpdateCommand[] = [];
  for (let i = 0; i < peers.length; i += 16) {
    commands.push({ id: createId(), version: peers[i].target!, source: peers[i].source, machines: peers.slice(i, i + 16).map(row => row.id) });
  }
  if (local) commands.push({ id: createId(), version: local.target!, source: local.source, machines: [local.id] });
  return { commands, rows: rows.map(row => ({ ...row, status: 'waiting' })) };
}

export function nextUpdateCommand(run: UpdateRun): UpdateCommand | undefined {
  return run.commands.find(command => run.rows.some(row => command.machines.includes(row.id) && !settled(row)));
}

export function resolveUpdateRunMachineStatus(
  row: RunMachine, command: UpdateCommand, snapshot: { current?: { version: string }; lastAttempt?: UpdateAttempt } | undefined, now: number,
): RunMachine {
  if (!command.startedAt || settled(row)) return row;
  const attempt = snapshot?.lastAttempt;
  const matches = attempt && attempt.id === (row.local ? command.id : row.attemptId);
  if (matches) return row.status === attempt.status && row.reason === attempt.reason
    ? row : { ...row, status: attempt.status, reason: attempt.reason };
  // Without a receipt, the installed target is the only positive proof from a peer.
  if (snapshot?.current?.version === command.version && (!attempt || terminalUpdate(attempt.status))) {
    return { ...row, status: 'done', reason: undefined };
  }
  if (now - command.startedAt >= 15 * 60_000) {
    return { ...row, status: 'unconfirmed', reason: row.reason ?? 'Check this machine for the update result.' };
  }
  if (command.submitted && !snapshot?.current && row.status !== 'reconnecting') {
    return { ...row, status: 'reconnecting' };
  }
  return row;
}
