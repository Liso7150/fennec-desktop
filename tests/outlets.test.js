'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { Outlets, OUTLET_SPECS, TOOL_OUTLETS, GOAL_CHARS, URL_CHARS, AGENT_CHARS,
  formatReport, readStatus, summarize } = require('../electron/outlets');
const { runTurn } = require('../electron/agent');

const BASE = 'http://127.0.0.1:8787';

function response(status, data) {
  return { ok: status >= 200 && status < 300, status, text: async () => JSON.stringify(data) };
}

function sse(payload) {
  return new TextEncoder().encode(`data: ${JSON.stringify(payload)}\n\ndata: [DONE]\n\n`);
}

const JEV_ONLY = {
  jev: { present: true, root: 'D:/jev-ultrafast', note: 'one round trip per page' },
  openbot: { present: false, url: 'http://127.0.0.1:3001', error: 'no OpenBot workspace answering' },
};

/** A fake Fennec service plus the engine replies a turn is allowed to make. */
function harness({ status = JEV_ONLY, engine = [], run = response(200, { ok: true, status: 'done', steps: 3, url: 'https://example.com', title: 'Example', text: 'the page' }) } = {}) {
  const calls = [];
  const fetchFn = async (input, init = {}) => {
    const url = new URL(typeof input === 'string' ? input : input.href);
    const key = `${init.method || 'GET'} ${url.pathname}`;
    calls.push({ key, headers: init.headers, body: init.body ? JSON.parse(init.body) : null, raw: init.body });
    if (url.pathname === '/api/outlets') return response(200, status);
    if (url.pathname === '/api/outlets/run') return run;
    if (url.pathname === '/v1/chat/completions') return { ok: true, status: 200, body: [engine.shift()] };
    throw new Error(`unexpected request ${key}`);
  };
  return { calls, fetchFn };
}

function outletsFor(fake, options = {}) {
  return new Outlets({ baseUrl: BASE, fetchFn: fake.fetchFn, ...options });
}

function engineBodies(fake) {
  return fake.calls.filter((call) => call.key === 'POST /v1/chat/completions')
    .map((call) => JSON.parse(call.raw));
}

function toolCallTurn(name, args, options) {
  const engine = [
    sse({ choices: [{ delta: { tool_calls: [{ index: 0, id: 'call_1', function: { name, arguments: JSON.stringify(args) } }] } }] }),
    sse({ choices: [{ delta: { content: 'reported back' } }], usage: { prompt_tokens: 4, completion_tokens: 3 } }),
  ];
  return harness({ engine, ...options });
}

function turnOptions(outlets, settings, approve) {
  return {
    engineUrl: 'http://127.0.0.1:8184',
    model: 'fennec-test',
    apiKey: '',
    workspace: process.cwd(),
    messages: [{ role: 'user', content: 'check the site' }],
    systemPrompt: 'test prompt',
    settings,
    onEvent: () => {},
    approve,
    outlets,
  };
}

test('the outlet client only speaks to a loopback Fennec service', () => {
  const fake = harness();
  assert.throws(() => new Outlets({ baseUrl: '', fetchFn: fake.fetchFn }), /need the local Fennec service URL/);
  assert.throws(() => new Outlets({ baseUrl: 'http://example.com:8787', fetchFn: fake.fetchFn }), /loopback/);
  assert.equal(new Outlets({ baseUrl: 'http://localhost:8787/', fetchFn: fake.fetchFn }).baseUrl, 'http://localhost:8787');
});

test('every request carries the client header, and the token only when set', async () => {
  const withKey = harness();
  const withToken = outletsFor(withKey, { appToken: 'sekret' });
  await withToken.refresh();
  assert.equal(withKey.calls[0].headers['X-Fennec-Client'], '1');
  assert.equal(withKey.calls[0].headers.authorization, 'Bearer sekret');

  const noKey = harness();
  const bare = outletsFor(noKey);
  await bare.refresh();
  assert.equal(noKey.calls[0].headers.authorization, undefined);
});

test('status decides which tools the model is offered', async () => {
  const outlets = outletsFor(harness());
  assert.deepEqual(outlets.specs(), [], 'nothing is advertised before a probe');
  await outlets.refresh();
  assert.deepEqual(outlets.specs().map((spec) => spec.function.name), ['web_task']);
  assert.equal(outlets.handles('web_task'), true);
  assert.equal(outlets.handles('coworker_task'), false);
  assert.equal(outlets.handles('read_file'), false);
  assert.match(outlets.summary(), /Jev ready · OpenBot not answering/);
  assert.deepEqual(OUTLET_SPECS.map((spec) => spec.function.name), Object.keys(TOOL_OUTLETS));
});

test('a service that is not running means no outlets, not a crash', async () => {
  const outlets = new Outlets({
    baseUrl: BASE,
    fetchFn: async () => { throw new Error('connect ECONNREFUSED 127.0.0.1:8787'); },
  });
  const snapshot = await outlets.refresh();
  assert.equal(snapshot.ok, false);
  assert.match(snapshot.error, /no Fennec service is answering.*fennec-agent serve/);
  assert.equal(outlets.state, null);
  assert.deepEqual(outlets.specs(), []);
  assert.match(outlets.summary(), /not checked\./);
  assert.equal(summarize(null), 'Browser outlets: not checked.');
});

test('a service that is up but slow is reported as the service, not the outlet', async () => {
  const outlets = new Outlets({
    baseUrl: BASE,
    fetchFn: async () => { throw new Error('socket hang up'); },
  });
  assert.match((await outlets.refresh()).error,
    /cannot reach the Fennec service for browser outlets: socket hang up/);
});

test('an HTTP refusal keeps the service advice', async () => {
  const fake = harness({
    run: response(409, { detail: 'jev is not available on this machine; run `fennec outlets` to see what is missing' }),
  });
  const outlets = outletsFor(fake);
  await outlets.refresh();
  await assert.rejects(() => outlets.run('web_task', { goal: 'open the page' }), /409.*run `fennec outlets`/);
});

test('web_task delegates to Jev with a clamped step budget', async () => {
  const fake = harness();
  const outlets = outletsFor(fake);
  await outlets.refresh();
  const text = await outlets.run('web_task', { goal: '  find the newest bug  ', url: 'https://example.com/issues', max_steps: 999 });
  const call = fake.calls.find((entry) => entry.key === 'POST /api/outlets/run');
  assert.deepEqual(call.body, { outlet: 'jev', goal: 'find the newest bug', url: 'https://example.com/issues', agent: '', max_steps: 40 });
  assert.match(text, /Jev stopped with status done after 3 steps/);
  assert.match(text, /https:\/\/example\.com — Example/);
  assert.match(text, /the page/);
});

test('a long goal is sent within the service caps, not bounced by a 422', async () => {
  const fake = harness();
  const outlets = outletsFor(fake);
  await outlets.refresh();
  await outlets.run('web_task', {
    goal: 'g'.repeat(GOAL_CHARS + 5000),
    url: 'https://example.com/' + 'u'.repeat(URL_CHARS),
    agent: 'a'.repeat(AGENT_CHARS + 40),
  });
  const call = fake.calls.find((entry) => entry.key === 'POST /api/outlets/run');
  assert.equal(call.body.goal.length, GOAL_CHARS);
  assert.ok(call.body.goal.endsWith('…'), call.body.goal.slice(-2));
  assert.equal(call.body.url.length, URL_CHARS);
  // The jev path does not carry an agent name, and the openbot path trims it to size.
  await outlets.run('coworker_task', { goal: 'short', agent: 'x'.repeat(AGENT_CHARS + 1) });
  assert.equal(fake.calls.at(-1).body.agent.length, AGENT_CHARS);
});

test('a junk step budget falls back to the default, never to zero', async () => {
  const fake = harness();
  const outlets = outletsFor(fake);
  await outlets.run('web_task', { goal: 'check', max_steps: 'many' });
  assert.equal(fake.calls[0].body.max_steps, 12);
  await outlets.run('web_task', { goal: 'check', max_steps: -4 });
  assert.equal(fake.calls[1].body.max_steps, 1);
});

test('coworker_task delegates to OpenBot and keeps its answer', async () => {
  const fake = harness({
    status: { jev: { present: false }, openbot: { present: true, url: 'http://127.0.0.1:3001', agents: [{ id: 'a1', name: 'Scout' }] } },
    run: response(200, { ok: true, outlet: 'openbot', thread: 't1', answer: 'the newest bug is #12', complete: true }),
  });
  const outlets = outletsFor(fake);
  await outlets.refresh();
  const text = await outlets.run('coworker_task', { goal: 'check the issue list', agent: 'Scout' });
  const call = fake.calls.find((entry) => entry.key === 'POST /api/outlets/run');
  assert.deepEqual(call.body, { outlet: 'openbot', goal: 'check the issue list', url: '', agent: 'Scout' });
  assert.match(text, /OpenBot finished \(thread t1\)/);
  assert.match(text, /the newest bug is #12/);
  assert.match(outlets.summary(), /OpenBot answering/);
});

test('a missing goal or an unknown tool never reaches the service', async () => {
  const fake = harness();
  const outlets = outletsFor(fake);
  await outlets.refresh();
  await assert.rejects(() => outlets.run('web_task', { goal: '   ' }), /web_task needs a goal/);
  await assert.rejects(() => outlets.run('browse_the_web', { goal: 'x' }), /unknown browser outlet tool/);
  await assert.rejects(() => outlets.run('web_task'), /needs a goal/);
  assert.deepEqual(fake.calls.map((entry) => entry.key), ['GET /api/outlets']);
});

test('the outlet report of a failure stays the outlet report', () => {
  assert.match(formatReport('web_task', { ok: false, error: 'jev outlet timed out after 240s' }), /jev outlet timed out/);
  assert.match(formatReport('coworker_task', { ok: false, error: { code: 500 } }), /reported a failure/);
  assert.match(formatReport('coworker_task', {}), /OpenBot ended early/);
  assert.match(formatReport('web_task', {}), /status unknown/);
  assert.match(readStatus({ jev: null, openbot: { present: true } }).jev.note, /Jev reported nothing/);
  assert.throws(() => readStatus(null), /no outlet status/);
});

test('a stopped turn is not blamed on the outlet', async () => {
  const controller = new AbortController();
  controller.abort();
  const outlets = new Outlets({
    baseUrl: BASE,
    fetchFn: async () => { throw Object.assign(new Error('This operation was aborted'), { name: 'AbortError' }); },
  });
  await assert.rejects(() => outlets.refresh({ signal: controller.signal }), /aborted/);
  assert.equal(outlets.error, '', 'a stop is not reported as a broken outlet');
});

/* --------------------------------------------------------- loop wiring */

test('an outlet call is asked for as a browser action, then delegated', async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const fake = toolCallTurn('web_task', { goal: 'open the issue list', url: 'https://example.com' });
  globalThis.fetch = fake.fetchFn;
  const outlets = outletsFor(fake);
  await outlets.refresh();
  const asked = [];
  const result = await runTurn(turnOptions(outlets, {}, async (request) => {
    asked.push(request);
    return true;
  }));
  assert.equal(asked.length, 1);
  assert.equal(asked[0].name, 'web_task');
  assert.equal(asked[0].browser, true, 'the dialog must know this is a browser action');
  assert.match(result.toolResults[0].output, /Jev stopped with status done/);
  const advertised = engineBodies(fake).at(-1).tools.map((spec) => spec.function.name);
  assert.ok(advertised.includes('web_task'), advertised.join(','));
  assert.ok(!advertised.includes('coworker_task'), advertised.join(','));
});

test('declining an outlet call never posts it', async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const fake = toolCallTurn('coworker_task', { goal: 'check the issue list' });
  globalThis.fetch = fake.fetchFn;
  const outlets = outletsFor(fake, { appToken: 't' });
  await outlets.refresh();
  const result = await runTurn(turnOptions(outlets, {}, async () => false));
  assert.match(result.toolResults[0].output, /the user declined this action/);
  assert.equal(fake.calls.filter((entry) => entry.key === 'POST /api/outlets/run').length, 0);
});

test('auto-approve skips the dialog and the switch off refuses it', async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const auto = toolCallTurn('web_task', { goal: 'check' });
  globalThis.fetch = auto.fetchFn;
  const outlets = outletsFor(auto);
  await outlets.refresh();
  let asked = 0;
  const approved = await runTurn(turnOptions(outlets, { autoApproveBrowser: true }, async () => { asked += 1; return false; }));
  assert.equal(asked, 0);
  assert.match(approved.toolResults[0].output, /Jev stopped/);

  const off = toolCallTurn('web_task', { goal: 'check' });
  globalThis.fetch = off.fetchFn;
  asked = 0;
  const refused = await runTurn(turnOptions(null, {}, async () => { asked += 1; return true; }));
  assert.equal(asked, 0, 'a switched-off outlet must not waste a dialog');
  assert.match(refused.toolResults[0].output, /browser outlets are switched off in Settings/);
  assert.equal(off.calls.filter((entry) => entry.key === 'POST /api/outlets/run').length, 0);
});

test('an absent outlet is not advertised, but core still has the last word', async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const fake = toolCallTurn('web_task', { goal: 'check' }, {
    run: response(409, { detail: 'jev is not available on this machine' }),
  });
  globalThis.fetch = fake.fetchFn;
  const outlets = outletsFor(fake);
  outlets.state = readStatus({ jev: { present: false }, openbot: { present: false } });
  const result = await runTurn(turnOptions(outlets, { autoApproveBrowser: true }, async () => true));
  const advertised = engineBodies(fake).at(-1).tools.map((spec) => spec.function.name);
  assert.ok(!advertised.includes('web_task'), advertised.join(','));
  assert.match(result.toolResults[0].output, /409: jev is not available on this machine/);
});
