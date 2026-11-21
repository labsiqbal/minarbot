// Adapted from T3 apps/desktop. Source files and changes: ../provenance.json.
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { app, BrowserWindow, Menu, dialog, ipcMain, shell, session, screen } from 'electron';
import electronUpdater from 'electron-updater';
import { addServer, readSettings, removeServer, resolveWindowBounds, writeSettings } from './settings.mjs';
import { canGrantPermission, canNotify, isAllowedExternal, isAllowedNavigation, parseServerUrl, trustedSender, validateServer } from './policy.mjs';
import { installContextMenu } from './context-menu.mjs';
import { configureUpdates } from './updates.mjs';
import { configureMacUpdates } from './mac-updates.mjs';
import { localServer, startWindowsServer } from './wsl.mjs';

const isDev = !app.isPackaged;
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const connectPage = path.join(root, 'src/connect.html');
const connectUrl = pathToFileURL(connectPage).href;
let settings;
let mainWindow;
let updates;
let configuredUrl = null;
let connectionGeneration = 0;
function persist(value) { writeSettings(app.getPath('userData'), value); settings = value; }
function report(message) { dialog.showErrorBox('Minarbot', message); }
function showConnect(errorMessage = '') {
  connectionGeneration++;
  configuredUrl = null;
  void mainWindow.loadFile(connectPage, { query: { error: errorMessage } }).catch(() => report('Could not load the connection page.'));
}
async function connect(value) {
  const generation = ++connectionGeneration;
  try {
    const url = await validateServer(value);
    if (generation !== connectionGeneration || !mainWindow) return;
    persist(addServer(settings, url));
    buildMenu();
    configuredUrl = url;
    await mainWindow.loadURL(url);
  } catch (error) {
    if (generation === connectionGeneration && mainWindow) showConnect(error instanceof TypeError ? 'Could not reach the server. Check the address and connection.' : error.message);
  }
}
function openExternal(value) {
  if (!isAllowedExternal(value)) throw new Error('Only HTTP and HTTPS links can open externally.');
  return shell.openExternal(value);
}
function openLink(value) { if (isAllowedExternal(value)) void openExternal(value).catch(() => report('Could not open the link in your browser.')); }
async function connectWindowsServer() {
  const generation = connectionGeneration;
  try {
    await startWindowsServer(process.resourcesPath, app.getVersion());
    if (mainWindow && generation === connectionGeneration) await connect(localServer);
  } catch (error) {
    if (!mainWindow || generation !== connectionGeneration) return;
    const { response } = await dialog.showMessageBox(mainWindow, {
      title: 'Minarbot local server', message: error.message,
      buttons: ['WSL setup guide', 'Close'], defaultId: 1, cancelId: 1,
    });
    if (response === 0) openLink('https://learn.microsoft.com/windows/wsl/install');
  }
}
function saveWindowState() {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  try { persist({ ...settings, window: mainWindow.getNormalBounds() }); }
  catch { report('Could not save window settings. Existing settings were preserved.'); }
}
function buildMenu() {
  const servers = settings.servers.map((url) => ({ label: url, type: 'radio', checked: url === settings.activeServer, click: () => { void connect(url); } }));
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: 'Minarbot', submenu: [{ label: 'About Minarbot', role: 'about' }, { label: 'Check for Updates...', click: () => { void updates.check(true); } }, { type: 'separator' }, { role: 'quit' }] },
    { label: 'Servers', submenu: [{ label: 'Connect to Server...', click: () => showConnect() },
      ...(!isDev && process.platform === 'win32' ? [{ label: 'Start WSL Server', click: () => void connectWindowsServer() }] : []), ...servers,
      { label: 'Remove Server', enabled: servers.length > 0, submenu: settings.servers.map((url) => ({ label: url, click: async () => {
        const { response } = await dialog.showMessageBox(mainWindow, { message: 'Remove this saved server?', detail: url, buttons: ['Remove', 'Cancel'], defaultId: 1, cancelId: 1 });
        if (response !== 0) return;
        try { persist(removeServer(settings, url)); buildMenu(); settings.activeServer ? void connect(settings.activeServer) : showConnect(); }
        catch { report('Could not remove the saved server. Existing settings were preserved.'); }
      } })) }] },
    { label: 'Edit', submenu: [{ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' }] },
    { label: 'View', submenu: [{ role: 'reload' }, { role: 'resetZoom' }, { role: 'zoomIn' }, { role: 'zoomOut' }, { role: 'togglefullscreen' }, ...(isDev ? [{ type: 'separator' }, { role: 'toggleDevTools' }] : [])] },
  ]));
}
function configurePermissions() {
  session.defaultSession.setPermissionRequestHandler((contents, permission, done, details) => {
    done(contents === mainWindow?.webContents && canGrantPermission(permission, details.requestingUrl, configuredUrl, details));
  });
  session.defaultSession.setPermissionCheckHandler((contents, permission, origin, details) =>
    permission === 'notifications' ? canNotify(permission, origin, configuredUrl) :
      contents === mainWindow?.webContents && canGrantPermission(permission, origin, configuredUrl, details));
}
function createWindow() {
  mainWindow = new BrowserWindow({ ...resolveWindowBounds(settings.window, screen.getAllDisplays().map((display) => display.workArea)),
    minWidth: 640, minHeight: 480, show: false, title: 'Minarbot', backgroundColor: '#111113', icon: path.join(root, 'assets/icon.png'),
    webPreferences: { preload: path.join(root, 'src/preload.cjs'), contextIsolation: true, nodeIntegration: false, sandbox: true, devTools: isDev },
  });
  mainWindow.once('ready-to-show', () => mainWindow.show());
  mainWindow.on('close', saveWindowState);
  mainWindow.on('closed', () => { mainWindow = null; });
  const contents = mainWindow.webContents;
  installContextMenu(mainWindow, contents, () => configuredUrl, connectUrl);
  contents.setWindowOpenHandler(({ url }) => { openLink(url); return { action: 'deny' }; });
  contents.on('will-attach-webview', (event) => event.preventDefault());
  contents.on('will-frame-navigate', (event) => {
    if (event.isMainFrame && event.url.startsWith('minarbot-connect:') && (contents.getURL() === connectUrl || contents.getURL().startsWith(`${connectUrl}?`))) {
      event.preventDefault();
      try { void connect(decodeURIComponent(event.url.slice('minarbot-connect:'.length))); } catch { showConnect('Enter a valid server URL.'); }
      return;
    }
    if (isAllowedNavigation(event.url, configuredUrl)) return;
    event.preventDefault();
    if (event.isMainFrame) openLink(event.url);
  });
  contents.on('will-redirect', (event, url, _inPlace, isMainFrame) => {
    if (!isAllowedNavigation(url, configuredUrl)) { event.preventDefault(); if (isMainFrame) openLink(url); }
  });
  contents.on('did-fail-load', (_event, code, _description, _url, isMainFrame) => {
    if (isMainFrame && code !== -3 && configuredUrl) showConnect('Could not load the server. Check its address and connection, then reconnect.');
  });
  // ponytail: no custom notification transport; Chromium's Notification API uses the native OS service.
  if (!isDev && process.platform === 'win32' && (!settings.activeServer || settings.activeServer === localServer)) {
    showConnect();
    void connectWindowsServer();
  } else if (settings.activeServer) void connect(settings.activeServer);
  else showConnect();
}
function registerIpc() {
  const handle = (channel, callback) => ipcMain.handle(channel, (event, ...args) => {
    if (!trustedSender(event, mainWindow?.webContents, configuredUrl, connectUrl)) throw new Error('Untrusted desktop IPC sender.');
    return callback(...args);
  });
  handle('app-version', () => app.getVersion());
  handle('check-for-updates', () => updates.check(true));
  handle('open-external', openExternal);
}
if (!app.requestSingleInstanceLock()) app.quit();
else {
  app.on('second-instance', () => { if (mainWindow) { if (mainWindow.isMinimized()) mainWindow.restore(); mainWindow.show(); mainWindow.focus(); } });
  app.whenReady().then(() => {
    settings = readSettings(app.getPath('userData'));
    // Dev URL is explicit and validated; packaged builds use saved servers only.
    const devServer = isDev && process.env.MINARBOT_SERVER_URL;
    if (devServer) settings = addServer(settings, parseServerUrl(devServer).href);
    const prompt = async (options) => (await dialog.showMessageBox(options)).response;
    updates = app.isPackaged && process.platform === 'darwin'
      ? configureMacUpdates({ version: app.getVersion(), arch: process.arch, downloads: app.getPath('downloads'), open: shell.openPath, prompt })
      : configureUpdates({ updater: electronUpdater.autoUpdater, enabled: app.isPackaged && (Boolean(process.env.APPIMAGE) || process.platform === 'win32'), prompt });
    configurePermissions(); registerIpc(); createWindow(); buildMenu();
    app.on('activate', () => { if (!mainWindow) createWindow(); });
  }).catch((error) => { report(error.message); app.quit(); });
  app.on('before-quit', () => { updates?.stop(); });
  app.on('window-all-closed', () => { if (process.platform !== 'darwin') app.quit(); });
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => app.quit());
}
