'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { compareVersions, refreshDesktop } = require('../electron/updates');

function stubFetch(body) {
  const original = globalThis.fetch;
  const state = { calls: 0 };
  globalThis.fetch = async () => {
    state.calls += 1;
    return body();
  };
  return { original, state };
}

test('compareVersions ranks release tags numerically', () => {
  assert.strictEqual(compareVersions('1.3.0', '1.2.9'), 1);
  assert.strictEqual(compareVersions('1.2.0', '1.2.0'), 0);
  assert.strictEqual(compareVersions('v1.10.0', 'v1.9.2'), 1);
  assert.strictEqual(compareVersions('1.2.0', '1.3.0'), -1);
});

test('refreshDesktop fetches once and serves the cache afterwards', async () => {
  const { original, state } = stubFetch(() => ({
    ok: true,
    json: async () => ({ tag_name: 'v9.9.9', html_url: 'https://x/y', body: 'notes', published_at: 'p', name: 'n' }),
  }));
  try {
    const first = await refreshDesktop('1.3.0', { force: true });
    assert.strictEqual(first.ok, true);
    assert.strictEqual(first.latest, '9.9.9');
    assert.strictEqual(first.available, true);
    assert.strictEqual(first.upToDate, false);
    const cached = await refreshDesktop('1.3.0');
    assert.strictEqual(state.calls, 1, 'the startup check must not re-hit the API');
    assert.strictEqual(cached.latest, '9.9.9');
  } finally {
    globalThis.fetch = original;
  }
});

test('a newer local build is not told it is behind', async () => {
  const { original } = stubFetch(() => ({
    ok: true,
    json: async () => ({ tag_name: 'v1.0.0', html_url: 'https://x/y', body: '', published_at: null, name: 'x' }),
  }));
  try {
    const info = await refreshDesktop('1.3.0', { force: true });
    assert.strictEqual(info.available, false);
    assert.strictEqual(info.upToDate, true);
  } finally {
    globalThis.fetch = original;
  }
});

test('an unreachable GitHub reports an error instead of throwing', async () => {
  const original = globalThis.fetch;
  globalThis.fetch = async () => { throw new Error('offline'); };
  try {
    const info = await refreshDesktop('1.3.0', { force: true });
    assert.strictEqual(info.ok, false);
    assert.match(info.error, /offline/);
  } finally {
    globalThis.fetch = original;
  }
});
