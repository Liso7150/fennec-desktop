'use strict';
const { app, BrowserWindow, ipcMain, dialog, shell, Menu } = require('electron');
const { execFile } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const { FennecBackend, listModels } = require('./backend');
const { Accelerator } = require('./npu');
const { runTurn, Cancelled } = require('./agent');
const { resolveInside } = require('./tools');
const { safeExternalUrl } = require('./security');
const { INSTALLER_URL } = require('./config');
const { SpeechToText } = require('./stt');
const setup = require('./setup');
const { checkDesktop, refreshDesktop, updateSnapshot, updateSuite } = require('./updates');

const isDev = process.env.ELECTRON_DEV === '1';
const backend = new FennecBackend();
const accelerator = new Accelerator(backend.config);
const speech = new SpeechToText(backend.config);
let win = null;
let currentTurn = null;
const pendingApprovals = new Map();

const settingsFile = () => path.join(app.getPath('userData'), 'settings.json');
const defaultSettings = () => ({
  workspace: backend.config.workspace,
  modelDir: backend.config.model,
  port: backend.port,
  kvSlots: 1,
  temperature: 0.2,
  topP: 0.9,
  maxTokens: 4096,
  autoApproveWrites: false,
  autoApproveCommands: false,
  recentWorkspaces: [],
  devicePolicy: 'auto',
  npuDevice: 'NPU',
  npuEnabled: true,
});

function loadSettings() {
  try {
    const saved = JSON.parse(fs.readFileSync(settingsFile(), 'utf8'));
    return { ...defaultSettings(), ...saved,
      // A legacy saved port/model must not override the suite's configured engine.
      port: backend.port, modelDir: backend.config.model || saved.modelDir || '',
      autoApproveWrites: saved.autoApproveWrites === true,
      autoApproveCommands: saved.autoApproveCommands === true };
  } catch {
    return defaultSettings();
  }
}

function saveSettings(next) {
  const merged = { ...loadSettings(), ...next };
  fs.mkdirSync(path.dirname(settingsFile()), { recursive: true });
  fs.writeFileSync(settingsFile(), JSON.stringify(merged, null, 2));
  return loadSettings();
}

function openExternal(value) {
  const url = safeExternalUrl(value);
  if (!url) return Promise.resolve(false);
  return shell.openExternal(url).then(() => true, () => false);
}

function createWindow() {
  win = new BrowserWindow({
    width: 1500,
    height: 980,
    minWidth: 1040,
    minHeight: 700,
    backgroundColor: '#0b0d10',
    title: 'Fennec Desktop',
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      spellcheck: false,
    },
  });

  win.once('ready-to-show', () => win.show());
  win.webContents.setWindowOpenHandler(({ url }) => {
    void openExternal(url);
    return { action: 'deny' };
  });
  win.webContents.on('will-navigate', (event, url) => {
    event.preventDefault();
    void openExternal(url);
  });
  win.webContents.on('will-frame-navigate', (event) => event.preventDefault());
  win.webContents.on('will-redirect', (event) => event.preventDefault());
  win.webContents.on('will-attach-webview', (event) => event.preventDefault());
  // Microphone is used only by this window's speech-recognition button.
  win.webContents.session.setPermissionRequestHandler((contents, permission, callback) => {
    callback(permission === 'media' && contents === win.webContents);
  });

  if (isDev) {
    win.loadURL('http://127.0.0.1:5173');
  } else {
    win.loadFile(path.join(__dirname, '..', 'dist', 'index.html'));
  }
}

function send(channel, payload) {
  if (win && !win.isDestroyed()) win.webContents.send(channel, payload);
}

function execCapture(command, args, { timeout = 120000 } = {}) {
  return new Promise((resolve) => {
    execFile(command, args, { timeout, maxBuffer: 16 * 1024 * 1024, windowsHide: true },
      (err, stdout, stderr) => resolve({
        code: err ? (err.code ?? 1) : 0,
        stdout: stdout || '',
        stderr: stderr || (err && err.message) || '',
      }));
  });
}

/**
 * Release feed check. Runs at startup and again when the window is focused or
 * an engine starts; updates.js caches the answer, so this stays one API call
 * per refresh window and never blocks or nags when GitHub is unreachable.
 */
function refreshUpdateInfo(options) {
  return refreshDesktop(app.getVersion(), options).then((info) => {
    send('update:info', info);
    return info;
  });
}

function buildSystemPrompt(workspace, extraContext) {
  const parts = [
    'You are Fennec Desktop, a coding agent running entirely on the user\'s own machine.',
    'The model behind you is served by the Colibri engine, which pages expert weights from disk on demand.',
    'You work by calling tools: read files, edit them, search the tree, and run commands. Prefer tools over guessing.',
    'Rules:',
    '- Read a file before editing it. Match existing style, imports and naming.',
    '- Keep edits minimal and complete. Never leave TODOs, stubs, or dead code.',
    '- Use todo_write to plan work with 3+ steps, and update it as you go.',
    '- After code changes, run the project\'s own checks (build, lint, tests) and fix what fails.',
    '- Paths are workspace-relative. Never write outside the workspace.',
    '- Report what you changed and what you verified. Do not claim a check passed unless you ran it.',
  ];
  if (workspace) parts.push(`\nWorkspace root: ${workspace}`);
  if (extraContext) parts.push(`\nFiles the user attached for context:\n${extraContext}`);
  return parts.join('\n');
}

async function workspaceSnapshot(workspace) {
  if (!workspace) return null;
  try {
    const entries = await fsp.readdir(workspace, { withFileTypes: true });
    const names = entries
      .filter((e) => !['node_modules', '.git', 'dist', 'build', '__pycache__', '.venv'].includes(e.name))
      .slice(0, 80)
      .map((e) => (e.isDirectory() ? `${e.name}/` : e.name));
    return names.join('\n');
  } catch {
    return null;
  }
}

/* Start the engine only when a turn actually needs it. */
async function ensureEngineForTurn(settings) {
  if (backend.state === 'ready') return;
  let modelDir = settings.modelDir;
  if (!modelDir) {
    const ready = (await listModels(backend.config)).find((m) => m.ready);
    if (!ready) throw new Error('no local model found. Run the Fennec installer, then pick a model in the Engine tab.');
    modelDir = ready.dir;
    saveSettings({ modelDir });
  }
  await backend.ensureRunning(modelDir, { kvSlots: settings.kvSlots, maxTokens: settings.maxTokens });
}

/* ------------------------------------------------------------------ ipc */

ipcMain.handle('app:info', () => ({
  version: app.getVersion(),
  settings: loadSettings(),
  backend: backend.describe(),
  accelerator: accelerator.status(),
  setup: setup.detectSetup(),
  packaged: app.isPackaged,
}));

ipcMain.handle('settings:save', (_e, patch) => saveSettings(patch));
ipcMain.handle('app:open-setup', () => openExternal(INSTALLER_URL));
ipcMain.handle('app:open-workspace', () => openExternal(backend.config.web_url));

/* ------------------------------------------------------------- setup wizard */

ipcMain.handle('setup:detect', () => setup.detectSetup());

ipcMain.handle('setup:pick-folder', async (_e, { title, defaultPath } = {}) => {
  const res = await dialog.showOpenDialog(win, {
    title: typeof title === 'string' ? title : 'Choose a folder',
    defaultPath: typeof defaultPath === 'string' && defaultPath ? defaultPath : undefined,
    properties: ['openDirectory', 'createDirectory'],
  });
  return res.canceled || !res.filePaths.length ? null : res.filePaths[0];
});

ipcMain.handle('setup:validate-root', (_e, directory) => setup.validateRoot(directory));

ipcMain.handle('setup:prerequisites', (_e, { python } = {}) =>
  setup.prerequisitePlan({ python: typeof python === 'string' ? python : '' }));

ipcMain.handle('setup:install-prerequisite', async (_e, { id } = {}) => {
  const item = setup.prerequisitePlan({}).find((entry) => entry.id === id);
  if (!item) return { ok: false, error: 'Unknown prerequisite' };
  if (item.installed) return { ok: true, output: `${item.name} is already installed.` };
  if (!item.install) {
    return { ok: false, manual: true, url: item.manual, error: `${item.name} needs a manual install.` };
  }
  if (item.install.requiresRoot) {
    return { ok: false, manual: true, url: item.manual,
      command: [item.install.command, ...item.install.args].join(' '),
      error: `${item.name} needs administrator rights; run the shown command in a terminal.` };
  }
  const res = await execCapture(item.install.command, item.install.args, { timeout: 15 * 60 * 1000 });
  return { ok: res.code === 0, output: `${res.stdout}${res.stderr}`.trim().slice(-8000) };
});

ipcMain.handle('setup:find-python', async () => {
  const tried = [];
  for (const candidate of setup.pythonCandidates()) {
    tried.push(candidate.command);
    const res = await execCapture(candidate.command, [...candidate.args, '-c', setup.PYTHON_PROBE], { timeout: 20000 });
    if (res.code !== 0) continue;
    const info = setup.parsePythonProbe(res.stdout);
    if (!info.ok) continue;
    return { ...info, command: candidate.command, source: candidate.source, tried };
  }
  return { ok: false, tried,
    error: 'No Python 3.10+ interpreter was found. Install Python or choose its path.' };
});

ipcMain.handle('setup:check-python', async (_e, { python } = {}) => {
  if (typeof python !== 'string' || !path.isAbsolute(python) || !fs.existsSync(python)) {
    return { ok: false, error: 'Choose an absolute python.exe or bin/python3 path' };
  }
  const res = await execCapture(python, ['-c', setup.PYTHON_PROBE], { timeout: 20000 });
  if (res.code !== 0) return { ok: false, error: `${res.stderr}${res.stdout}`.trim().slice(0, 800) || 'Python did not start' };
  return { ...setup.parsePythonProbe(res.stdout), command: python, source: 'selected' };
});

ipcMain.handle('setup:ensure-pip', async (_e, { python } = {}) => {
  if (typeof python !== 'string' || !path.isAbsolute(python)) return { ok: false, error: 'Invalid interpreter' };
  const res = await execCapture(python, setup.ensurePipArgs(), { timeout: 120000 });
  return { ok: res.code === 0, output: `${res.stdout}${res.stderr}`.trim().slice(-4000) };
});

ipcMain.handle('setup:check-deps', async (_e, { python } = {}) => {
  if (typeof python !== 'string' || !path.isAbsolute(python)) return { ok: false, error: 'Invalid interpreter' };
  const res = await execCapture(python, setup.dependencyProbeArgs(), { timeout: 60000 });
  if (res.code !== 0) return { ok: false, error: `${res.stderr}${res.stdout}`.trim().slice(-2000) };
  const parsed = setup.parseDependencyProbe(res.stdout);
  return { ok: parsed.missing.length === 0, ...parsed };
});

ipcMain.handle('setup:install-deps', async (_e, { python, venv, packages } = {}) => {
  if (typeof python !== 'string' || !path.isAbsolute(python)) return { ok: false, error: 'Invalid interpreter' };
  const selected = Array.isArray(packages) && packages.length
    ? packages.filter((spec) => setup.CORE_DEPS.includes(spec))
    : setup.CORE_DEPS;
  let args;
  try { args = setup.pipInstallArgs(python, selected, { venv: venv === true }); }
  catch (err) { return { ok: false, error: err.message }; }
  const res = await execCapture(python, args, { timeout: 30 * 60 * 1000 });
  return { ok: res.code === 0, code: res.code,
    output: `${res.stdout}${res.stderr}`.trim().slice(-20000) };
});

ipcMain.handle('setup:validate-engine', (_e, directory) => setup.validateEngine(directory));

ipcMain.handle('setup:clone-engine', async (_e, { directory } = {}) => {
  if (typeof directory !== 'string' || !path.isAbsolute(directory)) {
    return { ok: false, error: 'Choose an absolute engine folder' };
  }
  if (fs.existsSync(directory) && fs.readdirSync(directory).length) {
    return { ok: false, error: 'That folder is not empty; choose an empty folder for the clone.' };
  }
  const res = await execCapture('git', setup.cloneEngineArgs(directory), { timeout: 15 * 60 * 1000 });
  return { ok: res.code === 0, output: `${res.stdout}${res.stderr}`.trim().slice(-8000) };
});

ipcMain.handle('setup:scan-models', (_e, directory) => setup.scanModels(directory));

ipcMain.handle('setup:open-download', (_e, kind) => {
  const url = setup.DOWNLOADS[kind];
  return url ? openExternal(url) : Promise.resolve(false);
});

ipcMain.handle('setup:save', async (_e, { values } = {}) => {
  try {
    const config = setup.buildConfig(values);
    const result = setup.writeConfigAtomic(setup.configLocation(), config);
    setTimeout(() => { app.relaunch(); app.exit(0); }, 600);
    return { ok: true, ...result, config };
  } catch (err) {
    return { ok: false, error: err.message };
  }
});

ipcMain.handle('workspace:pick', async () => {
  const res = await dialog.showOpenDialog(win, { properties: ['openDirectory'] });
  if (res.canceled || !res.filePaths.length) return null;
  const workspace = res.filePaths[0];
  const settings = loadSettings();
  const recent = [workspace, ...(settings.recentWorkspaces || []).filter((w) => w !== workspace)].slice(0, 8);
  saveSettings({ workspace, recentWorkspaces: recent });
  return { workspace, recentWorkspaces: recent, snapshot: await workspaceSnapshot(workspace) };
});

ipcMain.handle('workspace:use', async (_e, workspace) => {
  saveSettings({ workspace });
  return { workspace, snapshot: await workspaceSnapshot(workspace) };
});

ipcMain.handle('workspace:snapshot', (_e, workspace) => workspaceSnapshot(workspace));

ipcMain.handle('models:list', async () => {
  const models = await listModels(backend.config);
  return models.map((m) => ({ ...m, selected: m.dir === loadSettings().modelDir }));
});

ipcMain.handle('engine:describe', () => backend.describe());

ipcMain.handle('engine:start', async (_e, { modelDir, port, kvSlots }) => {
  const settings = saveSettings({ modelDir, port, kvSlots });
  void refreshUpdateInfo(); // "on use" check: cheap while the engine loads.
  try {
    const described = await backend.start(modelDir, { port, kvSlots, maxTokens: settings.maxTokens });
    return { ok: true, backend: described, settings };
  } catch (err) {
    return { ok: false, error: err.message, backend: backend.describe(), log: backend.tailLog(60) };
  }
});

ipcMain.handle('engine:stop', async () => {
  if (currentTurn) currentTurn.abort();
  for (const [, resolve] of pendingApprovals) resolve(false);
  pendingApprovals.clear();
  await backend.stop();
  return backend.describe();
});
ipcMain.handle('engine:cleanupOrphans', async () => backend.cleanupOrphans());

ipcMain.handle('engine:stats', async () => backend.stats());
ipcMain.handle('engine:log', () => backend.tailLog(200));
ipcMain.handle('engine:doctor', async (_e, modelDir) => backend.doctor(modelDir));

/* accelerator sidecar: NPU / iGPU host probe + code index, never token decode */
const sidecarOff = () => ({ ok: false, error: 'Sidecar is disabled — tick "Keep the NPU available for sidecar work".' });
ipcMain.handle('npu:status', () => accelerator.status());
ipcMain.handle('npu:probe', async () => (loadSettings().npuEnabled === false ? sidecarOff() : accelerator.probe()));
ipcMain.handle('npu:index', async (_e, device) => {
  if (loadSettings().npuEnabled === false) return sidecarOff();
  const { workspace } = loadSettings();
  return accelerator.index(workspace, device || loadSettings().npuDevice || 'NPU');
});
ipcMain.handle('npu:ensure', async (_e, options) => {
  if (loadSettings().npuEnabled === false) return sidecarOff();
  return accelerator.install({ includeModelTooling: Boolean(options && options.includeModelTooling) });
});
ipcMain.handle('npu:model', async () => {
  if (loadSettings().npuEnabled === false) return sidecarOff();
  return accelerator.fetchEmbedModel();
});

ipcMain.handle('file:read', async (_e, rel) => {
  const { workspace } = loadSettings();
  if (!workspace) throw new Error('no workspace open');
  const abs = resolveInside(workspace, rel);
  return { path: rel, content: await fsp.readFile(abs, 'utf8') };
});

ipcMain.handle('file:tree', async () => {
  const { workspace } = loadSettings();
  if (!workspace) return [];
  return await buildTree(workspace, workspace, 0);
});

async function buildTree(root, dir, depth) {
  if (depth > 4) return [];
  let entries = [];
  try {
    entries = await fsp.readdir(dir, { withFileTypes: true });
  } catch {
    return [];
  }
  entries.sort((a, b) => Number(b.isDirectory()) - Number(a.isDirectory()) || a.name.localeCompare(b.name));
  const out = [];
  for (const entry of entries.slice(0, 300)) {
    if (['node_modules', '.git', 'dist', 'build', '__pycache__', '.venv', 'release'].includes(entry.name)) continue;
    const full = path.join(dir, entry.name);
    const rel = path.relative(root, full).replace(/\\/g, '/');
    if (entry.isDirectory()) {
      out.push({ name: entry.name, path: rel, dir: true, children: await buildTree(root, full, depth + 1) });
    } else {
      out.push({ name: entry.name, path: rel, dir: false });
    }
  }
  return out;
}

ipcMain.handle('shell:reveal', (_e, rel) => {
  const { workspace } = loadSettings();
  if (!workspace) return;
  shell.showItemInFolder(resolveInside(workspace, rel));
});

ipcMain.handle('approval:reply', (_e, { id, approved }) => {
  const resolve = pendingApprovals.get(id);
  if (resolve) {
    pendingApprovals.delete(id);
    resolve(Boolean(approved));
  }
  return true;
});

ipcMain.handle('agent:send', async (_e, { messages, extraContext }) => {
  const settings = loadSettings();
  if (!settings.workspace) throw new Error('open a folder first');
  if (currentTurn) throw new Error('a turn is already running');
  try {
    await ensureEngineForTurn(settings);
  } catch (err) {
    return { ok: false, error: `The local engine could not start: ${err.message}` };
  }

  const controller = new AbortController();
  currentTurn = controller;

  const approve = ({ id, name, args }) =>
    new Promise((resolve) => {
      pendingApprovals.set(id, resolve);
      send('approval:request', { id, name, args });
    });

  try {
    const result = await runTurn({
      engineUrl: backend.engineUrl,
      model: backend.modelId,
      apiKey: backend.apiKey,
      workspace: settings.workspace,
      messages,
      systemPrompt: buildSystemPrompt(settings.workspace, extraContext),
      settings,
      signal: controller.signal,
      approve,
      onEvent: (event) => send('agent:event', event),
    });
    return { ok: true, result };
  } catch (err) {
    if (err instanceof Cancelled) return { ok: false, cancelled: true };
    send('agent:event', { type: 'error', message: err.message });
    return { ok: false, error: err.message };
  } finally {
    currentTurn = null;
    for (const [, resolve] of pendingApprovals) resolve(false);
    pendingApprovals.clear();
  }
});

ipcMain.handle('agent:stop', () => {
  if (currentTurn) currentTurn.abort();
  return true;
});

ipcMain.handle('stt:transcribe', async (_e, { audio, type }) => {
  try {
    const bytes = Buffer.from(audio);
    if (!bytes.length || bytes.length > (16 << 20)) throw new Error('recording must be 1 byte to 16 MiB');
    const result = await speech.transcribe(bytes, typeof type === 'string' && type ? type : 'audio/webm');
    return { ok: true, text: result.text, language: result.language, duration: result.duration_s };
  } catch (err) {
    return { ok: false, error: err.message };
  }
});

ipcMain.handle('update:check', async () => refreshUpdateInfo({ force: true }));

ipcMain.handle('update:apps', async () => {
  try {
    return { ok: true, ...(await updateSuite(backend.config.suite_root)) };
  } catch (err) {
    return { ok: false, error: err.message, results: [] };
  }
});

ipcMain.handle('update:open', (_event, value) => {
  if (typeof value !== 'string' || !/^https:\/\/github\.com\/Liso7150\/fennec-desktop(\/|$)/.test(value)) {
    return Promise.resolve(false);
  }
  return openExternal(value);
});

/* ------------------------------------------------------------------ lifecycle */

app.whenReady().then(async () => {
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    ...(process.platform === 'darwin' ? [{ role: 'appMenu' }] : []),
    { label: 'Fennec', submenu: [
      { label: 'Open shared web workspace', click: () => { void openExternal(backend.config.web_url); } },
      { label: 'Install shared runtime and models', click: () => { void openExternal(INSTALLER_URL); } },
      { label: 'Run setup wizard…', click: () => send('setup:open') },
      { type: 'separator' },
      { label: 'Check for updates…', click: async () => {
        try {
          const info = await checkDesktop(app.getVersion());
          const answer = await dialog.showMessageBox(win, {
            type: 'info',
            title: 'Fennec Desktop updates',
            message: info.upToDate ? `Fennec Desktop ${info.current} is current.` : `Version ${info.latest} is available (you have ${info.current}).`,
            detail: info.notes || info.url,
            buttons: info.upToDate ? ['OK'] : ['Open release page', 'Later'],
            defaultId: 0,
          });
          if (!info.upToDate && answer.response === 0) await openExternal(info.url);
        } catch (err) {
          dialog.showErrorBox('Update check failed', err.message);
        }
      } },
      { label: 'Open releases on GitHub', click: () => { void openExternal('https://github.com/Liso7150/fennec-desktop/releases'); } },
      { type: 'separator' },
      { label: 'Engine base idea: JustVugg/colibri', click: () => { void openExternal('https://github.com/JustVugg/colibri'); } },
      { label: 'Fine-tuning: lyogavin/airllm', click: () => { void openExternal('https://github.com/lyogavin/airllm'); } },
      { role: 'quit' },
    ] },
    { role: 'editMenu' },
  ]));
  await backend.attach(); // Probe only; opening the app never loads a model.
  createWindow();
  void refreshUpdateInfo(); // startup check, silent and cached
  const timer = setInterval(() => void refreshUpdateInfo(), 6 * 60 * 60 * 1000);
  if (typeof timer.unref === 'function') timer.unref();
  win.on('focus', () => void refreshUpdateInfo());
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => app.quit());
let quitting = false;
let cleanedUp = false;
app.on('before-quit', (event) => {
  if (cleanedUp) return;
  event.preventDefault();
  if (quitting) return;
  quitting = true;
  if (currentTurn) currentTurn.abort();
  for (const [, resolve] of pendingApprovals) resolve(false);
  pendingApprovals.clear();
  speech.stop();
  backend.stop().then(() => {
    cleanedUp = true;
    app.quit();
  }).catch((err) => {
    quitting = false;
    dialog.showErrorBox('Engine cleanup failed', `Could not stop the owned engine: ${err.message}. Try Stop again.`);
  });
});
