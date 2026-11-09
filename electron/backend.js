'use strict';
/**
 * Colibri runtime supervisor.
 *
 * Owns the one thing that makes this app different from a chat wrapper: the
 * Colibri engine process. Colibri pages expert weights from disk on demand, so
 * the model is never loaded whole — the supervisor only starts the engine, reads
 * its plan (VRAM hot tier / RAM warm tier / SSD cold tier) and keeps it alive
 * while the agent streams tokens through the OpenAI-compatible endpoint.
 */
const { spawn, execFile } = require('node:child_process');
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');

const RUNTIME_CANDIDATES = [
  process.env.COLI_RUNTIME,
  'D:\\colibri-models\\.colibri\\runtime',
  'D:\\colibri-models\\.colibri\\runtime\\colibri-v2.1.0',
  'C:\\colibri-models\\.colibri\\runtime',
].filter(Boolean);

const MODEL_ROOTS = [
  process.env.COLI_MODELS,
  'D:\\colibri-models',
  'D:\\colibri-models\\containers',
  'D:\\.cache\\huggingface\\hub',
].filter(Boolean);

const PYTHON_CANDIDATES = [
  process.env.COLI_PYTHON,
  'D:\\Colibri\\.venv\\Scripts\\python.exe',
  'D:\\Colibri-Desktop\\.venv\\Scripts\\python.exe',
];

function exists(p) {
  try {
    fs.accessSync(p);
    return true;
  } catch {
    return false;
  }
}

function findRuntime() {
  for (const base of RUNTIME_CANDIDATES) {
    if (!base) continue;
    const direct = path.join(base, 'openai_server.py');
    if (exists(direct)) return base;
    if (!exists(base)) continue;
    // pick the newest colibri-v* folder that carries the server
    const kids = fs.readdirSync(base, { withFileTypes: true }).filter((d) => d.isDirectory());
    const versions = kids
      .map((d) => ({ name: d.name, full: path.join(base, d.name) }))
      .filter((d) => exists(path.join(d.full, 'openai_server.py')))
      .sort((a, b) => b.name.localeCompare(a.name, undefined, { numeric: true }));
    if (versions.length) return versions[0].full;
  }
  return null;
}

function findPython() {
  for (const p of PYTHON_CANDIDATES) if (p && exists(p)) return p;
  return 'python';
}

/** A model directory is anything Colibri could open: it needs config.json. */
async function listModels() {
  const found = new Map();
  for (const root of MODEL_ROOTS) {
    if (!exists(root)) continue;
    let entries = [];
    try {
      entries = await fsp.readdir(root, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const dir = path.join(root, entry.name);
      // huggingface hub layout: models--Org--Name/snapshots/<sha>
      if (entry.name.startsWith('models--')) {
        const snapRoot = path.join(dir, 'snapshots');
        if (!exists(snapRoot)) continue;
        let shas = [];
        try {
          shas = await fsp.readdir(snapRoot);
        } catch {
          continue;
        }
        for (const sha of shas) {
          const snap = path.join(snapRoot, sha);
          const meta = await describeModel(snap, `${entry.name.replace(/^models--/, '').replace(/--/g, '/')} (${sha.slice(0, 7)})`);
          if (meta) found.set(snap, meta);
        }
        continue;
      }
      const meta = await describeModel(dir, entry.name);
      if (meta) found.set(dir, meta);
    }
  }
  return [...found.entries()]
    .map(([dir, meta]) => ({ dir, ...meta }))
    .sort((a, b) => Number(b.container) - Number(a.container) || a.name.localeCompare(b.name));
}

async function describeModel(dir, name) {
  const cfg = path.join(dir, 'config.json');
  if (!exists(cfg)) return null;
  let config = {};
  try {
    config = JSON.parse(await fsp.readFile(cfg, 'utf8'));
  } catch {
    return null;
  }
  let files = [];
  try {
    files = await fsp.readdir(dir);
  } catch {
    return null;
  }
  const shards = files.filter((f) => /\.safetensors$/i.test(f));
  const gguf = files.filter((f) => /\.gguf$/i.test(f));
  if (!shards.length && !gguf.length) return null;
  let bytes = 0;
  for (const f of [...shards, ...gguf]) {
    try {
      bytes += (await fsp.stat(path.join(dir, f))).size;
    } catch {
      /* ignore */
    }
  }
  const container = files.includes('qwen36_meta.json');
  return {
    name,
    family: config.model_type || config.architectures?.[0] || 'unknown',
    layers: config.num_hidden_layers || config.text_config?.num_hidden_layers || 0,
    experts: config.num_experts || config.text_config?.num_experts || 0,
    sizeGb: +(bytes / 1e9).toFixed(2),
    shards: shards.length,
    gguf: gguf.length > 0,
    container,
    ready: container || gguf.length > 0,
  };
}

class ColibriBackend {
  constructor() {
    this.runtime = findRuntime();
    this.python = findPython();
    this.child = null;
    this.port = 8765;
    this.model = null;
    this.logPath = null;
    this.state = 'stopped';
    this.lastError = null;
    this.plan = null;
    this.startedAt = 0;
    // The engine names the model itself (e.g. qwen3-coder-30b-a3b-colibri); a
    // request with any other id is a 404, so the id is read back after boot.
    this.modelId = null;
    this.apiKey = process.env.COLI_API_KEY || '';
  }

  describe() {
    return {
      runtime: this.runtime,
      python: this.python,
      port: this.port,
      model: this.model,
      modelId: this.modelId,
      apiKey: this.apiKey ? '(set)' : '',
      state: this.state,
      lastError: this.lastError,
      plan: this.plan,
      uptimeMs: this.startedAt ? Date.now() - this.startedAt : 0,
      logPath: this.logPath,
    };
  }

  _run(args, { timeout = 180000 } = {}) {
    return new Promise((resolve) => {
      if (!this.runtime) {
        resolve({ code: -1, stdout: '', stderr: 'Colibri runtime not found' });
        return;
      }
      execFile(
        this.python,
        args,
        { cwd: this.runtime, timeout, maxBuffer: 32 * 1024 * 1024, windowsHide: true },
        (err, stdout, stderr) => {
          resolve({ code: err ? (err.code ?? 1) : 0, stdout: stdout || '', stderr: stderr || '' });
        },
      );
    });
  }

  async doctor(modelDir) {
    if (!modelDir) return { ok: false, checks: [], message: 'no model selected' };
    const res = await this._run(['coli', 'doctor', '--json', '--model', modelDir]);
    try {
      const report = JSON.parse(res.stdout.trim().split('\n').filter(Boolean).pop());
      return { ok: res.code === 0, report, raw: res.stdout };
    } catch {
      return { ok: false, raw: res.stdout + res.stderr, message: (res.stderr || res.stdout).slice(-800) };
    }
  }

  async planFor(modelDir) {
    const res = await this._run(['coli', 'plan', '--json', '--model', modelDir]);
    try {
      return JSON.parse(res.stdout.trim().split('\n').filter(Boolean).pop());
    } catch {
      return null;
    }
  }

  async stop() {
    if (this.child) {
      const child = this.child;
      this.child = null;
      try {
        child.kill('SIGTERM');
      } catch {
        /* ignore */
      }
      await new Promise((r) => setTimeout(r, 1200));
      try {
        child.kill('SIGKILL');
      } catch {
        /* ignore */
      }
    }
    // the engine keeps its own process tree; `coli stop` is the supported teardown
    await this._run(['coli', 'stop'], { timeout: 30000 }).catch(() => {});
    this.state = 'stopped';
    this.startedAt = 0;
  }

  async start(modelDir, { port = this.port, kvSlots = 1, maxTokens = 0 } = {}) {
    if (!this.runtime) throw new Error('Colibri runtime not found (set COLI_RUNTIME)');
    if (!modelDir) throw new Error('no model selected');
    if (this.child) await this.stop();

    this.port = port;
    this.model = modelDir;
    this.state = 'starting';
    this.lastError = null;
    this.plan = null;

    const logDir = path.join(process.env.APPDATA || process.env.HOME || '.', 'ColibriCode', 'logs');
    fs.mkdirSync(logDir, { recursive: true });
    this.logPath = path.join(logDir, 'engine.log');
    const out = fs.createWriteStream(this.logPath, { flags: 'w' });

    const args = [
      'openai_server.py',
      '--model', modelDir,
      '--host', '127.0.0.1',
      '--port', String(port),
      '--kv-slots', String(kvSlots),
    ];
    if (maxTokens > 0) args.push('--max-tokens', String(maxTokens));

    // Placement: let the engine use the GPU it finds, and give the planner the
    // machine's real RAM so the warm expert tier is sized instead of the legacy
    // eight slots. COLI_PLACE lets the user pin experts (e.g. 'experts=cpu').
    const env = { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' };
    if (!env.COLI_CUDA && !env.COLI_VULKAN) env.COLI_CUDA = '1';
    if (!env.RAM_GB) {
      const totalGb = Math.floor(os.totalmem() / 1e9);
      env.RAM_GB = String(Math.max(8, Math.min(28, totalGb - 6)));
    }
    if (!env.OMP_NUM_THREADS) env.OMP_NUM_THREADS = String(Math.max(4, os.cpus().length - 6));

    const child = spawn(this.python, args, {
      cwd: this.runtime,
      env,
      windowsHide: true,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.child = child;
    child.stdout.pipe(out);
    child.stderr.pipe(out);
    child.on('exit', (code, signal) => {
      if (this.child === child) {
        this.child = null;
        this.state = code === 0 ? 'stopped' : 'failed';
        this.lastError = signal ? `engine killed (${signal})` : `engine exited (code ${code})`;
      }
    });

    const deadline = Date.now() + 15 * 60 * 1000;
    while (Date.now() < deadline) {
      if (!this.child) break;
      if (await this._healthy()) {
        this.state = 'ready';
        this.startedAt = Date.now();
        this.modelId = await this._discoverModelId();
        this.plan = await this.planFor(modelDir);
        return this.describe();
      }
      await new Promise((r) => setTimeout(r, 1500));
    }
    if (this.state !== 'ready') {
      this.state = 'failed';
      this.lastError = this.lastError || 'engine did not become healthy in time';
      throw new Error(`${this.lastError}. See ${this.logPath}`);
    }
    return this.describe();
  }

  /** GET /v1/models → the model id this engine answers to. */
  _discoverModelId() {
    return new Promise((resolve) => {
      const req = http.get(
        {
          host: '127.0.0.1',
          port: this.port,
          path: '/v1/models',
          timeout: 4000,
          headers: this.apiKey ? { Authorization: `Bearer ${this.apiKey}` } : {},
        },
        (res) => {
          let body = '';
          res.setEncoding('utf8');
          res.on('data', (c) => (body += c));
          res.on('end', () => {
            try {
              const list = JSON.parse(body);
              resolve(list.data?.[0]?.id || null);
            } catch {
              resolve(null);
            }
          });
        },
      );
      req.on('error', () => resolve(null));
      req.on('timeout', () => {
        req.destroy();
        resolve(null);
      });
    });
  }

  _healthy() {
    return new Promise((resolve) => {
      const req = http.get(
        { host: '127.0.0.1', port: this.port, path: '/health', timeout: 1500 },
        (res) => {
          res.resume();
          resolve(res.statusCode >= 200 && res.statusCode < 500);
        },
      );
      req.on('error', () => resolve(false));
      req.on('timeout', () => {
        req.destroy();
        resolve(false);
      });
    });
  }

  async stats() {
    const get = (p) =>
      new Promise((resolve) => {
        const req = http.get({ host: '127.0.0.1', port: this.port, path: p, timeout: 2000 }, (res) => {
          let body = '';
          res.setEncoding('utf8');
          res.on('data', (c) => (body += c));
          res.on('end', () => resolve(body));
        });
        req.on('error', () => resolve(''));
        req.on('timeout', () => {
          req.destroy();
          resolve('');
        });
      });
    const [metrics, experts, profile] = await Promise.all([get('/metrics'), get('/experts'), get('/profile')]);
    return { metrics: parseMetrics(metrics), experts: safeJson(experts), profile: safeJson(profile) };
  }

  tailLog(lines = 120) {
    if (!this.logPath || !exists(this.logPath)) return '';
    const text = fs.readFileSync(this.logPath, 'utf8');
    return text.split('\n').slice(-lines).join('\n');
  }
}

function safeJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return text ? { raw: text.slice(0, 4000) } : null;
  }
}

function parseMetrics(text) {
  const out = {};
  if (!text) return out;
  for (const line of text.split('\n')) {
    if (!line || line.startsWith('#')) continue;
    const m = line.match(/^([a-zA-Z0-9_:]+)(?:\{[^}]*\})?\s+([-\d.eE+]+)$/);
    if (m) out[m[1]] = Number(m[2]);
  }
  return out;
}

module.exports = { ColibriBackend, listModels, findRuntime, findPython };
