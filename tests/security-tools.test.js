'use strict';
const test = require('node:test');
const assert = require('node:assert');
const path = require('node:path');
const { safeExternalUrl } = require('../electron/security');
const { resolveInside } = require('../electron/tools');

test('safeExternalUrl allows only plain web links', () => {
  assert.strictEqual(safeExternalUrl('https://github.com/JustVugg/colibri'), 'https://github.com/JustVugg/colibri');
  assert.strictEqual(safeExternalUrl('http://127.0.0.1:8787/'), 'http://127.0.0.1:8787/');
  for (const value of ['javascript:alert(1)', 'file:///C:/Windows', 'mailto:a@b.c',
    'https://user:pw@example.com', 'not a url', 42, null]) {
    assert.strictEqual(safeExternalUrl(value), null, String(value));
  }
});

test('resolveInside keeps every path inside the workspace', () => {
  const root = path.resolve('D:/work/project');
  assert.strictEqual(resolveInside(root, 'src/app.ts'), path.join(root, 'src', 'app.ts'));
  assert.strictEqual(resolveInside(root, '.'), root);
  for (const target of ['../secrets.txt', '..', 'src/../../outside.txt', path.resolve('D:/Windows/System32')]) {
    assert.throws(() => resolveInside(root, target), /escapes the workspace/, target);
  }
});
