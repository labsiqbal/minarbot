// Adapted from T3 useUpdateEverything.ts (MIT). See app/t3/provenance.json.
import { useRef, useState } from 'react';
import type { PairedMachine } from '../../shared/machines';
import type { ReleaseInfo, UpdateDownload, UpdateState } from '../../shared/updates';
import { isReconnecting } from '../http.ts';
import {
  availableUpdates, collectServerUpdateTargets, nextUpdateCommand, planUpdateRun, resolveUpdateRunMachineStatus, runActive, terminalUpdate,
  type PeerState, type UpdateCommand, type UpdateMachine, type UpdateRun,
} from '../updateEverything.logic.ts';
import { localUpdateKey, readLocalUpdate, readSavedUpdateRun, submitUpdateCommand, updateRunCommand, updateRunKey, withUpdateReceipts, type UpdateRequest } from '../updateEverythingRun.ts';
import { useCachedData, useQueryPolling } from './useCachedQuery.ts';
import { queryCache } from './queryCache.ts';

type PollIssue = { message: string; reconnecting: boolean };
const message = (error: unknown) => error instanceof Error ? error.message : 'Update status unavailable.';
const pollIssue = (error: unknown): PollIssue => ({ message: message(error), reconnecting: isReconnecting(error) });

function readRecovery() {
  try {
    return { run: readSavedUpdateRun(sessionStorage), local: readLocalUpdate(sessionStorage), error: '' };
  } catch {
    return { run: null, local: null, error: 'Saved update could not be read. Check update history before discarding it.' };
  }
}

type Directory = { machines: PairedMachine[]; error: string; reconnecting: boolean; loaded: boolean };

export function useUpdateEverything(request: UpdateRequest, directory: Directory, localName: string, settingsOpen: boolean) {
  const { machines, error: machineError, reconnecting: machinesReconnecting, loaded } = directory;
  const [recovery] = useState(readRecovery);
  const [run, setRun] = useState(recovery.run);
  const current = useRef(run), peerSnapshots = useRef<PeerState[]>([]);
  const legacy = useRef(recovery.run ? null : recovery.local);
  const path = '/api/updates', peersPath = '/api/updates/machines';
  const { data: state, fresh } = useCachedData<UpdateState>(path);
  const { data: peerList, fresh: peersFresh } = useCachedData<{ machines: PeerState[] }>(peersPath);
  const peers = peerList?.machines ?? null;
  const [selection, setSelection] = useState<string[] | null>(null);
  const [selectedRelease, setSelectedRelease] = useState<ReleaseInfo>();
  const [open, setOpen] = useState(!!recovery.run), [localIssue, setLocalIssue] = useState<PollIssue | null>(null);
  const [peerIssue, setPeerIssue] = useState<PollIssue | null>(null), [commandIssue, setCommandIssue] = useState<PollIssue | null>(null);
  const [storageError, setStorageError] = useState(recovery.error);
  const active = runActive(run);
  const rows = state ? collectServerUpdateTargets(state, machines, peers ?? [], localName) : [];
  const reconnecting = machinesReconnecting || !!(localIssue?.reconnecting || peerIssue?.reconnecting || commandIssue?.reconnecting);
  const error = [localIssue, peerIssue, commandIssue].find(issue => issue && !issue.reconnecting)?.message || (!machinesReconnecting ? machineError : '');
  const downloads: Record<string, UpdateDownload> = {};
  if (state?.lastAttempt?.status === 'downloading') downloads.local = state.lastAttempt;
  for (const peer of peers ?? []) {
    const attempt = peer.fanOutAttempt;
    if (attempt?.state === 'transferring') downloads[peer.machine] = attempt;
  }
  const ready = rows.some(row => !row.skipped);
  const planning = selection === null && (!loaded || machines.length > 0 && !peersFresh);

  function save(next: UpdateRun) {
    try { sessionStorage.setItem(updateRunKey, JSON.stringify(next)); }
    catch { setStorageError('Update progress could not be saved. Check browser storage.'); return false; }
    current.current = next;
    setRun(next);
    setStorageError('');
    return true;
  }

  function clearLocalCommand(id: string) {
    if (readLocalUpdate(sessionStorage)?.id === id) sessionStorage.removeItem(localUpdateKey);
  }

  function start(plan: UpdateMachine[]) {
    if (current.current || !fresh || storageError || planning && !legacy.current || reconnecting || error || !plan.some(row => !row.skipped)) return;
    const pending = legacy.current;
    const next = planUpdateRun(plan, () => crypto.randomUUID());
    if (pending) {
      const local = next.commands.find(command => command.machines.includes('local'));
      if (!local || local.version !== pending.version || local.source !== pending.source) return;
      local.id = pending.id;
    }
    if (save(next)) {
      legacy.current = null;
      setCommandIssue(null);
    }
  }

  function dismiss() {
    if (current.current && !runActive(current.current)) {
      try {
        const local = current.current.commands.find(command => command.machines.includes('local'));
        if (local && current.current.rows.find(row => row.local)?.status !== 'unconfirmed') clearLocalCommand(local.id);
        sessionStorage.removeItem(updateRunKey);
        legacy.current = readLocalUpdate(sessionStorage);
      } catch { setStorageError('Update progress could not be cleared. Check browser storage.'); return; }
      current.current = null;
      setRun(null);
    }
    setOpen(false);
  }

  function discardRecovery() {
    try {
      sessionStorage.removeItem(updateRunKey);
      sessionStorage.removeItem(localUpdateKey);
      legacy.current = null;
      setStorageError('');
    } catch { setStorageError('Saved update could not be discarded. Check browser storage.'); }
  }

  function show(machineIds: string[] | null = null, release?: ReleaseInfo) {
    setSelection(machineIds);
    setSelectedRelease(release);
    setOpen(true);
  }

  async function advance(value: UpdateState) {
    let next = current.current;
    if (!next || !runActive(next) || storageError) return;
    const pending = nextUpdateCommand(next);
    if (!pending) return;
    const local = pending.machines[0] === 'local';
    const now = Date.now();
    if (pending.startedAt) {
      const existing = next;
      const rows = next.rows.map(row => {
        if (!pending.machines.includes(row.id)) return row;
        const snapshot = row.local ? value : peerSnapshots.current.find(peer => peer.machine === row.id);
        return resolveUpdateRunMachineStatus(row, pending, snapshot, now);
      });
      if (rows.some((row, index) => row !== existing.rows[index])
        && !save(next = { ...next, rows })) return;
    }
    const command = nextUpdateCommand(next);
    if (!command || command !== pending || command.submitted) return;
    // A matched local attempt proves acceptance even when the POST response was lost.
    if (local && value.lastAttempt?.id === command.id) {
      save(updateRunCommand(next, { ...command, submitted: true }));
      setCommandIssue(null);
      return;
    }
    const sending = { ...command, startedAt: command.startedAt ?? now };
    if (!command.startedAt) {
      if (!save(next = updateRunCommand({ ...next, rows: next.rows.map(row =>
        command.machines.includes(row.id) ? { ...row, status: 'submitting' } : row) }, sending))) return;
    }
    try {
      if (local) {
        sessionStorage.setItem(localUpdateKey, JSON.stringify({ id: sending.id, version: sending.version, source: sending.source }));
      }
    } catch {
      setStorageError('Update request could not be saved. Check browser storage.');
      return;
    }

    try {
      save(withUpdateReceipts(next, sending, await submitUpdateCommand(request, sending)));
      setCommandIssue(null);
    } catch (error) {
      if (isReconnecting(error)) setCommandIssue(pollIssue(error));
      else save(withUpdateReceipts(next, sending, sending.machines.map(machine => ({ machine, status: 'failed', reason: message(error) }))));
    }
  }

  useQueryPolling(async signal => {
    const version = queryCache.version();
    let value: UpdateState;
    try { value = await queryCache.refresh(path, () => request<UpdateState>(path, undefined, signal)); }
    catch (error) { if (!signal.aborted) setLocalIssue(pollIssue(error)); return; }
    if (signal.aborted || version !== queryCache.version()) return;
    setLocalIssue(null);
    setCommandIssue(null);
    const pending = legacy.current;
    if (pending && value.lastAttempt?.id === pending.id) {
      if (terminalUpdate(value.lastAttempt.status)) {
        try { clearLocalCommand(pending.id); legacy.current = null; }
        catch { setStorageError('Saved update could not be cleared. Check browser storage.'); }
      } else if (!current.current && !storageError) {
        const [machine] = collectServerUpdateTargets(value, [], [], localName);
        const next = planUpdateRun([{ ...machine, skipped: null, target: pending.version, source: pending.source }], () => pending.id);
        if (save(updateRunCommand(next, { ...next.commands[0], startedAt: Date.now(), submitted: true }))) {
          legacy.current = null;
          setOpen(true);
        }
      }
    }
    await advance(value);
  }, { interval: active || settingsOpen || open ? 1000 : 15000 }, [request]);

  useQueryPolling(async signal => {
    const version = queryCache.version();
    try {
      const result = await queryCache.refresh(peersPath, () => request<NonNullable<typeof peerList>>(peersPath, undefined, signal));
      if (!signal.aborted && version === queryCache.version()) {
        peerSnapshots.current = result.machines;
        setPeerIssue(null);
      }
    } catch (error) { if (!signal.aborted) setPeerIssue(pollIssue(error)); }
  }, { interval: active || settingsOpen || open ? 1000 : 5000, enabled: machines.length > 0 || settingsOpen || !!run }, [request]);

  const offer = state && selectedRelease && availableUpdates(state).find(release =>
    release.id === selectedRelease.id && release.source === selectedRelease.source);
  const plan = selection === null ? rows : rows.filter(row => selection.includes(row.id)).map(row =>
    row.local && selectedRelease && !row.skipped
      ? { ...row, target: offer?.version ?? null, source: offer?.source, skipped: offer ? null : 'Update unavailable' }
      : row);

  const previous = legacy.current;
  const recoveryPlan = previous && state && state.launcherAvailable && (!state.lastAttempt || terminalUpdate(state.lastAttempt.status)) ? rows.filter(row => row.local).map(row => ({
    ...row, target: previous.version, source: previous.source, skipped: null,
  })) : null;
  return {
    state, rows, downloads, plan: recoveryPlan ?? plan, ready: ready || !!recoveryPlan || !!storageError,
    run, active, open, show, dismiss, start, error, reconnecting, planning: planning && !recoveryPlan, storageError, discardRecovery,
    localPending: !!previous, lastAttempt: state?.lastAttempt,
  };
}

export type UpdateEverythingState = ReturnType<typeof useUpdateEverything>;
