'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  Accelerator,
  EMBED_MODEL_ID,
  EMBED_IR_REPO,
  MODEL_PACKAGES,
  SIDECAR_PACKAGES,
  embedTarget,
  findEmbedModel,
  findPython,
  installArgs,
  pythonCandidates,
} = require('../electron/npu');

function tempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'fennec-npu-'));
}

function withEnvScrubbed(fn) {
  const saved = {};
  for (const key of ['FENNEC_PYTHON', 'COLI_PYTHON', 'COLI_EMBED_MODEL']) {
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

test('pythonCandidates keeps a PATH python as the last resort and dedupes', () => withEnvScrubbed(() => {
  const self = pythonCandidates({ python: process.execPath });
  assert.strictEqual(self[0], process.execPath);
  assert.strictEqual(self[self.length - 1], 'python');
  const duplicated = pythonCandidates({ python: 'python', engine_root: 'D:/definitely-not-here' });
  assert.deepStrictEqual(duplicated, ['python']);
}));

test('installArgs targets a user site outside a virtualenv and caches downloads', () => {
  const outside = installArgs(path.join(process.execPath), SIDECAR_PACKAGES, { cache_root: 'D:/.cache' });
  assert.ok(!outside.argv.includes('--user') || !outside.insideVenv, 'only a non-venv may use --user');
  assert.strictEqual(outside.env.PIP_CACHE_DIR, path.join('D:/.cache', 'pip'));
  assert.deepStrictEqual(outside.argv.slice(-3), SIDECAR_PACKAGES);
});

test('installArgs installs into the virtualenv without --user', () => {
  const root = tempRoot();
  const scripts = path.join(root, process.platform === 'win32' ? 'Scripts' : 'bin');
  fs.mkdirSync(scripts, { recursive: true });
  fs.writeFileSync(path.join(root, 'pyvenv.cfg'), 'home = nowhereland\n');
  const python = path.join(scripts, process.platform === 'win32' ? 'python.exe' : 'python');
  fs.writeFileSync(python, '');
  const inside = installArgs(python, SIDECAR_PACKAGES, {});
  assert.strictEqual(inside.insideVenv, true);
  assert.ok(!inside.argv.includes('--user'));
});

test('the model step fetches prebuilt IR instead of exporting it', () => {
  const root = tempRoot();
  const dest = embedTarget({ models_root: root });
  assert.ok(dest.endsWith(path.join('embed', 'bge-base-en-v1.5-int8-ov')));
  assert.ok(EMBED_IR_REPO.startsWith('OpenVINO/'), 'only the official OpenVINO org publishes these IR builds');
  assert.ok(EMBED_IR_REPO.includes(EMBED_MODEL_ID.split('/')[1]), 'the IR repo is the same model as the id');
});

test('the index run pins static shapes and labels itself a benchmark', () => {
  const src = fs.readFileSync(path.join(__dirname, '..', 'electron', 'npu.js'), 'utf8');
  // A dynamic-shape IR cannot be compiled by the NPU plugin, and padding ids
  // are not an embedding, so both facts have to stay visible in the output.
  assert.match(src, /model\.reshape\(\{i\.get_any_name\(\): \[1, seq\]/);
  assert.match(src, /out\["mode"\] = "benchmark"/);
  assert.match(src, /out\["tokenizer"\] = False/);
});

test('no sidecar dependency set drags torch or optimum into a shared interpreter', () => {
  const all = [...SIDECAR_PACKAGES, ...MODEL_PACKAGES].map((p) => p.toLowerCase());
  for (const banned of ['torch', 'torchvision', 'optimum', 'optimum-intel', 'nncf']) {
    assert.ok(!all.some((p) => p.includes(banned)), `${banned} must never be a sidecar dependency`);
  }
  assert.ok(MODEL_PACKAGES.includes('huggingface-hub'));
});

test('the embedding model lands where findEmbedModel looks', () => withEnvScrubbed(() => {
  const root = tempRoot();
  const dest = embedTarget({ models_root: root });
  fs.mkdirSync(dest, { recursive: true });
  fs.writeFileSync(path.join(dest, 'openvino_model.xml'), '<net/>');
  assert.strictEqual(findEmbedModel({ models_root: root }), dest);
}));

test('status reports the interpreter list and what is missing before any probe', () => withEnvScrubbed(() => {
  const accelerator = new Accelerator({ python: process.execPath });
  const status = accelerator.status();
  assert.ok(status.packages.includes('openvino'));
  assert.ok(status.pythonChecked.length >= 1);
  assert.strictEqual(status.deps, null);
  assert.deepStrictEqual(status.missingDeps, []);
}));
