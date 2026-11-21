// Adapted from T3 DesktopUpdates.ts and ElectronUpdater.ts; Effect removed.
export const UPDATE_INTERVAL_MS = 6 * 60 * 60 * 1000;
export function configureUpdates({ updater, enabled, prompt, log = console.warn, timers = globalThis }) {
  updater.autoDownload = true;
  updater.autoInstallOnAppQuit = false;
  updater.allowDowngrade = false;
  let checking = null;
  let downloaded = false;
  let prompting = false;
  const reportError = () => log('Minarbot desktop update failed. Retry from the app menu.');
  const offerRestart = async () => {
    if (prompting) return;
    prompting = true;
    try {
      if (await prompt({ title: 'Update ready', message: 'Minarbot update downloaded.', buttons: ['Restart to update', 'Later'], defaultId: 1, cancelId: 1 }) === 0) updater.quitAndInstall(false, true);
    } catch { reportError(); } finally { prompting = false; }
  };
  updater.on('error', reportError);
  const onDownloaded = () => { downloaded = true; void offerRestart(); };
  updater.on('update-downloaded', onDownloaded);
  async function check(manual = false) {
    if (!enabled) {
      if (manual) await prompt({ title: 'Updates unavailable', message: 'Updates require an installed desktop release.', buttons: ['OK'] });
      return;
    }
    if (downloaded) { if (manual) await offerRestart(); return; }
    try {
      checking ??= updater.checkForUpdates().finally(() => { checking = null; });
      const result = await checking;
      if (manual) await prompt({ title: 'Updates', message: result?.isUpdateAvailable ? 'Update found. Downloading in the background.' : 'Minarbot is up to date.', buttons: ['OK'] });
    } catch { if (manual) await prompt({ title: 'Update check failed', message: 'Could not check for updates on GitHub. Try again later.', buttons: ['OK'] }); }
  }
  let interval;
  if (enabled) { void check(); interval = timers.setInterval(() => { void check(); }, UPDATE_INTERVAL_MS); }
  return { check, stop: () => { if (interval !== undefined) timers.clearInterval(interval); updater.removeListener('error', reportError); updater.removeListener('update-downloaded', onDownloaded); } };
}
