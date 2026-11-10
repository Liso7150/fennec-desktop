'use strict';
/**
 * The agent loop.
 *
 * One turn = stream a completion from the local Colibri engine, execute any tool
 * calls it asks for, feed the results back, repeat until it answers without tools.
 * Everything is reported through onEvent so the renderer can draw it live.
 */
const tools = require('./tools');

const MAX_STEPS = 24;

function sseLines(stream) {
  const decoder = new TextDecoder();
  let buffer = '';
  return {
    async *[Symbol.asyncIterator]() {
      for await (const chunk of stream) {
        buffer += decoder.decode(chunk, { stream: true });
        let idx;
        while ((idx = buffer.indexOf('\n')) >= 0) {
          const line = buffer.slice(0, idx).trim();
          buffer = buffer.slice(idx + 1);
          if (line.startsWith('data:')) yield line.slice(5).trim();
        }
      }
      const tail = buffer.trim();
      if (tail.startsWith('data:')) yield tail.slice(5).trim();
    },
  };
}

class Cancelled extends Error {}

async function runTurn(opts) {
  const {
    engineUrl,
    model,
    apiKey,
    workspace,
    messages,
    systemPrompt,
    settings = {},
    onEvent,
    approve,
    signal,
  } = opts;

  const convo = [{ role: 'system', content: systemPrompt }, ...messages];
  const toolResults = [];
  let assistantText = '';
  let reasoning = '';
  let totalPrompt = 0;
  let totalCompletion = 0;
  let tokensPerSecond = 0;
  const startedTurn = Date.now();

  for (let step = 0; step < MAX_STEPS; step += 1) {
    if (signal?.aborted) throw new Cancelled('stopped');

    onEvent({ type: 'step', step: step + 1 });
    const body = {
      model,
      messages: convo,
      stream: true,
      temperature: settings.temperature ?? 0.2,
      top_p: settings.topP ?? 0.9,
      max_tokens: settings.maxTokens ?? 4096,
      tools: tools.TOOL_SPECS,
      tool_choice: 'auto',
      stream_options: { include_usage: true },
    };

    const headers = { 'Content-Type': 'application/json' };
    if (apiKey) headers.Authorization = `Bearer ${apiKey}`;
    const res = await fetch(new URL('/v1/chat/completions', engineUrl), {
      method: 'POST',
      headers,
      body: JSON.stringify(body),
      signal,
    }).catch((err) => {
      if (err.name === 'AbortError') throw new Cancelled('stopped');
      throw new Error(`cannot reach the configured Colibri engine: ${err.message}`);
    });

    if (!res.ok) {
      const text = await res.text().catch(() => '');
      throw new Error(`engine returned ${res.status}: ${text.slice(0, 600)}`);
    }

    let stepText = '';
    const calls = new Map();
    const stepStart = Date.now();
    let stepTokens = 0;

    for await (const data of sseLines(res.body)) {
      if (signal?.aborted) throw new Cancelled('stopped');
      if (!data || data === '[DONE]') continue;
      let payload;
      try {
        payload = JSON.parse(data);
      } catch {
        continue;
      }
      if (payload.usage) {
        totalPrompt = payload.usage.prompt_tokens ?? totalPrompt;
        totalCompletion = payload.usage.completion_tokens ?? totalCompletion;
      }
      const choice = payload.choices?.[0];
      if (!choice) continue;
      const delta = choice.delta || {};
      if (delta.reasoning_content) {
        reasoning += delta.reasoning_content;
        onEvent({ type: 'reasoning', text: delta.reasoning_content });
      }
      if (delta.content) {
        assistantText += delta.content;
        stepText += delta.content;
        stepTokens += 1;
        onEvent({ type: 'assistant', text: delta.content });
      }
      if (Array.isArray(delta.tool_calls)) {
        for (const call of delta.tool_calls) {
          const idx = call.index ?? 0;
          const slot = calls.get(idx) || { id: call.id || `call_${idx}`, name: '', arguments: '' };
          if (call.id) slot.id = call.id;
          if (call.function?.name) slot.name += call.function.name;
          if (call.function?.arguments) slot.arguments += call.function.arguments;
          calls.set(idx, slot);
        }
      }
    }

    const elapsed = Math.max(0.001, (Date.now() - stepStart) / 1000);
    if (stepTokens > 0) tokensPerSecond = +(stepTokens / elapsed).toFixed(2);
    onEvent({ type: 'usage', promptTokens: totalPrompt, completionTokens: totalCompletion, tokensPerSecond });

    if (!calls.size) {
      onEvent({ type: 'done', text: assistantText, reasoning, toolResults, ms: Date.now() - startedTurn });
      return { text: assistantText, reasoning, toolResults };
    }

    const toolCalls = [...calls.values()];
    const assistantMessage = {
      role: 'assistant',
      content: stepText || null,
      tool_calls: toolCalls.map((c) => ({
        id: c.id,
        type: 'function',
        function: { name: c.name, arguments: c.arguments || '{}' },
      })),
    };
    convo.push(assistantMessage);

    for (const call of toolCalls) {
      if (signal?.aborted) throw new Cancelled('stopped');
      let args = {};
      try {
        args = call.arguments ? JSON.parse(call.arguments) : {};
      } catch {
        const message = `could not parse arguments as JSON: ${call.arguments?.slice(0, 200)}`;
        onEvent({ type: 'tool', phase: 'error', id: call.id, name: call.name, message });
        convo.push({ role: 'tool', tool_call_id: call.id, content: message });
        continue;
      }

      onEvent({ type: 'tool', phase: 'start', id: call.id, name: call.name, args });

      let content;
      try {
        if (tools.WRITE_TOOLS.has(call.name)) {
          const allowed =
            call.name === 'run_command'
              ? settings.autoApproveCommands === true
              : settings.autoApproveWrites === true;
          const verdict = allowed ? true : await approve({ id: call.id, name: call.name, args });
          if (!verdict) throw new Error('the user declined this action');
        }
        content = await executeTool(workspace, call.name, args, onEvent);
      } catch (err) {
        content = `error: ${err.message}`;
        onEvent({ type: 'tool', phase: 'error', id: call.id, name: call.name, message: err.message });
      }
      toolResults.push({ name: call.name, args, output: content });
      onEvent({ type: 'tool', phase: 'end', id: call.id, name: call.name, output: String(content).slice(0, 6000) });
      convo.push({ role: 'tool', tool_call_id: call.id, content: String(content).slice(0, 60000) });
    }
  }

  const message = `stopped after ${MAX_STEPS} steps without a final answer`;
  onEvent({ type: 'error', message });
  throw new Error(message);
}

async function executeTool(workspace, name, args, onEvent) {
  switch (name) {
    case 'read_file':
      return tools.readFile(workspace, args);
    case 'write_file':
      return tools.writeFile(workspace, args);
    case 'edit_file':
      return tools.editFile(workspace, args);
    case 'list_directory':
      return tools.listDirectory(workspace, args);
    case 'search_files':
      return tools.searchFiles(workspace, args);
    case 'run_command': {
      const result = await tools.runCommand(workspace, args);
      return `exit ${result.code}\n${result.output}`;
    }
    case 'todo_write':
      onEvent({ type: 'todos', todos: args.todos || [] });
      return 'plan recorded';
    default:
      throw new Error(`unknown tool ${name}`);
  }
}

module.exports = { runTurn, Cancelled };
