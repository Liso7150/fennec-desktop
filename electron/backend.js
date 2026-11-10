'use strict';
// Attach to the shared engine first. Only a child spawned here is ours to stop.
const { spawn, execFile } = require('node:child_process');
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const path = require('node:path');
const http = require('node:http');
const https = require('node:https');
const net = require('node:net');
const { loadConfig, runtimePaths, engineEnvironment, isLoopback, INSTALLER_URL } = require('./config');

function exists(p) { return Boolean(p) && fs.existsSync(p); }
function findRuntime(config = loadConfig()) { return runtimePaths(config).runtime; }
function findPython(config = loadConfig()) { return exists(config.python) ? config.python : null; }

/** Inventory only; actual compatibility is checked by coli doctor. */
async function listModels(config = loadConfig()) {
  const found = new Map();
  const add = async (dir, name) => {
    const meta = await describeModel(dir, name);
    if (meta) found.set(dir, meta);
  };
  if (config.model) await add(config.model, path.basename(config.model));
  const roots = [config.models_root, path.join(config.models_root, 'containers'), path.join(config.cache_root, 'huggingface', 'hub')];
  for (const root of roots) {
    let entries;
    try { entries = await fsp.readdir(root, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const dir = path.join(root, entry.name);
      if (entry.name.startsWith('models--')) {
        const snapRoot = path.join(dir, 'snapshots');
        let shas;
        try { shas = await fsp.readdir(snapRoot); } catch { continue; }
        for (const sha of shas) {
          await add(path.join(snapRoot, sha), `${entry.name.replace(/^models--/, '').replace(/--/g, '/')} (${sha.slice(0, 7)})`);
        }
      } else await add(dir, entry.name);
    }
  }
  return [...found.entries()].map(([dir, meta]) => ({ dir, ...meta }))
    .sort((a, b) => Number(b.container) - Number(a.container) || a.name.localeCompare(b.name));
}
async function describeModel(dir, name) {
  try {
    const config = JSON.parse(await fsp.readFile(path.join(dir, 'config.json'), 'utf8'));
    const files = await fsp.readdir(dir);
    const shards = files.filter((f) => /\.safetensors$/i.test(f));
    const gguf = files.filter((f) => /\.gguf$/i.test(f));
    if (!shards.length && !gguf.length) return null;
    let bytes = 0;
    for (const f of [...shards, ...gguf]) bytes += (await fsp.stat(path.join(dir, f))).size;
    const container = files.includes('qwen36_meta.json') || files.includes('qwen38_meta.json');
    return { name, family: config.model_type || config.architectures?.[0] || 'unknown',
      layers: config.num_hidden_layers || config.text_config?.num_hidden_layers || 0,
      experts: config.num_experts || config.text_config?.num_experts || 0,
      sizeGb: +(bytes / 1e9).toFixed(2), shards: shards.length, gguf: gguf.length > 0,
      container, ready: container || gguf.length > 0 };
  } catch { return null; }
}

function request(base, route, apiKey, timeout = 2000) {
  return new Promise((resolve) => {
    const url = new URL(route, base);
    const req = (url.protocol === 'https:' ? https : http).get(url, {
      timeout, headers: apiKey ? { Authorization: `Bearer ${apiKey}` } : {},
    }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => {
        body += chunk;
        if (body.length > 2 * 1024 * 1024) req.destroy(new Error('Response too large'));
      });
      res.on('error', (err) => resolve({ status: res.statusCode, body: '', error: err.message }));
      res.on('end', () => resolve({ status: res.statusCode, body }));
    });
    req.on('error', (err) => resolve({ status: 0, body: '', error: err.code || err.message }));
    req.on('timeout', () => req.destroy(new Error('Request timed out')));
  });
}

async function probeServer(base, apiKey) {
  const health = await request(base, '/health', apiKey);
  if (!health.status) return { kind: health.error === 'ECONNREFUSED' ? 'absent' : 'blocked', message: health.error };
  if (health.status === 401 || health.status === 403) return { kind: 'unauthorized', message: 'Engine authentication failed. Set FENNEC_API_KEY.' };
  if (health.status !== 200 || safeJson(health.body)?.status !== 'ok') {
    return { kind: 'blocked', message: `Endpoint is occupied but not healthy (/health ${health.status}). No engine was started.` };
  }
  const models = await request(base, '/v1/models', apiKey);
  if (models.status === 401 || models.status === 403) return { kind: 'unauthorized', message: 'Engine authentication failed. Set FENNEC_API_KEY.' };
  const list = safeJson(models.body);
  if (models.status !== 200 || list?.object !== 'list' || !Array.isArray(list.data) || !list.data.length ||
      !list.data.every((m) => m && m.object === 'model' && typeof m.id === 'string' && m.id.trim() && m.owned_by === 'fennec')) {
    return { kind: 'foreign', message: 'Occupied endpoint did not return a valid Fennec model list. No engine was started.' };
  }
  return { kind: 'ready', modelId: list.data[0].id };
}

function assertPortAvailable(base) {
  return new Promise((resolve, reject) => {
    const url = new URL(base);
    const server = net.createServer();
    server.once('error', () => reject(new Error('Configured engine port is occupied or unavailable; no engine was started.')));
    server.listen({ host: url.hostname.replace(/^\[|\]$/g, ''), port: Number(url.port || 80), exclusive: true }, () => server.close(resolve));
  });
}

function serveArgs(config, modelDir, { kvSlots = 1, maxTokens = 4096 } = {}) {
  if (!Number.isInteger(kvSlots) || kvSlots < 1 || kvSlots > 8) throw new Error('Invalid KV slot count');
  if (!Number.isInteger(maxTokens) || maxTokens < 1) throw new Error('Invalid token limit');
  return [runtimePaths(config).cli, 'serve', '--model', modelDir, '--auto-tier', '--policy', config.policy,
    '--ctx', String(config.ctx), '--host', new URL(config.engine_url).hostname.replace(/^\[|\]$/g, ''),
    '--port', String(config.port), '--api-key', config.api_key, '--kv-slots', String(kvSlots), '--ngen', String(maxTokens)];
}

/** List engine (qwen36.exe) processes that outlived their owner on Windows.
 *  Used to clean up orphans the process tree could not reach. */
function listOrphanEnginePids(startedAt = 0, { platform = process.platform, exec = execFile } = {}) {
  if (platform !== 'win32') return Promise.resolve([]);
  const since = startedAt ? new Date(startedAt).toISOString().replace('T', ' ').replace('Z', '') : null;
  const filter = since ? `Where-Object { $_.CreationDate -gt [DateTime]::Parse('${since}') } | ` : '';
  const ps = `Get-CimInstance Win32_Process -Filter "Name='qwen36.exe'" | ${filter}Select-Object -ExpandProperty ProcessId`;
  return new Promise((resolve) => {
    exec('powershell', ['-NoProfile', '-Command', ps], { windowsHide: true, timeout: 15000 }, (err, stdout) => {
      if (err) return resolve([]);
      const pids = (stdout || '').split(/\s+/).map((s) => Number(s.trim())).filter((n) => Number.isInteger(n) && n > 0);
      resolve([...new Set(pids)]);
    });
  });
}

async function killOrphanEnginePids(pids, { platform = process.platform, exec = execFile } = {}) {
  if (platform !== 'win32') return 0;
  let killed = 0;
  for (const pid of pids) {
    await new Promise((resolve) => exec('taskkill', ['/PID', String(pid), '/T', '/F'],
      { windowsHide: true, timeout: 15000 }, (err) => { if (!err) killed += 1; resolve(); }));
  }
  return killed;
}

/** Never use coli stop/pidfiles: those may belong to another application. */
async function terminateOwnedTree(child, { platform = process.platform, exec = execFile, kill = process.kill } = {}) {
  if (!child?.pid || child.exitCode != null || child.signalCode != null) return;
  if (platform === 'win32') {
    await new Promise((resolve, reject) => exec('taskkill', ['/PID', String(child.pid), '/T', '/F'],
      { windowsHide: true, timeout: 15000 }, (err) => {
        if (err && child.exitCode == null && child.signalCode == null) reject(err); else resolve();
      }));
    return;
  }
  // POSIX spawn creates a process group. Signal only that owned group.
  try { kill(-child.pid, 'SIGTERM'); } catch (err) { if (err.code !== 'ESRCH') throw err; }
  await new Promise((resolve) => {
    const done = () => { clearTimeout(timer); child.removeListener('exit', done); resolve(); };
    const timer = setTimeout(() => {
      if (child.exitCode == null && child.signalCode == null) {
        try { kill(-child.pid, 'SIGKILL'); } catch { /* already exited */ }
      }
      done();
    }, 3000);
    child.once('exit', done);
    if (child.exitCode != null || child.signalCode != null) done();
  });
}

class FennecBackend {
  constructor({ config, spawnChild = spawn, terminate = terminateOwnedTree, probe = probeServer,
    checkPort = assertPortAvailable, startupTimeout = 15 * 60 * 1000, pollMs = 1500 } = {}) {
    try { this.config = config || loadConfig(); }
    catch (err) {
      this.configError = err.message;
      this.config = loadConfig({ env: {}, exists: () => false });
    }
    Object.assign(this, runtimePaths(this.config));
    this.python = findPython(this.config);
    this.port = this.config.port;
    this.engineUrl = this.config.engine_url;
    this.apiKey = this.config.api_key;
    this.child = null;
    this.model = null;
    this.modelId = null;
    this.logPath = null;
    this.state = 'stopped';
    this.lastError = this.configError || null;
    this.plan = null;
    this.startedAt = 0;
    this.generation = 0;
    this.orphans = 0;
    Object.assign(this, { spawnChild, terminate, probe, checkPort, startupTimeout, pollMs });
  }

  setupStatus(modelDir = this.config.model) {
    const issues = [];
    if (this.configError) issues.push(this.configError);
    if (!this.runtime) issues.push(`Engine checkout missing: ${this.cli} and ${this.server}. Run the shared installer.`);
    if (!this.python) issues.push(`Shared Python runtime missing under ${this.config.engine_root}/.venv. Run the installer or set FENNEC_PYTHON.`);
    if (!modelDir || !exists(path.join(modelDir, 'config.json'))) issues.push('Choose an installed model or set model in the shared configuration. Models are not bundled or downloaded by this app.');
    return { issues, configPath: this.config.configPath, installerUrl: INSTALLER_URL };
  }

  describe() {
    return { runtime: this.runtime, python: this.python, port: this.port, engineUrl: this.engineUrl,
      model: this.model, modelId: this.modelId, apiKey: this.apiKey ? '(set)' : '',
      owned: Boolean(this.child), state: this.state, lastError: this.lastError, plan: this.plan,
      orphans: this.orphans || 0,
      uptimeMs: this.startedAt ? Date.now() - this.startedAt : 0, logPath: this.logPath,
      setup: this.setupStatus(), backend: this.config.backend,
      connectionNote: this.state === 'ready' && !this.child
        ? 'Attached to the server’s current model. Local model selection was not loaded; Stop only detaches.' : null };
  }

  _run(args, { timeout = 180000 } = {}) {
    return new Promise((resolve) => {
      if (this.configError || !this.runtime || !this.python) {
        resolve({ code: -1, stdout: '', stderr: this.setupStatus().issues.join('\n') });
        return;
      }
      execFile(this.python, args, { cwd: this.runtime, timeout, maxBuffer: 32 * 1024 * 1024,
        windowsHide: true, env: engineEnvironment(this.config) }, (err, stdout, stderr) => {
        resolve({ code: err ? (err.code ?? 1) : 0, stdout: stdout || '', stderr: stderr || err?.message || '' });
      });
    });
  }

  async doctor(modelDir) {
    if (!modelDir) return { ok: false, checks: [], message: 'No model selected' };
    const res = await this._run([this.cli, 'doctor', '--json', '--model', modelDir, '--policy', this.config.policy, '--ctx', String(this.config.ctx)]);
    try {
      const report = JSON.parse(res.stdout.trim());
      return { ok: res.code === 0, report, raw: res.stdout };
    } catch { return { ok: false, raw: res.stdout + res.stderr, message: (res.stderr || res.stdout).slice(-1500) }; }
  }

  async planFor(modelDir) {
    const res = await this._run([this.cli, 'plan', '--json', '--model', modelDir, '--policy', this.config.policy, '--ctx', String(this.config.ctx)]);
    try { return res.code === 0 ? JSON.parse(res.stdout.trim()) : null; } catch { return null; }
  }

  async attach() {
    if (this.configError) return false;
    const status = await this.probe(this.engineUrl, this.apiKey);
    if (status.kind === 'ready') {
      this.state = 'ready';
      this.modelId = status.modelId;
      this.startedAt ||= Date.now();
      this.lastError = null;
      return true;
    }
    if (status.kind !== 'absent') {
      this.state = 'failed';
      this.lastError = status.message;
    }
    return false;
  }

  /** Attach to a healthy local engine, or spawn one on demand. */
  async ensureRunning(modelDir = this.config.model, options = {}) {
    if (this.configError) throw new Error(this.configError);
    if (this.state === 'ready') return this.describe();
    if (await this.attach()) return this.describe();
    if (this.state === 'failed') throw new Error(this.lastError || 'Engine endpoint is not usable');
    return this.start(modelDir, options);
  }

  async cleanupOrphans() {
    const startedAt = this.startedAt || 0;
    const pids = await listOrphanEnginePids(startedAt);
    const killed = await killOrphanEnginePids(pids);
    this.orphans = (this.orphans || 0) + killed;
    return { found: pids.length, killed, pids };
  }

  stop() {
    if (this.stopping) return this.stopping;
    ++this.generation; // cancels an in-flight probe/start, including before spawn
    this.stopping = (async () => {
      if (this.child) {
        const child = this.child;
        await this.terminate(child);
        if (this.child === child) this.child = null;
      }
      // Engines that re-execed and escaped the process tree (Windows) can
      // outlive the owned child; sweep them so they do not accumulate.
      try { await this.cleanupOrphans(); } catch { /* best-effort */ }
      this.state = 'stopped';
      this.startedAt = 0;
      this.modelId = null;
      this.model = null;
      this.plan = null;
    })().finally(() => { this.stopping = null; });
    return this.stopping;
  }

  start(modelDir = this.config.model, options = {}) {
    if (this.starting || this.stopping) return Promise.reject(new Error('Engine operation already in progress'));
    const generation = ++this.generation;
    const active = () => { if (generation !== this.generation) throw new Error('Engine start cancelled'); };
    this.starting = this._start(modelDir, options, active).catch(async (err) => {
      if (generation === this.generation) {
        await this.stop().catch((stopError) => { err.message += `; cleanup failed: ${stopError.message}`; });
        this.state = 'failed';
        this.lastError = err.message;
      }
      throw err;
    }).finally(() => { this.starting = null; });
    return this.starting;
  }

  async _start(modelDir, options, active) {
    if (this.configError) throw new Error(this.configError);
    if (options.port !== undefined && Number(options.port) !== this.port) throw new Error('Change engine_url in the shared configuration, not the desktop port.');
    this.lastError = null;
    const status = await this.probe(this.engineUrl, this.apiKey);
    active();
    if (status.kind === 'ready') {
      this.state = 'ready';
      this.modelId = status.modelId;
      this.startedAt ||= Date.now();
      return this.describe();
    }
    if (status.kind !== 'absent') throw new Error(status.message);
    if (this.child) throw new Error('Owned engine is still running but unhealthy. Stop it before retrying.');
    const url = new URL(this.engineUrl);
    if (!isLoopback(url.hostname) || url.protocol !== 'http:') throw new Error('Cannot spawn for a remote or HTTPS endpoint. Start that configured server separately.');
    const issues = this.setupStatus(modelDir).issues;
    if (issues.length) throw new Error(issues.join('\n'));
    if (!await describeModel(modelDir, 'selected')) throw new Error('Selected model has no readable configuration and weights. Run shared setup; no download was started.');
    active();
    const args = serveArgs(this.config, modelDir, options);
    await this.checkPort(this.engineUrl);
    active();
    this.state = 'starting';
    this.model = modelDir;
    this.plan = null;
    const logDir = path.join(this.config.cache_root, 'fennec-desktop', 'logs');
    fs.mkdirSync(logDir, { recursive: true });
    this.logPath = path.join(logDir, 'engine.log');
    const logFd = fs.openSync(this.logPath, 'w');
    let child;
    try {
      child = this.spawnChild(this.python, args, { cwd: this.runtime, env: engineEnvironment(this.config),
        windowsHide: true, detached: process.platform !== 'win32', stdio: ['ignore', logFd, logFd] });
    } finally { fs.closeSync(logFd); }
    this.child = child;
    child.once('error', (err) => {
      if (this.child === child) { this.child = null; this.state = 'failed'; this.lastError = `Cannot start runtime: ${err.message}`; }
    });
    child.once('exit', (code, signal) => {
      if (this.child === child) {
        this.child = null;
        this.state = 'failed';
        this.lastError = `Engine exited (${signal || code}). Check the engine log and shared setup.`;
      }
    });
    const deadline = Date.now() + this.startupTimeout;
    while (Date.now() < deadline) {
      active();
      if (this.child !== child) break;
      const ready = await this.probe(this.engineUrl, this.apiKey);
      active();
      if (this.child !== child) break;
      if (ready.kind === 'ready') {
        this.state = 'ready';
        this.modelId = ready.modelId;
        this.startedAt = Date.now();
        return this.describe();
      }
      if (['foreign', 'unauthorized'].includes(ready.kind)) throw new Error(ready.message);
      await new Promise((resolve) => setTimeout(resolve, this.pollMs));
    }
    throw new Error(this.lastError || `Engine did not become healthy. See ${this.logPath}`);
  }

  async stats() {
    if (this.state !== 'ready') return { metrics: {}, experts: null, profile: null };
    const [metrics, experts, profile] = await Promise.all(['/metrics', '/experts', '/profile'].map((route) => request(this.engineUrl, route, this.apiKey)));
    return { metrics: metrics.status === 200 ? parseMetrics(metrics.body) : {},
      experts: experts.status === 200 ? safeJson(experts.body) : null, profile: profile.status === 200 ? safeJson(profile.body) : null };
  }

  tailLog(lines = 120) {
    if (!this.logPath || !exists(this.logPath)) return '';
    return fs.readFileSync(this.logPath, 'utf8').split('\n').slice(-lines).join('\n');
  }
}
function safeJson(text) { try { return JSON.parse(text); } catch { return null; } }
function parseMetrics(text) {
  const out = {};
  for (const line of (text || '').split('\n')) {
    const m = line.match(/^([a-zA-Z0-9_:]+)(?:\{[^}]*\})?\s+([-\d.eE+]+)$/);
    if (m) out[m[1]] = Number(m[2]);
  }
  return out;
}
module.exports = { FennecBackend, listModels, findRuntime, findPython, probeServer, serveArgs, terminateOwnedTree, listOrphanEnginePids, killOrphanEnginePids, assertPortAvailable };
