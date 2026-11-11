'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const setup = require('../electron/setup');

const temp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'fennec-setup-'));

test('python probe accepts 3.10+ and reports venv and pip', () => {
  const parsed = setup.parsePythonProbe(JSON.stringify({
    version: '3.12.4', venv: true, executable: 'C:/Python312/python.exe', pip: true,
  }));
  assert.strictEqual(parsed.ok, true);
  assert.deepStrictEqual([parsed.major, parsed.minor, parsed.patch], [3, 12, 4]);
  assert.strictEqual(parsed.venv, true);
  assert.strictEqual(parsed.hasPip, true);
});

test('python probe rejects old interpreters and garbage', () => {
  const old = setup.parsePythonProbe(JSON.stringify({ version: '3.9.19' }));
  assert.strictEqual(old.ok, false);
  assert.match(old.error, /older than 3\.10/);
  assert.strictEqual(setup.parsePythonProbe('not json').ok, false);
});

test('python candidates prefer FENNEC_PYTHON then PATH and known locations', () => {
  const exists = (file) => [
    '/opt/py/python3', '/usr/bin/python3', '/custom/python3',
  ].includes(file);
  const candidates = setup.pythonCandidates({
    platform: 'linux', home: '/home/dev', exists,
    env: { PATH: '/opt/py:/usr/bin', FENNEC_PYTHON: '/custom/python3' },
  });
  assert.strictEqual(candidates[0].command, '/custom/python3');
  assert.strictEqual(candidates[0].source, 'FENNEC_PYTHON');
  assert.deepStrictEqual(candidates.slice(1).map((c) => c.command), ['/opt/py/python3', '/usr/bin/python3']);
});

test('windows candidates expand to python.exe and include the py launcher', () => {
  const candidates = setup.pythonCandidates({
    platform: 'win32', home: 'C:/Users/dev', exists: (file) => file === 'C:\\Python\\python.exe',
    env: { PATH: 'C:\\Python', LOCALAPPDATA: 'C:/Users/dev/AppData/Local' },
  });
  assert.strictEqual(candidates[0].command, 'C:\\Python\\python.exe');
  assert.ok(candidates.some((c) => c.command === 'py' && c.args[0] === '-3'));
});

test('pip args use --user outside a venv and never a shell string', () => {
  const user = setup.pipInstallArgs('C:/Python312/python.exe', ['fastapi>=0.110', 'pydantic']);
  assert.deepStrictEqual(user, ['-m', 'pip', 'install', '--disable-pip-version-check', '--no-input',
    '--user', 'fastapi>=0.110', 'pydantic']);
  const venv = setup.pipInstallArgs('/opt/venv/bin/python', ['fastapi'], { venv: true });
  assert.ok(!venv.includes('--user'));
  assert.throws(() => setup.pipInstallArgs('/opt/python', ['fastapi; rm -rf /']), /Invalid package/);
});

test('dependency probe parses missing modules', () => {
  const parsed = setup.parseDependencyProbe(JSON.stringify({ missing: ['psutil'] }));
  assert.deepStrictEqual(parsed.missing, ['psutil']);
  assert.deepStrictEqual(parsed.installed, ['fastapi', 'uvicorn', 'pydantic']);
});

test('validateRoot creates a folder and reports free space', () => {
  const root = temp();
  const target = path.join(root, 'new', 'root');
  const result = setup.validateRoot(target);
  assert.strictEqual(result.ok, true);
  assert.strictEqual(result.created, true);
  assert.ok(fs.existsSync(target));
  assert.ok(result.freeBytes >= 0);
  fs.rmSync(root, { recursive: true, force: true });
});

test('validateEngine requires both the launcher and the server', () => {
  const root = temp();
  assert.strictEqual(setup.validateEngine(root).ok, false);
  fs.mkdirSync(path.join(root, 'c'), { recursive: true });
  fs.writeFileSync(path.join(root, 'c', 'coli'), '');
  assert.deepStrictEqual(setup.validateEngine(root).missing, ['c/openai_server.py']);
  fs.writeFileSync(path.join(root, 'c', 'openai_server.py'), '');
  assert.strictEqual(setup.validateEngine(root).ok, true);
  fs.rmSync(root, { recursive: true, force: true });
});

test('scanModels reports size and readiness, ready models first', () => {
  const root = temp();
  fs.mkdirSync(path.join(root, 'qwen36-35b'));
  fs.writeFileSync(path.join(root, 'qwen36-35b', 'config.json'), '{}');
  fs.writeFileSync(path.join(root, 'qwen36-35b', 'model-00001.safetensors'), Buffer.alloc(1024));
  fs.writeFileSync(path.join(root, 'qwen36-35b', 'model-00002.safetensors'), Buffer.alloc(1024));
  fs.mkdirSync(path.join(root, 'partial'));
  fs.writeFileSync(path.join(root, 'partial', 'config.json'), '{}');
  fs.mkdirSync(path.join(root, 'not-a-model'));
  const models = setup.scanModels(root);
  assert.deepStrictEqual(models.map((m) => m.name), ['qwen36-35b', 'partial']);
  assert.strictEqual(models[0].ready, true);
  assert.strictEqual(models[0].shards, 2);
  assert.strictEqual(models[1].ready, false);
  assert.strictEqual(setup.scanModels(path.join(root, 'missing')).length, 0);
  fs.rmSync(root, { recursive: true, force: true });
});

test('buildConfig derives the install layout and validates enums', () => {
  const config = setup.buildConfig({ root: '/data/Fennec', python: '/usr/bin/python3', backend: 'vulkan', ctx: 8192 },
    { home: '/home/dev', platform: 'linux' });
  assert.strictEqual(config.engine_root, '/data/Fennec/engine');
  assert.strictEqual(config.models_root, '/data/Fennec/models');
  assert.strictEqual(config.cache_root, '/data/Fennec/cache');
  assert.strictEqual(config.suite_root, '/data/Fennec/suite');
  assert.strictEqual(config.web_root, '/data/Fennec/suite/fennec-coding-agent/web');
  assert.strictEqual(config.backend, 'vulkan');
  assert.strictEqual(config.ctx, 8192);
  assert.strictEqual(config.model, '');
  assert.throws(() => setup.buildConfig({ backend: 'npu' }, { home: '/home/dev', platform: 'linux' }), /backend/);
  assert.throws(() => setup.buildConfig({ ctx: 3.5 }, { home: '/home/dev', platform: 'linux' }), /ctx/);
  assert.throws(() => setup.buildConfig({ model: 'm', api_key: 'x' }, { home: '/home/dev', platform: 'linux' }), /Unsupported|Secrets/);
});

test('models resolve against models_root when relative', () => {
  const config = setup.buildConfig({ root: '/data/Fennec', model: 'qwen36-35b' },
    { home: '/home/dev', platform: 'linux' });
  assert.strictEqual(config.model, '/home/dev/qwen36-35b');
});

test('mergeConfig preserves keys the wizard does not own', () => {
  const merged = setup.mergeConfig({ custom: 'keep', engine_root: '/old' }, { engine_root: '/new' });
  assert.strictEqual(merged.custom, 'keep');
  assert.strictEqual(merged.engine_root, '/new');
});

test('writeConfigAtomic backs up, writes 0600 and leaves no temp file', () => {
  const root = temp();
  const file = path.join(root, '.fennec', 'config.json');
  setup.writeConfigAtomic(file, { engine_root: '/e' });
  assert.strictEqual(JSON.parse(fs.readFileSync(file, 'utf8')).engine_root, '/e');
  const result = setup.writeConfigAtomic(file, { engine_root: '/new' });
  assert.ok(result.backup && fs.existsSync(result.backup));
  assert.strictEqual(JSON.parse(fs.readFileSync(file, 'utf8')).engine_root, '/new');
  assert.deepStrictEqual(fs.readdirSync(path.dirname(file)).filter((name) => name.endsWith('.tmp')), []);
  if (process.platform !== 'win32') assert.strictEqual(fs.statSync(file).mode & 0o777, 0o600);
  fs.rmSync(root, { recursive: true, force: true });
});

test('prerequisite plan offers winget on Windows and never escalates on Linux', () => {
  const winExists = (file) => /winget\.exe$/.test(file);
  const windows = setup.prerequisitePlan({
    platform: 'win32', python: '', env: { PATH: 'C:\\Windows\\System32' }, exists: winExists,
  });
  const pythonPlan = windows.find((item) => item.id === 'python');
  assert.strictEqual(pythonPlan.installed, false);
  assert.strictEqual(pythonPlan.install.command, 'winget');
  assert.ok(pythonPlan.install.args.includes('--scope'));

  const linux = setup.prerequisitePlan({
    platform: 'linux', python: '', env: { PATH: '/usr/bin' },
    exists: (file) => file === '/usr/bin/apt-get',
  });
  const item = linux.find((entry) => entry.id === 'python');
  assert.strictEqual(item.install.requiresRoot, true);
  assert.strictEqual(item.install.command, 'apt-get');
});

test('detectSetup reports first run and reads an existing config', () => {
  const root = temp();
  const home = path.join(root, 'home');
  fs.mkdirSync(home, { recursive: true });
  const platform = process.platform;
  const engine = path.join(root, 'engine');
  const first = setup.detectSetup({ home, platform, env: {}, exists: fs.existsSync });
  assert.strictEqual(first.needed, true);
  assert.strictEqual(first.configPath, path.join(home, '.fennec', 'config.json'));
  fs.mkdirSync(path.join(home, '.fennec'), { recursive: true });
  fs.writeFileSync(first.configPath, JSON.stringify({ engine_root: engine }));
  const later = setup.detectSetup({ home, platform, env: {}, exists: fs.existsSync });
  assert.strictEqual(later.needed, false);
  assert.strictEqual(later.roots.engine, engine);
  fs.rmSync(root, { recursive: true, force: true });
});
