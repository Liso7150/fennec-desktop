'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

const { compact, elide, limits, DEFAULT_BUDGET_CHARS, DEFAULT_RESULT_CHARS, MIN_RESULT_CHARS } =
  require('../electron/context');
const { runTurn } = require('../electron/agent');

test('elide keeps the head and the tail of a long result', () => {
  const text = 'A'.repeat(5000) + 'MIDDLE' + 'Z'.repeat(5000);
  const out = elide(text, 1000);
  assert.ok(out.startsWith('A'.repeat(100)));
  assert.ok(out.includes('chars elided'));
  assert.ok(out.endsWith('Z'.repeat(100)));
  assert.ok(out.length < text.length);
  assert.strictEqual(elide('short', 10), 'short');
});

test('limits are the core defaults and FENNEC_AGENT_* overrides them', () => {
  assert.deepStrictEqual(limits({}), { budgetChars: DEFAULT_BUDGET_CHARS, resultChars: DEFAULT_RESULT_CHARS });
  assert.deepStrictEqual(
    limits({ FENNEC_AGENT_RESULT_CHARS: '1200', FENNEC_AGENT_CONTEXT_BUDGET: '9000' }),
    { budgetChars: 9000, resultChars: 1200 },
  );
  assert.deepStrictEqual(
    limits({ FENNEC_AGENT_RESULT_CHARS: '10' }),
    { budgetChars: DEFAULT_BUDGET_CHARS, resultChars: MIN_RESULT_CHARS },
  );
  // A budget below the per-result cap is raised, never the other way round.
  assert.strictEqual(
    limits({ FENNEC_AGENT_RESULT_CHARS: '8000', FENNEC_AGENT_CONTEXT_BUDGET: '3000' }).budgetChars,
    8000,
  );
});

test('a repeated tool result becomes a reference', () => {
  const messages = [
    { role: 'tool', tool_call_id: 'c1', content: 'x'.repeat(500) },
    { role: 'tool', tool_call_id: 'c2', content: 'x'.repeat(500) },
  ];
  const { messages: payload, stats } = compact(messages);
  assert.match(payload[1].content, /identical to the result of c1/);
  assert.strictEqual(stats.deduped, 1);
  assert.ok(stats.charsAfter < stats.charsBefore);
});

test('the budget shrinks the oldest results and never the system prompt or task', () => {
  const messages = [
    { role: 'system', content: 'S'.repeat(200) },
    { role: 'user', content: 'task' },
    { role: 'tool', tool_call_id: 'a', content: 'A'.repeat(4000) },
    { role: 'tool', tool_call_id: 'b', content: 'B'.repeat(4000) },
  ];
  const { messages: payload, stats } = compact(messages, { budgetChars: 5000 });
  assert.strictEqual(payload[0].content, 'S'.repeat(200));
  assert.strictEqual(payload[1].content, 'task');
  assert.ok(payload[3].content.length > payload[2].content.length);
  assert.ok(stats.charsAfter < stats.charsBefore);
});

test('compaction never mutates the transcript and seen carries across steps', () => {
  const original = { role: 'tool', tool_call_id: 'c', content: 'z'.repeat(9000) };
  compact([original], { resultChars: 600 });
  assert.strictEqual(original.content, 'z'.repeat(9000));

  const seen = new Map();
  compact([{ role: 'tool', tool_call_id: 'c1', content: 'same' }], { seen });
  const { messages: payload, stats } = compact(
    [{ role: 'tool', tool_call_id: 'c2', content: 'same' }],
    { seen },
  );
  assert.strictEqual(stats.deduped, 1);
  assert.match(payload[0].content, /identical to the result of c1/);
});

function sse(chunks) {
  return (async function* stream() {
    for (const chunk of chunks) yield Buffer.from(`data: ${JSON.stringify(chunk)}\n\n`);
  })();
}

test('the agent sends a compacted payload and reports the savings to the panel', async () => {
  const dir = await fsp.mkdtemp(path.join(os.tmpdir(), 'fennec-ctx-'));
  await fsp.writeFile(path.join(dir, 'big.txt'), 'x'.repeat(20000));
  const bodies = [];
  const original = globalThis.fetch;
  globalThis.fetch = async (_url, init) => {
    const body = JSON.parse(init.body);
    bodies.push(body);
    if (bodies.length === 1) {
      return {
        ok: true,
        body: sse([
          {
            choices: [
              {
                delta: {
                  tool_calls: [
                    { index: 0, id: 'c1', function: { name: 'read_file', arguments: JSON.stringify({ path: 'big.txt' }) } },
                  ],
                },
              },
            ],
          },
        ]),
      };
    }
    return {
      ok: true,
      body: sse([
        { choices: [{ delta: { content: 'done' } }] },
        { choices: [{ delta: {} }], usage: { prompt_tokens: 1200, completion_tokens: 2 } },
      ]),
    };
  };

  const events = [];
  try {
    await runTurn({
      engineUrl: 'http://127.0.0.1:8197',
      model: 'test-model',
      workspace: dir,
      messages: [{ role: 'user', content: 'read big.txt' }],
      systemPrompt: 'You are Fennec',
      onEvent: (event) => events.push(event),
      approve: async () => true,
    });
  } finally {
    globalThis.fetch = original;
    await fsp.rm(dir, { recursive: true, force: true });
  }

  assert.strictEqual(bodies.length, 2);
  assert.strictEqual(bodies[0].messages[0].role, 'system');
  const toolMessages = bodies[1].messages.filter((message) => message.role === 'tool');
  assert.strictEqual(toolMessages.length, 1);
  assert.ok(toolMessages[0].content.length < 20000);
  assert.match(toolMessages[0].content, /chars elided/);

  // Step 1 has no tool results yet, so its event carries zero savings; the last
  // event is the one the panel ends up showing for this turn.
  const usage = events.filter((event) => event.type === 'usage').pop();
  assert.ok(usage, 'a usage event reached the renderer');
  assert.ok(usage.contextSavedTokens > 0);
  assert.ok(usage.contextCharsBefore > usage.contextCharsAfter);
  assert.ok(Number.isFinite(usage.contextSavedSeconds) && usage.contextSavedSeconds >= 0);
});
