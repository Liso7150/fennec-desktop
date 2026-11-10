'use strict';
const test = require('node:test');
const assert = require('node:assert');
const http = require('node:http');
const net = require('node:net');
const {
  ColibriBackend, probeServer, serveArgs, terminateOwnedTree, assertPortAvailable,
} = require('../electron/backend');

const config = {
  engine_root: '/opt/colibri', suite_root: '/opt/colibri-suite', models_root: '/opt/models',
  model: '', engine_url: 'http://127.0.0.1:8184', backend: 'auto', ctx: 4096,
  policy: 'quality', api_key: 'test-key', port: 8184, cache_root: '/tmp/colibri-cache',
};

function listen(handler) {
  return new Promise((resolve) => {
    const server = http.createServer(handler);
    server.listen(0, '127.0.0.1', () => resolve({
      url: `http://127.0.0.1:${server.address().port}`,
      close: () => new Promise((done) => server.close(done)),
    }));
  });
}

test('serveArgs builds the documented serve command', () => {
  const argv = serveArgs(config, '/opt/models/qwen', { kvSlots: 2, maxTokens: 8192 });
  assert.strictEqual(argv[1], 'serve');
  assert.deepStrictEqual(argv.slice(2), [
    '--model', '/opt/models/qwen', '--auto-tier', '--policy', 'quality',
    '--ctx', '4096', '--host', '127.0.0.1', '--port', '8184',
    '--api-key', 'test-key', '--kv-slots', '2', '--max-tokens', '8192',
  ]);
});

test('serveArgs rejects out-of-range slots and token limits', () => {
  for (const kvSlots of [0, 9, 1.5, '2']) assert.throws(() => serveArgs(config, '/m', { kvSlots }), /KV slot/);
  for (const maxTokens of [0, -1, 1.5, 'x']) assert.throws(() => serveArgs(config, '/m', { maxTokens }), /token limit/);
});

test('probeServer recognises a healthy Colibri engine', async () => {
  const server = await listen((req, res) => {
    res.setHeader('content-type', 'application/json');
    if (req.url === '/health') res.end(JSON.stringify({ status: 'ok' }));
    else if (req.url === '/v1/models') {
      res.end(JSON.stringify({ object: 'list', data: [{ object: 'model', id: 'qwen', owned_by: 'colibri' }] }));
    } else { res.statusCode = 404; res.end('{}'); }
  });
  try {
    assert.deepStrictEqual(await probeServer(server.url, 'k'), { kind: 'ready', modelId: 'qwen' });
  } finally {
    await server.close();
  }
});

test('probeServer refuses foreign or unhealthy endpoints', async () => {
  const foreign = await listen((req, res) => {
    res.setHeader('content-type', 'application/json');
    if (req.url === '/health') res.end(JSON.stringify({ status: 'ok' }));
    else res.end(JSON.stringify({ object: 'list', data: [{ object: 'model', id: 'gpt', owned_by: 'openai' }] }));
  });
  try {
    assert.strictEqual((await probeServer(foreign.url, 'k')).kind, 'foreign');
  } finally {
    await foreign.close();
  }

  const unhealthy = await listen((req, res) => {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ status: 'loading' }));
  });
  try {
    assert.strictEqual((await probeServer(unhealthy.url, 'k')).kind, 'blocked');
  } finally {
    await unhealthy.close();
  }

  const guarded = await listen((req, res) => {
    res.statusCode = 401;
    res.end('{}');
  });
  try {
    assert.strictEqual((await probeServer(guarded.url, 'k')).kind, 'unauthorized');
  } finally {
    await guarded.close();
  }
});

test('probeServer reports an absent engine when nothing listens', async () => {
  const server = await listen((req, res) => res.end('{}'));
  const url = server.url;
  await server.close();
  const result = await probeServer(url, 'k');
  assert.strictEqual(result.kind, 'absent');
});

test('assertPortAvailable detects an occupied port and frees it', async () => {
  const server = await listen((req, res) => res.end(''));
  await assert.rejects(assertPortAvailable(server.url), /occupied or unavailable/);
  await server.close();
  await assertPortAvailable(server.url);
});

test('terminateOwnedTree ignores an already-exited child', async () => {
  let touched = false;
  await terminateOwnedTree({ pid: 4242, exitCode: 0, signalCode: null }, {
    platform: 'linux',
    exec: () => { touched = true; },
    kill: () => { touched = true; },
  });
  assert.strictEqual(touched, false);
});

test('terminateOwnedTree kills the whole tree on windows', async () => {
  let call = null;
  await terminateOwnedTree({ pid: 4242, exitCode: null, signalCode: null }, {
    platform: 'win32',
    exec: (cmd, args, options, cb) => { call = { cmd, args, options }; cb(null); },
    kill: () => { throw new Error('taskkill must be used on win32'); },
  });
  assert.strictEqual(call.cmd, 'taskkill');
  assert.deepStrictEqual(call.args, ['/PID', '4242', '/T', '/F']);
});

test('terminateOwnedTree signals only the owned process group on posix', async () => {
  let exit = null;
  const signals = [];
  const child = {
    pid: 4242, exitCode: null, signalCode: null,
    once(event, cb) { if (event === 'exit') exit = cb; },
    removeListener() {},
  };
  await terminateOwnedTree(child, {
    platform: 'linux',
    exec: () => { throw new Error('taskkill must not run on posix'); },
    kill: (pid, signal) => {
      signals.push([pid, signal]);
      setImmediate(() => exit && exit(0, signal));
    },
  });
  assert.deepStrictEqual(signals, [[-4242, 'SIGTERM']]);
});

test('backend attaches to a healthy engine without spawning', async () => {
  const backend = new ColibriBackend({
    config,
    spawnChild: () => { throw new Error('attach must not spawn'); },
    probe: async () => ({ kind: 'ready', modelId: 'qwen' }),
  });
  assert.strictEqual(await backend.attach(), true);
  assert.strictEqual(backend.state, 'ready');
  assert.strictEqual(backend.modelId, 'qwen');
  assert.strictEqual(backend.describe().owned, false);
  await backend.stop();
  assert.strictEqual(backend.state, 'stopped');
});

test('backend reports a blocked engine instead of pretending readiness', async () => {
  const backend = new ColibriBackend({
    config,
    probe: async () => ({ kind: 'foreign', message: 'Occupied endpoint did not return a valid Colibri model list.' }),
  });
  assert.strictEqual(await backend.attach(), false);
  assert.strictEqual(backend.state, 'failed');
  assert.match(backend.lastError, /Colibri model list/);
});

test('backend start refuses a port override that contradicts engine_url', async () => {
  const backend = new ColibriBackend({ config, probe: async () => ({ kind: 'absent' }) });
  await assert.rejects(backend.start('/opt/models/qwen', { port: 9999 }), /engine_url/);
});

test('backend ensureRunning attaches without spawning when an engine is healthy', async () => {
  const backend = new ColibriBackend({
    config,
    spawnChild: () => { throw new Error('healthy engine must not spawn'); },
    probe: async () => ({ kind: 'ready', modelId: 'qwen' }),
  });
  const described = await backend.ensureRunning('/opt/models/qwen');
  assert.strictEqual(described.state, 'ready');
  assert.strictEqual(described.modelId, 'qwen');
  assert.strictEqual(described.owned, false);
});

test('backend ensureRunning spawns an engine on demand when the port is free', async () => {
  const os = require('node:os');
  const fs = require('node:fs');
  const path = require('node:path');
  const { EventEmitter } = require('node:events');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'colibri-ensure-'));
  const engine = path.join(root, 'engine');
  const model = path.join(root, 'models', 'qwen');
  fs.mkdirSync(path.join(engine, 'c'), { recursive: true });
  fs.writeFileSync(path.join(engine, 'c', 'coli'), '');
  fs.writeFileSync(path.join(engine, 'c', 'openai_server.py'), '');
  fs.mkdirSync(model, { recursive: true });
  fs.writeFileSync(path.join(model, 'config.json'), JSON.stringify({ model_type: 'qwen' }));
  fs.writeFileSync(path.join(model, 'qwen36_meta.json'), '{}');
  fs.writeFileSync(path.join(model, 'model-00001-of-00001.safetensors'), '');
  const spawned = [];
  let probes = 0;
  const child = Object.assign(new EventEmitter(), { pid: 4242, exitCode: null, signalCode: null });
  const backend = new ColibriBackend({
    config: { ...config, engine_root: engine, cache_root: path.join(root, 'cache'), python: process.execPath },
    spawnChild: (python, args, opts) => { spawned.push({ python, args, opts }); return child; },
    terminate: async () => {},
    checkPort: async () => {},
    probe: async () => (++probes <= 2 ? { kind: 'absent' } : { kind: 'ready', modelId: 'qwen' }),
    pollMs: 1,
  });
  try {
    const described = await backend.ensureRunning(model, { kvSlots: 1, maxTokens: 512 });
    assert.strictEqual(described.state, 'ready');
    assert.strictEqual(described.modelId, 'qwen');
    assert.strictEqual(described.owned, true);
    assert.strictEqual(spawned.length, 1);
    assert.deepStrictEqual(spawned[0].args.slice(0, 3), [path.join(engine, 'c', 'coli'), 'serve', '--model']);
  } finally {
    await backend.stop();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
