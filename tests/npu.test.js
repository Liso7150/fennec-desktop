'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { Accelerator, findEmbedModel, findPython } = require('../electron/npu');

function tempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'colibri-npu-'));
}

function withEnvScrubbed(fn) {
  const saved = {};
  for (const key of ['COLIBRI_PYTHON', 'COLI_PYTHON', 'COLI_EMBED_MODEL']) {
    saved[key] = process.env[key];
    delete process.env[key];
  }
  try {
    return fn();
  } finally {
    for (const [key, value] of Object.entries(saved)) {
      if (value !== undefined) process.env[key] = value;
    }
  }
}

test('findPython prefers the configured interpreter', () => withEnvScrubbed(() => {
  assert.strictEqual(findPython({ python: process.execPath }), process.execPath);
  assert.strictEqual(findPython({ python: path.join(tempRoot(), 'nope.exe') }), 'python');
}));

test('findPython falls back to the shared venv under engine_root', () => withEnvScrubbed(() => {
  const root = tempRoot();
  const venv = path.join(root, '.venv', process.platform === 'win32' ? 'Scripts/python.exe' : 'bin/python');
  fs.mkdirSync(path.dirname(venv), { recursive: true });
  fs.writeFileSync(venv, '');
  assert.strictEqual(findPython({ engine_root: root }), venv);
}));

test('findEmbedModel reads models_root from configuration', () => withEnvScrubbed(() => {
  const root = tempRoot();
  fs.mkdirSync(path.join(root, 'embed'));
  fs.writeFileSync(path.join(root, 'embed', 'openvino_model.xml'), '<net/>');
  assert.strictEqual(findEmbedModel({ models_root: root }), path.join(root, 'embed'));
}));

test('findEmbedModel accepts an IR model directly under models_root', () => withEnvScrubbed(() => {
  const root = tempRoot();
  fs.writeFileSync(path.join(root, 'openvino_model.xml'), '<net/>');
  assert.strictEqual(findEmbedModel({ models_root: root }), root);
}));

test('findEmbedModel finds a nested exported model without hardcoded paths', () => withEnvScrubbed(() => {
  const root = tempRoot();
  const nested = path.join(root, 'embed', 'bge-small');
  fs.mkdirSync(nested, { recursive: true });
  fs.writeFileSync(path.join(nested, 'openvino_model.xml'), '<net/>');
  assert.strictEqual(findEmbedModel({ models_root: root }), nested);
}));

test('findEmbedModel returns null when no IR model is installed', () => withEnvScrubbed(() => {
  assert.strictEqual(findEmbedModel({ models_root: tempRoot() }), null);
  assert.strictEqual(findEmbedModel({}), null);
}));

test('Accelerator consumes the shared configuration', () => withEnvScrubbed(() => {
  const root = tempRoot();
  fs.mkdirSync(path.join(root, 'embed'));
  fs.writeFileSync(path.join(root, 'embed', 'openvino_model.xml'), '<net/>');
  const accelerator = new Accelerator({ python: process.execPath, models_root: root });
  const status = accelerator.status();
  assert.strictEqual(status.python, process.execPath);
  assert.strictEqual(status.embedModel, path.join(root, 'embed'));
}));
