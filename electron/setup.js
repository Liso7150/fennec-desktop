'use strict';
// First-run setup: detect prerequisites, validate Python/engine/model paths and
// write ~/.fennec/config.json atomically. No shell, no secrets, no admin.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { hasVulkanLoader } = require('./config');

const MIN_PYTHON = [3, 10];
const CORE_DEPS = ['fastapi>=0.110', 'uvicorn>=0.29', 'psutil>=5.9', 'pydantic>=2.6'];
const CORE_MODULES = CORE_DEPS.map((spec) => spec.match(/^[A-Za-z0-9_.]+/)[0]);
const BACKENDS = ['auto', 'vulkan', 'metal', 'cpu'];
const POLICIES = ['quality', 'balanced', 'experimental-fast'];
const ENGINE_FILES = ['c/coli', 'c/openai_server.py'];
const SECRET_KEYS = /^(api_key|app_token|token|fennec_api_key|fennec_app_token|coli_api_key)$/i;
const ENGINE_REPO = 'https://github.com/JustVugg/colibri';
const DOWNLOADS = {
  python: 'https://www.python.org/downloads/',
  git: 'https://git-scm.com/downloads',
};
const CONFIG_KEYS = new Set([
  'engine_root', 'suite_root', 'models_root', 'cache_root', 'python', 'model',
  'engine_url', 'web_root', 'workspace', 'backend', 'ctx', 'policy',
]);

const PYTHON_PROBE = [
  'import importlib.util, json, sys',
  "print(json.dumps({'version': '%d.%d.%d' % sys.version_info[:3],",
  "  'venv': sys.prefix != sys.base_prefix,",
  "  'executable': sys.executable,",
  "  'pip': importlib.util.find_spec('pip') is not None}))",
].join('\n');

function platformPath(platform) {
  return platform === 'win32' ? path.win32 : path.posix;
}

function pathEntries(env, platform) {
  return String(env.PATH || env.Path || '')
    .split(platform === 'win32' ? ';' : ':')
    .filter(Boolean);
}

function expandPath(value, { home = os.homedir(), platform = process.platform } = {}) {
  const p = platformPath(platform);
  let text = String(value == null ? '' : value).trim();
  if (text === '~') text = home;
  else if (text.startsWith('~/') || text.startsWith('~\\')) text = p.join(home, text.slice(2));
  return p.normalize(p.isAbsolute(text) ? text : p.resolve(home, text));
}

function configLocation({ env = process.env, home = os.homedir(), platform = process.platform } = {}) {
  const p = platformPath(platform);
  const selected = env.FENNEC_CONFIG || p.join(home, '.fennec', 'config.json');
  return expandPath(selected, { home, platform });
}

function defaultRoots({ home = os.homedir(), platform = process.platform } = {}) {
  const p = platformPath(platform);
  const root = p.join(home, 'Fennec');
  return {
    root,
    engine: p.join(root, 'engine'),
    suite: p.join(root, 'suite'),
    models: p.join(root, 'models'),
    cache: p.join(root, 'cache'),
  };
}

function knownPythonPaths({ home = os.homedir(), platform = process.platform, env = process.env } = {}) {
  const p = platformPath(platform);
  const local = env.LOCALAPPDATA || p.join(home, 'AppData', 'Local');
  if (platform === 'win32') {
    return ['311', '312', '313', '314'].flatMap((minor) => [
      p.join(local, 'Programs', 'Python', `Python${minor}`, 'python.exe'),
      p.join('C:/', `Python${minor}`, 'python.exe'),
    ]).concat([p.join(home, 'AppData', 'Local', 'Programs', 'Python', 'Python312', 'python.exe')]);
  }
  if (platform === 'darwin') {
    return ['/opt/homebrew/bin/python3.13', '/opt/homebrew/bin/python3.12', '/opt/homebrew/bin/python3',
      '/usr/local/bin/python3.13', '/usr/local/bin/python3.12', '/usr/local/bin/python3', '/usr/bin/python3'];
  }
  return ['/usr/bin/python3.12', '/usr/bin/python3', '/usr/local/bin/python3.12', '/usr/local/bin/python3'];
}

function pythonCandidates({ env = process.env, platform = process.platform,
  exists = fs.existsSync, home = os.homedir() } = {}) {
  const p = platformPath(platform);
  const found = [];
  const seen = new Set();
  const add = (command, args, source) => {
    const key = `${command}\0${args.join('\0')}`;
    if (seen.has(key)) return;
    seen.add(key);
    found.push({ command, args, source });
  };
  if (env.FENNEC_PYTHON) add(expandPath(env.FENNEC_PYTHON, { home, platform }), [], 'FENNEC_PYTHON');
  const names = platform === 'win32' ? ['python', 'python3'] : ['python3', 'python'];
  const extensions = platform === 'win32' ? ['.exe'] : [''];
  for (const dir of pathEntries(env, platform)) {
    for (const name of names) {
      for (const ext of extensions) {
        const candidate = p.join(dir, name + ext);
        if (exists(candidate)) add(candidate, [], 'PATH');
      }
    }
  }
  for (const candidate of knownPythonPaths({ home, platform, env })) {
    if (exists(candidate)) add(candidate, [], 'known location');
  }
  if (platform === 'win32') add('py', ['-3'], 'py launcher');
  return found;
}

function parsePythonProbe(stdout) {
  let data;
  try { data = JSON.parse(String(stdout || '').trim().split('\n').pop()); } catch { data = null; }
  if (!data || !data.version) return { ok: false, error: 'Python did not report its version' };
  const [major, minor, patch] = String(data.version).split('.').map(Number);
  const ok = major > MIN_PYTHON[0] || (major === MIN_PYTHON[0] && minor >= MIN_PYTHON[1]);
  return {
    ok,
    version: data.version,
    major, minor, patch: patch || 0,
    venv: data.venv === true,
    executable: data.executable || '',
    hasPip: data.pip === true,
    error: ok ? null : `Python ${data.version} is older than ${MIN_PYTHON[0]}.${MIN_PYTHON[1]}`,
  };
}

function dependencyProbeArgs(modules = CORE_MODULES) {
  const code = [
    'import importlib, json',
    `missing = []`,
    `for name in ${JSON.stringify(modules)}:`,
    '    try:',
    '        importlib.import_module(name)',
    '    except Exception:',
    '        missing.append(name)',
    "print(json.dumps({'missing': missing}))",
  ].join('\n');
  return ['-c', code];
}

function parseDependencyProbe(stdout, modules = CORE_MODULES) {
  let data;
  try { data = JSON.parse(String(stdout || '').trim().split('\n').pop()); } catch { data = null; }
  const missing = data && Array.isArray(data.missing) ? data.missing : modules;
  return { missing, installed: modules.filter((name) => !missing.includes(name)), modules };
}

function pipInstallArgs(python, packages = CORE_DEPS, { venv = false } = {}) {
  if (!python) throw new Error('A Python interpreter is required');
  const specs = packages.map((spec) => String(spec).trim()).filter(Boolean);
  if (!specs.length) throw new Error('No packages selected');
  if (specs.some((spec) => /[\s;&|`$"'\\]/.test(spec))) throw new Error('Invalid package specification');
  return ['-m', 'pip', 'install', '--disable-pip-version-check', '--no-input',
    ...(venv ? [] : ['--user']), ...specs];
}

function ensurePipArgs() {
  return ['-m', 'ensurepip', '--upgrade'];
}

function cloneEngineArgs(destination) {
  return ['clone', '--depth', '1', ENGINE_REPO, destination];
}

function freeBytesAt(directory) {
  let target = directory;
  while (target && !fs.existsSync(target)) {
    const parent = path.dirname(target);
    if (parent === target) break;
    target = parent;
  }
  try {
    const stat = fs.statfsSync(target);
    return Number(stat.bavail) * Number(stat.bsize);
  } catch { return 0; }
}

function validateRoot(directory) {
  if (!directory || typeof directory !== 'string') return { ok: false, writable: false, freeBytes: 0, error: 'Choose a folder' };
  let created = false;
  try {
    if (!fs.existsSync(directory)) {
      fs.mkdirSync(directory, { recursive: true });
      created = true;
    }
    if (!fs.statSync(directory).isDirectory()) {
      return { ok: false, writable: false, freeBytes: 0, error: `${directory} is not a folder` };
    }
    const probe = path.join(directory, `.fennec-write-${process.pid}.tmp`);
    fs.writeFileSync(probe, 'ok');
    fs.unlinkSync(probe);
  } catch (err) {
    return { ok: false, writable: false, freeBytes: 0, error: err.message, created };
  }
  return { ok: true, writable: true, freeBytes: freeBytesAt(directory), created };
}

function validateEngine(directory) {
  if (!directory || typeof directory !== 'string') return { ok: false, missing: ENGINE_FILES, path: '' };
  let isDirectory = false;
  try { isDirectory = fs.statSync(directory).isDirectory(); } catch { isDirectory = false; }
  const missing = isDirectory
    ? ENGINE_FILES.filter((relative) => !fs.existsSync(path.join(directory, relative)))
    : [...ENGINE_FILES];
  return { ok: isDirectory && !missing.length, path: directory, missing, isDirectory };
}

function scanModels(directory, { limit = 40 } = {}) {
  let entries;
  try { entries = fs.readdirSync(directory, { withFileTypes: true }); } catch { return []; }
  const models = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const dir = path.join(directory, entry.name);
    let files;
    try { files = fs.readdirSync(dir, { withFileTypes: true }); } catch { continue; }
    const names = files.filter((file) => file.isFile()).map((file) => file.name);
    const hasConfig = names.includes('config.json');
    const weights = names.filter((name) => /\.(safetensors|gguf)$/i.test(name));
    if (!hasConfig && !weights.length) continue;
    let bytes = 0;
    for (const name of weights) {
      try { bytes += fs.statSync(path.join(dir, name)).size; } catch { /* skip unreadable shard */ }
    }
    models.push({
      name: entry.name,
      path: dir,
      sizeGb: Number((bytes / 1e9).toFixed(2)),
      shards: weights.length,
      hasConfig,
      hasWeights: weights.length > 0,
      ready: hasConfig && weights.length > 0,
    });
  }
  return models.sort((a, b) => Number(b.ready) - Number(a.ready) || a.name.localeCompare(b.name)).slice(0, limit);
}

function validateConfigObject(config) {
  if (!config || typeof config !== 'object' || Array.isArray(config)) throw new Error('Configuration must be a JSON object');
  for (const key of Object.keys(config)) {
    if (SECRET_KEYS.test(key)) throw new Error('Secrets belong in FENNEC_API_KEY, not configuration JSON');
    if (!CONFIG_KEYS.has(key)) throw new Error(`Unsupported configuration key: ${key}`);
  }
  for (const [key, value] of Object.entries(config)) {
    if (key === 'ctx') {
      if (!Number.isInteger(value) || value < 0 || value > 1048576) throw new Error('ctx must be an integer from 0 to 1048576');
    } else if (typeof value !== 'string' || (!value.trim() && key !== 'model')) {
      throw new Error(`${key} must be a nonempty string`);
    }
  }
  if (config.backend && !BACKENDS.includes(config.backend)) throw new Error('backend must be auto, vulkan, metal or cpu');
  if (config.policy && !POLICIES.includes(config.policy)) throw new Error('policy must be quality, balanced or experimental-fast');
  return config;
}

const INPUT_KEYS = new Set(['root', 'engineRoot', 'suiteRoot', 'modelsRoot', 'cacheRoot',
  'engineUrl', 'python', 'model', 'backend', 'ctx', 'policy', 'workspace']);

function buildConfig(values, { home = os.homedir(), platform = process.platform } = {}) {
  const p = platformPath(platform);
  const input = values || {};
  for (const key of Object.keys(input)) {
    if (SECRET_KEYS.test(key)) throw new Error('Secrets belong in FENNEC_API_KEY, not configuration JSON');
    if (!INPUT_KEYS.has(key)) throw new Error(`Unsupported setup option: ${key}`);
  }
  const root = expandPath(input.root || p.join(home, 'Fennec'), { home, platform });
  const suite = expandPath(input.suiteRoot || p.join(root, 'suite'), { home, platform });
  const models = expandPath(input.modelsRoot || p.join(root, 'models'), { home, platform });
  const engine = expandPath(input.engineRoot || p.join(root, 'engine'), { home, platform });
  const cache = expandPath(input.cacheRoot || p.join(root, 'cache'), { home, platform });
  let model = String(input.model || '').trim();
  if (model) model = expandPath(model, { home, platform });
  const config = {
    engine_root: engine,
    suite_root: suite,
    models_root: models,
    cache_root: cache,
    engine_url: 'http://127.0.0.1:8184',
    workspace: suite,
    web_root: p.join(suite, 'fennec-coding-agent', 'web'),
    backend: input.backend || 'auto',
    ctx: input.ctx === '' || input.ctx == null ? 0 : Number(input.ctx),
    policy: input.policy || 'quality',
  };
  if (input.python) config.python = expandPath(input.python, { home, platform });
  config.model = model;
  return validateConfigObject(config);
}

function mergeConfig(existing, next) {
  if (!existing || typeof existing !== 'object' || Array.isArray(existing)) return { ...next };
  return { ...existing, ...next };
}

function writeConfigAtomic(file, config, { backup = true } = {}) {
  validateConfigObject(config);
  if (!file) throw new Error('No configuration path');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  let backupPath = null;
  if (fs.existsSync(file)) {
    if (!backup) throw new Error(`Configuration already exists: ${file}`);
    backupPath = `${file}.bak-fennec-setup`;
    fs.copyFileSync(file, backupPath);
  }
  const temporary = path.join(path.dirname(file), `.${path.basename(file)}.${process.pid}.${Date.now()}.tmp`);
  try {
    fs.writeFileSync(temporary, `${JSON.stringify(config, null, 2)}\n`, { encoding: 'utf8', mode: 0o600 });
    fs.renameSync(temporary, file);
  } finally {
    try { if (fs.existsSync(temporary)) fs.unlinkSync(temporary); } catch { /* rename already moved it */ }
  }
  return { path: file, backup: backupPath };
}

function hasCommand(command, { env = process.env, platform = process.platform, exists = fs.existsSync } = {}) {
  if (!command) return false;
  if (/[\\/]/.test(command)) return exists(command);
  const p = platformPath(platform);
  const extensions = platform === 'win32' ? ['', '.exe', '.cmd', '.bat'] : [''];
  return pathEntries(env, platform).some((dir) => extensions.some((ext) => exists(p.join(dir, command + ext))));
}

function prerequisitePlan({ platform = process.platform, python = '', env = process.env,
  exists = fs.existsSync } = {}) {
  const winget = platform === 'win32' && hasCommand('winget', { env, platform, exists });
  const brew = platform === 'darwin' && hasCommand('brew', { env, platform, exists });
  const linuxManager = platform === 'linux'
    ? (['apt-get', 'dnf', 'pacman'].find((name) => hasCommand(name, { env, platform, exists })) || '')
    : '';
  const pythonItem = {
    id: 'python',
    name: 'Python 3.10+ with pip',
    required: true,
    installed: Boolean(python),
    detail: python || 'Not found — the engine, core and CLI run on it.',
    manual: DOWNLOADS.python,
  };
  if (!pythonItem.installed) {
    if (winget) {
      pythonItem.install = {
        label: 'Install Python 3.12 with winget (user scope)',
        command: 'winget',
        args: ['install', '--id', 'Python.Python.3.12', '-e', '--scope', 'user',
          '--accept-package-agreements', '--accept-source-agreements'],
      };
    } else if (brew) {
      pythonItem.install = { label: 'Install Python 3.12 with Homebrew', command: 'brew', args: ['install', 'python@3.12'] };
    } else if (linuxManager) {
      const packages = linuxManager === 'pacman' ? ['python', 'python-pip'] : ['python3', 'python3-pip'];
      pythonItem.install = {
        label: `Install Python with ${linuxManager} (needs administrator rights)`,
        command: linuxManager,
        args: linuxManager === 'pacman' ? ['-S', ...packages] : ['install', '-y', ...packages],
        requiresRoot: true,
      };
    }
  }
  const git = hasCommand('git', { env, platform, exists });
  const gitItem = {
    id: 'git',
    name: 'Git',
    required: false,
    installed: git,
    detail: git ? 'Available for cloning and updating the engine.' : 'Optional — needed to clone or update the engine.',
    manual: DOWNLOADS.git,
  };
  if (!git) {
    if (winget) {
      gitItem.install = {
        label: 'Install Git with winget (user scope)',
        command: 'winget',
        args: ['install', '--id', 'Git.Git', '-e', '--scope', 'user',
          '--accept-package-agreements', '--accept-source-agreements'],
      };
    } else if (brew) {
      gitItem.install = { label: 'Install Git with Homebrew', command: 'brew', args: ['install', 'git'] };
    } else if (linuxManager) {
      gitItem.install = {
        label: `Install Git with ${linuxManager} (needs administrator rights)`,
        command: linuxManager,
        args: linuxManager === 'pacman' ? ['-S', 'git'] : ['install', '-y', 'git'],
        requiresRoot: true,
      };
    }
  }
  return [pythonItem, gitItem];
}

function deviceInfo(root, { platform = process.platform, env = process.env } = {}) {
  return {
    platform,
    arch: process.arch,
    cores: os.cpus().length,
    ramGb: Number((os.totalmem() / 1e9).toFixed(1)),
    freeGb: Number((freeBytesAt(root) / 1e9).toFixed(1)),
    vulkan: hasVulkanLoader({ platform, env }),
    metal: platform === 'darwin',
  };
}

function detectSetup({ env = process.env, home = os.homedir(), platform = process.platform,
  exists = fs.existsSync } = {}) {
  const location = configLocation({ env, home, platform });
  const defaults = defaultRoots({ home, platform });
  let existing = null;
  let configError = null;
  if (exists(location)) {
    try { existing = JSON.parse(fs.readFileSync(location, 'utf8')); }
    catch (err) { configError = err.message; }
  }
  const roots = existing ? {
    root: defaults.root,
    engine: existing.engine_root || defaults.engine,
    suite: existing.suite_root || defaults.suite,
    models: existing.models_root || defaults.models,
    cache: existing.cache_root || defaults.cache,
  } : defaults;
  return {
    needed: !existing,
    configPath: location,
    configError,
    roots,
    existing,
    models: scanModels(roots.models),
    device: deviceInfo(roots.root, { platform, env }),
  };
}

module.exports = {
  MIN_PYTHON, CORE_DEPS, CORE_MODULES, BACKENDS, POLICIES, ENGINE_FILES, ENGINE_REPO, DOWNLOADS,
  PYTHON_PROBE, configLocation, defaultRoots, knownPythonPaths, pythonCandidates, parsePythonProbe,
  dependencyProbeArgs, parseDependencyProbe, pipInstallArgs, ensurePipArgs, cloneEngineArgs,
  validateRoot, freeBytesAt, validateEngine, scanModels, validateConfigObject, buildConfig, mergeConfig,
  writeConfigAtomic, hasCommand, prerequisitePlan, deviceInfo, detectSetup, expandPath,
};
