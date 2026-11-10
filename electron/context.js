'use strict';
/**
 * Context economy for the desktop agent loop — the JS twin of
 * fennec_core/context.py, so the panel and the core report the same figures.
 *
 * Prefill dominates a turn on the engine, so characters re-sent every step cost
 * real seconds. Only the payload sent to the engine is compacted: the transcript
 * the UI draws keeps every tool result in full.
 *
 *   * a result keeps its head and tail, with a marker for what was removed;
 *   * a result identical to an earlier one becomes a short reference;
 *   * a total budget shrinks the oldest results first, never the system prompt,
 *     the user task or the newest result.
 */
const crypto = require('node:crypto');

const DEFAULT_BUDGET_CHARS = 48000;
const DEFAULT_RESULT_CHARS = 6000;
const MIN_RESULT_CHARS = 600;

const marker = (removed) => `\n…[${removed} chars elided; narrow the read or grep for the rest]\n`;

function positiveInt(raw, fallback, floor) {
  const parsed = Number.parseInt(String(raw ?? '').trim(), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) return fallback;
  return Math.max(floor, parsed);
}

/** Budget and per-result caps; the FENNEC_AGENT_* variables win. */
function limits(env = process.env) {
  const resultChars = positiveInt(env.FENNEC_AGENT_RESULT_CHARS, DEFAULT_RESULT_CHARS, MIN_RESULT_CHARS);
  const budgetChars = positiveInt(env.FENNEC_AGENT_CONTEXT_BUDGET, DEFAULT_BUDGET_CHARS, 2000);
  return { budgetChars: Math.max(budgetChars, resultChars), resultChars };
}

function elide(text, limit) {
  if (typeof text !== 'string' || text.length <= limit) return text;
  const head = Math.max(1, Math.floor((limit * 2) / 3));
  const tail = Math.max(0, limit - head - 80);
  const removed = text.length - head - tail;
  return text.slice(0, head) + marker(removed) + (tail ? text.slice(-tail) : '');
}

function size(message) {
  let total = typeof message.content === 'string' ? message.content.length : 0;
  if (message.tool_calls) total += JSON.stringify(message.tool_calls).length;
  return total;
}

/**
 * Return `{ messages, stats }`; `messages` is never mutated.
 *
 * `seen` maps a result hash to the tool call that produced it, so duplicates are
 * caught across steps too; pass the same Map on every call in one turn.
 */
function compact(messages, {
  budgetChars = DEFAULT_BUDGET_CHARS,
  resultChars = DEFAULT_RESULT_CHARS,
  seen = new Map(),
  floorChars = MIN_RESULT_CHARS,
} = {}) {
  const charsBefore = messages.reduce((sum, message) => sum + size(message), 0);
  const payload = [];
  let elided = 0;
  let deduped = 0;

  for (const message of messages) {
    const copy = { ...message };
    const content = copy.content;
    if (copy.role === 'tool' && typeof content === 'string' && content) {
      const digest = crypto.createHash('sha256').update(content).digest('hex');
      const label = copy.tool_call_id || 'an earlier call';
      if (seen.has(digest)) {
        copy.content = `[identical to the result of ${seen.get(digest)}; ${content.length} chars not repeated]`;
        deduped += 1;
      } else {
        seen.set(digest, label);
        const capped = elide(content, resultChars);
        if (capped !== content) {
          copy.content = capped;
          elided += 1;
        }
      }
    }
    payload.push(copy);
  }

  let total = payload.reduce((sum, message) => sum + size(message), 0);
  for (const message of payload) {
    if (total <= budgetChars) break;
    const content = message.content;
    if (message.role !== 'tool' || typeof content !== 'string' || content.length <= floorChars) continue;
    const target = Math.max(floorChars, content.length - (total - budgetChars));
    if (target >= content.length) continue;
    message.content = elide(content, target);
    total = payload.reduce((sum, item) => sum + size(item), 0);
  }

  return {
    messages: payload,
    stats: { charsBefore, charsAfter: total, elided, deduped, budgetChars, resultChars },
  };
}

module.exports = { compact, elide, limits, DEFAULT_BUDGET_CHARS, DEFAULT_RESULT_CHARS, MIN_RESULT_CHARS };
