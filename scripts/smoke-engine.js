'use strict';
/** End-to-end smoke test of the app's own engine supervisor + agent endpoint. */
const { ColibriBackend } = require('../electron/backend');

const MODEL = process.env.SMOKE_MODEL || 'D:\\colibri-models\\Qwen3-Coder-30B-A3B-i4';
const PORT = Number(process.env.SMOKE_PORT || 8799);

(async () => {
  const b = new ColibriBackend();
  console.log('runtime:', b.runtime);
  console.log('python :', b.python);
  if (!b.runtime) process.exit(2);
  const t0 = Date.now();
  const st = await b.start(MODEL, { port: PORT, kvSlots: 1 });
  console.log('READY in', ((Date.now() - t0) / 1000).toFixed(1), 's');
  console.log('state  :', st.state);
  console.log('modelId:', st.modelId);
  console.log('plan   :', JSON.stringify(st.plan));

  const body = JSON.stringify({
    model: st.modelId,
    messages: [
      { role: 'system', content: 'You are Colibri Code, a coding agent. Answer with code only.' },
      { role: 'user', content: 'Write a Python function add(a, b) that returns a + b. Only code.' },
    ],
    max_tokens: 96,
    temperature: 0.2,
    stream: false,
  });
  const t1 = Date.now();
  const res = await fetch(`http://127.0.0.1:${PORT}/v1/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${b.apiKey}` },
    body,
  });
  const ms = Date.now() - t1;
  const j = await res.json();
  console.log('HTTP', res.status, 'in', (ms / 1000).toFixed(1), 's');
  console.log('usage:', JSON.stringify(j.usage || {}));
  const text = j.choices?.[0]?.message?.content ?? JSON.stringify(j);
  console.log('TEXT>>>');
  console.log(text.slice(0, 800));
  const stats = await b.stats();
  console.log('metrics:', JSON.stringify(stats.metrics).slice(0, 400));
  await b.stop();
  console.log('stopped; state', b.describe().state);
})().catch((err) => {
  console.error('SMOKE FAILED:', err.message);
  process.exit(1);
});
