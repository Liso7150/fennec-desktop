'use strict';
const test = require('node:test');
const assert = require('node:assert');
const path = require('node:path');
const { loadConfig, runtimePaths, engineEnvironment, isLoopback } = require('../electron/config');

const readJson = (value) => () => JSON.stringify(value);

test('portable defaults live under the user home when D: roots are absent', () => {
  const config = loadConfig({ env: {}, home: 'C:/Users/dev', platform: 'win32', exists: () => false });
  assert.strictEqual(config.engine_root, path.win32.join('C:/Users/dev', 'Colibri'));
  assert.strictEqual(config.suite_root, path.win32.join('C:/Users/dev', 'Fennec-Suite'));
  assert.strictEqual(config.models_root, path.win32.join('C:/Users/dev', 'fennec-models'));
  assert.strictEqual(config.cache_root, path.win32.join('C:/Users/dev', '.cache/fennec'));
  assert.strictEqual(config.workspace, config.suite_root);
  assert.strictEqual(config.web_root, path.win32.join(config.suite_root, 'fennec-coding-agent', 'web'));
  assert.strictEqual(config.engine_url, 'http://127.0.0.1:8184');
  assert.strictEqual(config.port, 8184);
  assert.strictEqual(config.backend, 'auto');
  assert.strictEqual(config.api_key, 'local');
});

test('a D: drive is never assumed, saved roots win instead', () => {
  const portable = loadConfig({ env: {}, home: 'C:/Users/dev', platform: 'win32', exists: (p) => p === 'D:/Colibri' });
  assert.strictEqual(portable.engine_root, path.win32.join('C:/Users/dev', 'Colibri'));
  const saved = loadConfig({
    env: { FENNEC_CONFIG: 'C:/cfg/fennec.json' }, platform: 'win32', home: 'C:/Users/dev',
    exists: (p) => p === path.win32.resolve('C:/cfg/fennec.json'),
    readFile: readJson({ engine_root: 'E:/Apps/Colibri' }),
  });
  assert.strictEqual(saved.engine_root, path.win32.resolve('E:/Apps/Colibri'));
});

test('environment variables override saved configuration', () => {
  const config = loadConfig({
    env: { FENNEC_MODELS: '/srv/models', FENNEC_WORKSPACE: '/srv/work' },
    home: '/home/dev', platform: 'linux', exists: () => false,
  });
  assert.strictEqual(config.models_root, '/srv/models');
  assert.strictEqual(config.workspace, '/srv/work');
});

test('relative models resolve under models_root', () => {
  const config = loadConfig({
    env: { FENNEC_MODELS: '/srv/models' }, home: '/home/dev', platform: 'linux', exists: () => false,
  });
  assert.strictEqual(config.model, '');
  const withModel = loadConfig({
    env: { FENNEC_MODELS: '/srv/models', FENNEC_MODEL: 'qwen3' },
    home: '/home/dev', platform: 'linux', exists: () => false,
  });
  assert.strictEqual(withModel.model, path.posix.resolve('/srv/models', 'qwen3'));
});

test('a missing explicit config file is an error, not a silent default', () => {
  assert.throws(
    () => loadConfig({ env: { FENNEC_CONFIG: '/etc/nope.json' }, platform: 'linux', exists: () => false }),
    /does not exist/,
  );
});

test('secrets in configuration JSON are rejected', () => {
  assert.throws(
    () => loadConfig({
      env: { FENNEC_CONFIG: '/etc/fennec.json' }, platform: 'linux',
      exists: (p) => p === '/etc/fennec.json', readFile: readJson({ api_key: 'sk-123' }),
    }),
    /secrets/,
  );
});

test('backend selections match the core contract', () => {
  const load = (backend) => loadConfig({
    env: { FENNEC_CONFIG: '/etc/fennec.json' }, platform: 'linux',
    exists: (p) => p === '/etc/fennec.json', readFile: readJson({ backend }),
  });
  for (const backend of ['auto', 'vulkan', 'metal', 'cpu']) assert.strictEqual(load(backend).backend, backend);
  for (const backend of ['cuda', 'npu', '']) assert.throws(() => load(backend), /Invalid (configuration|backend)/);
});

test('engine_url must be a bare origin and a remote one requires a key', () => {
  const load = (engine_url, env = {}) => loadConfig({
    env: { FENNEC_CONFIG: '/etc/fennec.json', ...env }, platform: 'linux',
    exists: (p) => p === '/etc/fennec.json', readFile: readJson({ engine_url }),
  });
  assert.throws(() => load('http://127.0.0.1:8184/v1'), /Invalid|origin/);
  assert.throws(() => load('http://user:pw@127.0.0.1:8184'), /Invalid|origin/);
  assert.throws(() => load('ftp://127.0.0.1:8184'), /Invalid|origin/);
  assert.throws(() => load('http://10.0.0.2:8184'), /non-loopback/);
  assert.strictEqual(load('http://10.0.0.2:8184', { FENNEC_API_KEY: 'k' }).api_key, 'k');
});

test('isLoopback accepts only local hosts', () => {
  for (const host of ['127.0.0.1', '127.1.2.3', 'localhost', '[::1]', '::1']) assert.ok(isLoopback(host), host);
  for (const host of ['10.0.0.1', 'example.com', '127.0.0.1.evil']) assert.ok(!isLoopback(host), host);
});

test('runtimePaths needs both the launcher and the server', () => {
  const config = { engine_root: '/opt/fennec' };
  assert.strictEqual(runtimePaths(config, { platform: 'linux', exists: () => false }).runtime, null);
  const found = runtimePaths(config, {
    platform: 'linux',
    exists: (p) => p === '/opt/fennec/c/coli' || p === '/opt/fennec/c/openai_server.py',
  });
  assert.strictEqual(found.runtime, '/opt/fennec/c');
  assert.strictEqual(found.cli, '/opt/fennec/c/coli');
  assert.strictEqual(found.server, '/opt/fennec/c/openai_server.py');
});

test('engineEnvironment scrubs inherited backend switches', () => {
  const inherited = {
    PATH: '/usr/bin', COLI_CUDA: '1', COLI_VULKAN: '1', COLI_METAL: '1', COLI_VK_DEV2: '1',
    FENNEC_API_KEY: 'secret', COLI_API_KEY: 'secret',
  };
  const auto = engineEnvironment({ backend: 'auto' }, inherited, { platform: 'linux', exists: () => false });
  for (const key of ['COLI_CUDA', 'COLI_VULKAN', 'COLI_METAL', 'COLI_VK_DEV2', 'FENNEC_API_KEY', 'COLI_API_KEY']) {
    assert.ok(!(key in auto), `${key} should be scrubbed in auto mode`);
  }
  assert.strictEqual(auto.PATH, '/usr/bin');
  assert.strictEqual(auto.PYTHONIOENCODING, 'utf-8');
});

test('auto asks for the GPU tier when a Vulkan loader exists', () => {
  const win = engineEnvironment({ backend: 'auto' }, { SystemRoot: 'C:\\Windows' },
    { platform: 'win32', exists: (p) => p === 'C:\\Windows\\System32\\vulkan-1.dll' });
  assert.strictEqual(win.COLI_VULKAN, '1');

  const absent = engineEnvironment({ backend: 'auto' }, {}, { platform: 'win32', exists: () => false });
  assert.ok(!('COLI_VULKAN' in absent), 'no loader on the box: leave the engine CPU-only');

  const linux = engineEnvironment({ backend: 'auto' }, {},
    { platform: 'linux', exists: (p) => p === '/usr/lib/x86_64-linux-gnu/libvulkan.so.1' });
  assert.strictEqual(linux.COLI_VULKAN, '1');

  const mac = engineEnvironment({ backend: 'auto' }, {}, { platform: 'darwin', exists: () => true });
  assert.ok(!('COLI_VULKAN' in mac), 'Metal is not requested from the auto path');
});

test('auto keeps the host expert pool small', () => {
  const win = engineEnvironment({ backend: 'auto' }, { SystemRoot: 'C:\\Windows' },
    { platform: 'win32', exists: (p) => p.endsWith('vulkan-1.dll') });
  assert.strictEqual(win.COLI_VULKAN, '1');
  assert.strictEqual(win.COLI_VK_TIER_HOST_GB, '4');

  const inherited = engineEnvironment({ backend: 'auto' }, { COLI_VK_TIER_HOST_GB: '9' },
    { platform: 'linux', exists: () => true });
  assert.strictEqual(inherited.COLI_VK_TIER_HOST_GB, '9', 'an explicit pool wins');

  const noLoader = engineEnvironment({ backend: 'auto' }, {},
    { platform: 'linux', exists: () => false });
  assert.ok(!('COLI_VK_TIER_HOST_GB' in noLoader));
});

test('engineEnvironment pins an explicit backend and clears the rest', () => {
  const cpu = engineEnvironment({ backend: 'cpu' }, { COLI_VULKAN: '1' });
  assert.deepStrictEqual([cpu.COLI_CUDA, cpu.COLI_VULKAN, cpu.COLI_METAL], ['0', '0', '0']);
  const vulkan = engineEnvironment({ backend: 'vulkan' }, {});
  assert.deepStrictEqual([vulkan.COLI_CUDA, vulkan.COLI_VULKAN, vulkan.COLI_METAL], ['0', '1', '0']);
  const metal = engineEnvironment({ backend: 'metal' }, {});
  assert.deepStrictEqual([metal.COLI_CUDA, metal.COLI_VULKAN, metal.COLI_METAL], ['0', '0', '1']);
});
