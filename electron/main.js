'use strict';
const { app, BrowserWindow, ipcMain, dialog, shell, Menu } = require('electron');
const path = require('node:path');
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const { ColibriBackend, listModels } = require('./backend');
const { Accelerator } = require('./npu');
const { runTurn, Cancelled } = require('./agent');
const { resolveInside } = require('./tools');

const isDev = process.env.ELECTRON_DEV === '1';
const backend = new ColibriBackend();
const accelerator = new Accelerator();
let win = null;
let currentTurn = null;
const pendingApprovals = new Map();

const settingsFile = () => path.join(app.getPath('userData'), 'settings.json');
const defaultSettings = () => ({
  workspace: '',
  modelDir: '',
  port: 8765,
  kvSlots: 1,
  temperature: 0.2,
  topP: 0.9,
  maxTokens: 4096,
  autoApproveWrites: true,
  autoApproveCommands: false,
  recentWorkspaces: [],
  devicePolicy: 'auto',
  npuDevice: 'NPU',
  npuEnabled: true,
});

function loadSettings() {
  try {
    return { ...defaultSettings(), ...JSON.parse(fs.readFileSync(settingsFile(), 'utf8')) };
  } catch {
    return defaultSettings();
  }
}

function saveSettings(next) {
  const merged = { ...loadSettings(), ...next };
  fs.mkdirSync(path.dirname(settingsFile()), { recursive: true });
  fs.writeFileSync(settingsFile(), JSON.stringify(merged, null, 2));
  return merged;
}

function createWindow() {
  win = new BrowserWindow({
    width: 1500,
    height: 980,
    minWidth: 1040,
    minHeight: 700,
    backgroundColor: '#0b0d10',
    title: 'Colibri Code',
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      spellcheck: false,
    },
  });

  win.once('ready-to-show', () => win.show());
  win.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
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

function buildSystemPrompt(workspace, extraContext) {
  const parts = [
    'You are Colibri Code, a coding agent running entirely on the user\'s own machine.',
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

/* ------------------------------------------------------------------ ipc */

ipcMain.handle('app:info', () => ({
  version: app.getVersion(),
  settings: loadSettings(),
  backend: backend.describe(),
  accelerator: accelerator.status(),
  packaged: app.isPackaged,
}));

ipcMain.handle('settings:save', (_e, patch) => saveSettings(patch));

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
  const models = await listModels();
  return models.map((m) => ({ ...m, selected: m.dir === loadSettings().modelDir }));
});

ipcMain.handle('engine:describe', () => backend.describe());

ipcMain.handle('engine:start', async (_e, { modelDir, port, kvSlots }) => {
  const settings = saveSettings({ modelDir, port, kvSlots });
  try {
    const described = await backend.start(modelDir, { port, kvSlots });
    return { ok: true, backend: described, settings };
  } catch (err) {
    return { ok: false, error: err.message, backend: backend.describe(), log: backend.tailLog(60) };
  }
});

ipcMain.handle('engine:stop', async () => {
  await backend.stop();
  return backend.describe();
});

ipcMain.handle('engine:stats', async () => backend.stats());
ipcMain.handle('engine:log', () => backend.tailLog(200));
ipcMain.handle('engine:doctor', async (_e, modelDir) => backend.doctor(modelDir));

/* accelerator sidecar: NPU / iGPU host probe + code index, never token decode */
ipcMain.handle('npu:status', () => accelerator.status());
ipcMain.handle('npu:probe', async () => accelerator.probe());
ipcMain.handle('npu:index', async (_e, device) => {
  const { workspace } = loadSettings();
  return accelerator.index(workspace, device || loadSettings().npuDevice || 'NPU');
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
  if (backend.state !== 'ready') throw new Error('start the Colibri engine first');
  if (currentTurn) throw new Error('a turn is already running');

  const controller = new AbortController();
  currentTurn = controller;

  const approve = ({ id, name, args }) =>
    new Promise((resolve) => {
      pendingApprovals.set(id, resolve);
      send('approval:request', { id, name, args });
    });

  try {
    const result = await runTurn({
      port: settings.port,
      model: backend.modelId || settings.modelDir,
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

/* ------------------------------------------------------------------ lifecycle */

app.whenReady().then(() => {
  Menu.setApplicationMenu(null);
  createWindow();
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', async () => {
  await backend.stop().catch(() => {});
  app.quit();
});

app.on('before-quit', async () => {
  await backend.stop().catch(() => {});
});
