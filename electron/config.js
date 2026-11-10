'use strict';
// Read-only implementation of the suite's shared configuration contract.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

const INSTALLER_URL = 'https://github.com/Liso7150/colibri-installer';
function isLoopback(host) {
  return host === 'localhost' || host === '[::1]' || host === '::1' || /^127(?:\.\d{1,3}){3}$/.test(host);
}
function loadConfig({ env = process.env, home = os.homedir(), platform = process.platform,
  exists = fs.existsSync, readFile = (p) => fs.readFileSync(p, 'utf8') } = {}) {
  const p = platform === 'win32' ? path.win32 : path.posix;
  const resolve = (value, base) => p.resolve(base, value.replace(/^~(?=$|[\\/])/, home));
  const location = resolve(env.COLIBRI_CONFIG || p.join(home, '.colibri', 'config.json'), home);
  let data = {};
  if (exists(location)) {
    try { data = JSON.parse(readFile(location)); }
    catch { throw new Error(`Cannot read configuration JSON: ${location}`); }
  } else if (env.COLIBRI_CONFIG) {
    throw new Error(`Configured file does not exist: ${location}`);
  }
  if (!data || typeof data !== 'object' || Array.isArray(data)) throw new Error('Colibri configuration must be a JSON object');
  if (Object.keys(data).some((k) => /^(api_key|app_token|token|colibri_api_key|colibri_app_token|coli_api_key)$/i.test(k))) {
    throw new Error('Keep secrets in COLIBRI_API_KEY, not configuration JSON');
  }
  const fallback = (windows, portable) => platform === 'win32' && exists(windows) ? windows : p.join(home, portable);
  const values = {
    engine_root: fallback('D:/Colibri', 'Colibri'),
    suite_root: fallback('D:/Colibri-Suite', 'Colibri-Suite'),
    models_root: fallback('D:/colibri-models', 'colibri-models'),
    cache_root: fallback('D:/.cache', '.cache/colibri'),
    model: '', engine_url: 'http://127.0.0.1:8184', backend: 'auto', ctx: 0, policy: 'quality',
  };
  for (const key of [...Object.keys(values), 'python', 'workspace', 'web_root']) {
    if (key in data) values[key] = data[key];
  }
  const aliases = { COLI_PYTHON: 'python', COLI_MODEL: 'model', COLI_MODELS: 'models_root' };
  const preferred = { COLIBRI_HOME: 'engine_root', COLIBRI_MODELS: 'models_root', COLIBRI_CACHE: 'cache_root',
    COLIBRI_PYTHON: 'python', COLIBRI_MODEL: 'model', COLIBRI_ENGINE_URL: 'engine_url', COLIBRI_WORKSPACE: 'workspace' };
  for (const [variable, key] of Object.entries({ ...aliases, ...preferred })) {
    if (env[variable] !== undefined) values[key] = env[variable];
  }
  for (const [key, value] of Object.entries(values)) {
    if (key !== 'ctx' && (typeof value !== 'string' || (!value.trim() && key !== 'model'))) throw new Error(`Invalid configuration: ${key}`);
  }
  for (const key of ['engine_root', 'suite_root', 'models_root', 'cache_root']) values[key] = resolve(values[key], p.dirname(location));
  values.workspace = resolve(values.workspace || values.suite_root, p.dirname(location));
  values.web_root = resolve(values.web_root || p.join(values.suite_root, 'colibri-coding-agent', 'web'), p.dirname(location));
  if (values.model) values.model = resolve(values.model, values.models_root);
  if (!['auto', 'vulkan', 'metal', 'cpu'].includes(values.backend)) throw new Error('Invalid backend selection');
  if (!['quality', 'balanced', 'experimental-fast'].includes(values.policy)) throw new Error('Invalid resource policy');
  if (!Number.isInteger(values.ctx) || values.ctx < 0 || values.ctx > 1048576) throw new Error('ctx must be an integer from 0 to 1048576');
  let url;
  try { url = new URL(values.engine_url); } catch { throw new Error('engine_url must be an HTTP(S) origin'); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.port === '0' ||
      url.pathname !== '/' || url.search || url.hash) throw new Error('engine_url must be an HTTP(S) origin without credentials, path or query');
  values.api_key = env.COLIBRI_API_KEY || env.COLI_API_KEY || '';
  if (!isLoopback(url.hostname) && !values.api_key) throw new Error('A non-loopback engine requires COLIBRI_API_KEY');
  values.api_key ||= 'local';
  values.engine_url = url.origin;
  values.configPath = location;
  values.port = Number(url.port || (url.protocol === 'https:' ? 443 : 80));
  const python = values.python;
  if (python) {
    const candidates = (env.PATH || env.Path || '').split(platform === 'win32' ? ';' : ':')
      .flatMap((dir) => [p.join(dir, python), ...(platform === 'win32' ? [p.join(dir, `${python}.exe`)] : [])]);
    values.python = (!/[\\/]/.test(python) && candidates.find(exists)) || resolve(python, p.dirname(location));
  } else {
    const candidates = platform === 'win32' ? ['Scripts/pythonw.exe', 'Scripts/python.exe'] : ['bin/python'];
    values.python = candidates.map((name) => p.join(values.engine_root, '.venv', name)).find(exists) || null;
  }
  // The web workspace is a separate, local core client. Never load it into Electron.
  values.web_url = 'http://127.0.0.1:8787';
  return values;
}

function runtimePaths(config, { platform = process.platform, exists = fs.existsSync } = {}) {
  const p = platform === 'win32' ? path.win32 : path.posix;
  const runtime = p.join(config.engine_root, 'c');
  const cli = p.join(runtime, 'coli');
  const server = p.join(runtime, 'openai_server.py');
  return { runtime: exists(cli) && exists(server) ? runtime : null, cli, server };
}

function engineEnvironment(config, env = process.env) {
  const next = { ...env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' };
  for (const key of ['COLIBRI_APP_TOKEN', 'COLIBRI_API_KEY', 'COLI_API_KEY',
    'COLI_VULKAN', 'COLI_METAL', 'COLI_CUDA', 'COLI_VK_DEV2']) delete next[key];
  // serve has no --backend flag; auto delegates detection to coli.
  if (config.backend !== 'auto') {
    Object.assign(next, { COLI_VULKAN: '0', COLI_METAL: '0', COLI_CUDA: '0' });
    if (config.backend !== 'cpu') next[`COLI_${config.backend.toUpperCase()}`] = '1';
  }
  return next;
}
module.exports = { loadConfig, runtimePaths, engineEnvironment, isLoopback, INSTALLER_URL };
