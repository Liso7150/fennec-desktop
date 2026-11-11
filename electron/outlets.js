'use strict';
/**
 * Browser outlets: Jev (jev-ultrafast) and OpenBot.
 *
 * The desktop has no browser code of its own. fennec-core already owns the two
 * outlet contracts, so this is a thin client for its local service: GET
 * /api/outlets to see what a machine has, POST /api/outlets/run to delegate one
 * task. Only a loopback service is ever addressed, and one that is not running
 * simply means "no outlets on this machine".
 */
const CLIENT_HEADER = 'X-Fennec-Client';

const TOOL_OUTLETS = { web_task: 'jev', coworker_task: 'openbot' };
const OUTLET_TOOLS = new Set(Object.keys(TOOL_OUTLETS));

const OUTLET_SPECS = [
  {
    type: 'function',
    function: {
      name: 'web_task',
      description:
        'Delegate one browser task to the local Jev browser agent and get the final page back. '
        + 'Describe the end state you want, not the clicks. Use it for pages you cannot read '
        + 'directly: logins, JavaScript apps, search results.',
      parameters: {
        type: 'object',
        properties: {
          goal: { type: 'string', description: 'The end state, e.g. "open the issues list and report the newest open bug"' },
          url: { type: 'string', description: 'Starting page (http or https)' },
          max_steps: { type: 'integer', description: 'Max page steps, 1-40. Default 12' },
        },
        required: ['goal'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'coworker_task',
      description:
        'Ask a local OpenBot coworker to do one web task and return its answer. Name the coworker '
        + 'when more than one is registered.',
      parameters: {
        type: 'object',
        properties: {
          goal: { type: 'string', description: 'The task, in one instruction' },
          agent: { type: 'string', description: 'OpenBot coworker id or name; optional when only one is registered' },
        },
        required: ['goal'],
      },
    },
  },
];

class ServiceError extends Error {}

/** The caps of the core service's OutletRequest; past them it answers 422. */
const GOAL_CHARS = 4000;
const URL_CHARS = 2000;
const AGENT_CHARS = 120;
const STEP_LIMITS = { min: 1, max: 40, default: 12 };

function clip(value, limit) {
  const text = String(value ?? '');
  return text.length <= limit ? text : `${text.slice(0, limit)}…`;
}

/** Send what the service can actually take, and say so in the text itself. */
function cut(value, limit) {
  const text = String(value ?? '');
  return text.length <= limit ? text : `${text.slice(0, limit - 1)}…`;
}

function trimmed(value) {
  return typeof value === 'string' ? value.trim() : '';
}

/** One outlet as the service reports it, reduced to what the UI and the loop need. */
function readOutlet(entry, name) {
  if (!entry || typeof entry !== 'object') return { present: false, note: `${name} reported nothing`, url: '', agents: [] };
  const note = trimmed(entry.error) || trimmed(entry.note);
  return {
    present: entry.present === true,
    note: note ? clip(note, 200) : '',
    url: trimmed(entry.url),
    agents: Array.isArray(entry.agents) ? entry.agents.slice(0, 20) : [],
  };
}

function readStatus(data) {
  if (!data || typeof data !== 'object') throw new Error('the Fennec service sent no outlet status');
  return { jev: readOutlet(data.jev, 'Jev'), openbot: readOutlet(data.openbot, 'OpenBot') };
}

function summarize(state) {
  if (!state) return 'Browser outlets: not checked.';
  return `Jev ${state.jev.present ? 'ready' : 'not installed'} · `
    + `OpenBot ${state.openbot.present ? 'answering' : 'not answering'}`;
}

/** What the model reads back: the outlet's own report, never a paraphrase. */
function formatReport(tool, data) {
  const report = data && typeof data === 'object' ? data : {};
  if (report.ok === false) {
    const detail = trimmed(report.error) || clip(JSON.stringify(report), 600);
    return `${TOOL_OUTLETS[tool]} reported a failure: ${clip(detail, 2000)}`;
  }
  if (tool === 'web_task') {
    const lines = [
      `Jev stopped with status ${trimmed(report.status) || 'unknown'} after ${report.steps ?? '?'} steps.`,
      report.url ? `Page: ${clip(report.url, 300)}${trimmed(report.title) ? ` — ${clip(report.title, 200)}` : ''}` : '',
      report.decision ? `Last decision: ${clip(JSON.stringify(report.decision), 600)}` : '',
      clip(trimmed(report.text) || '(no page text)', 6000),
    ];
    return lines.filter(Boolean).join('\n');
  }
  const lines = [
    `OpenBot ${report.complete ? 'finished' : 'ended early'}${report.thread ? ` (thread ${report.thread})` : ''}.`,
    clip(trimmed(report.answer) || '(no answer)', 6000),
  ];
  return lines.filter(Boolean).join('\n');
}

class Outlets {
  constructor({ baseUrl, appToken = '', fetchFn = globalThis.fetch, probeTimeoutMs = 8000, runTimeoutMs = {} } = {}) {
    if (typeof baseUrl !== 'string' || !baseUrl) throw new Error('outlets need the local Fennec service URL');
    const url = new URL(baseUrl);
    if (!/^(?:127(?:\.\d{1,3}){3}|localhost|\[::1\]|::1)$/.test(url.hostname)) {
      throw new Error('browser outlets are only driven through a loopback Fennec service');
    }
    this.baseUrl = url.origin;
    this.appToken = trimmed(appToken);
    this.fetchFn = fetchFn;
    this.probeTimeoutMs = probeTimeoutMs;
    // Core waits out the whole upstream task: 240 s for Jev, 900 s for OpenBot.
    this.runTimeoutMs = { web_task: 300000, coworker_task: 960000, ...runTimeoutMs };
    this.state = null;
    this.error = '';
  }

  headers(withBody) {
    const headers = { [CLIENT_HEADER]: '1', accept: 'application/json' };
    if (withBody) headers['content-type'] = 'application/json';
    if (this.appToken) headers.authorization = `Bearer ${this.appToken}`;
    return headers;
  }

  async request(path, { method = 'GET', body, signal, timeoutMs } = {}) {
    const limit = timeoutMs || this.probeTimeoutMs;
    const controller = new AbortController();
    let timedOut = false;
    const fromUser = () => controller.abort();
    const timer = setTimeout(() => { timedOut = true; controller.abort(); }, limit);
    if (signal) signal.addEventListener('abort', fromUser, { once: true });
    try {
      const res = await this.fetchFn(new URL(path, this.baseUrl), {
        method,
        headers: this.headers(body !== undefined),
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: controller.signal,
      });
      const raw = await res.text().catch(() => '');
      let payload = null;
      try {
        payload = raw ? JSON.parse(raw) : {};
      } catch {
        payload = null;
      }
      if (!res.ok) {
        const detail = payload && typeof payload.detail === 'string' ? payload.detail : clip(raw, 300);
        throw new ServiceError(`${method} ${path} returned ${res.status}${detail ? `: ${detail}` : ''}`);
      }
      return payload ?? {};
    } catch (err) {
      if (err instanceof ServiceError) throw err;
      if (signal?.aborted) throw err;
      if (timedOut) throw new Error(`the Fennec service did not answer ${path} within ${Math.round(limit / 1000)}s`);
      if (err.name === 'AbortError') throw err;
      const refused = /ECONNREFUSED|ECONNRESET|EPIPE|ENOENT/.test(err.message || '');
      throw new Error(refused
        ? `no Fennec service is answering on ${this.baseUrl}; browser outlets need \`fennec-agent serve\` running there`
        : `cannot reach the Fennec service for browser outlets: ${err.message}`);
    } finally {
      clearTimeout(timer);
      if (signal) signal.removeEventListener('abort', fromUser);
    }
  }

  /** Re-read what this machine has; a failure clears the state, so no tools are advertised. */
  async refresh({ signal, refresh = false, timeoutMs } = {}) {
    try {
      this.state = readStatus(await this.request(`/api/outlets${refresh ? '?refresh=true' : ''}`, { signal, timeoutMs }));
      this.error = '';
    } catch (err) {
      if (err.name === 'AbortError' || signal?.aborted) throw err;
      this.state = null;
      this.error = err.message;
    }
    return this.snapshot();
  }

  snapshot() {
    return { ok: !this.error, summary: this.summary(), outlets: this.state, error: this.error || null };
  }

  summary() {
    const line = summarize(this.state);
    return this.error ? `${line} (${this.error})` : line;
  }

  /** Only the tools whose outlet is actually here. */
  specs() {
    return OUTLET_SPECS.filter((spec) => this.handles(spec.function.name));
  }

  handles(name) {
    const outlet = TOOL_OUTLETS[name];
    return Boolean(outlet && this.state?.[outlet]?.present);
  }

  /** One delegated task, as the text the model is given. */
  async run(name, args = {}, { signal } = {}) {
    const outlet = TOOL_OUTLETS[name];
    if (!outlet) throw new Error(`unknown browser outlet tool ${name}`);
    const goal = trimmed(args.goal);
    if (!goal) throw new Error(`${name} needs a goal`);
    // Clamped here rather than refused there: a 422 from the service is a pydantic
    // blob the model cannot act on, and the task it wanted never starts at all.
    const body = { outlet, goal: cut(goal, GOAL_CHARS), url: '', agent: '' };
    if (outlet === 'jev') {
      const steps = Number(args.max_steps ?? args.maxSteps ?? STEP_LIMITS.default);
      body.max_steps = Number.isFinite(steps)
        ? Math.min(Math.max(Math.round(steps), STEP_LIMITS.min), STEP_LIMITS.max)
        : STEP_LIMITS.default;
      body.url = cut(trimmed(args.url), URL_CHARS);
    } else {
      body.agent = cut(trimmed(args.agent), AGENT_CHARS);
    }
    return formatReport(name, await this.request('/api/outlets/run', {
      method: 'POST', body, signal, timeoutMs: this.runTimeoutMs[name],
    }));
  }
}

module.exports = {
  Outlets,
  OUTLET_SPECS,
  OUTLET_TOOLS,
  TOOL_OUTLETS,
  CLIENT_HEADER,
  GOAL_CHARS,
  URL_CHARS,
  AGENT_CHARS,
  formatReport,
  readStatus,
  summarize,
};
