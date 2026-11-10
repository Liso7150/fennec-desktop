'use strict';
/**
 * Headless end-to-end check: start the real Colibri engine through the same
 * supervisor the app uses, then run one agent turn that must call tools and edit
 * a file. Run with: node scripts/e2e-agent.js <modelDir>
 */
const fs = require('node:fs');
const fsp = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { FennecBackend } = require('../electron/backend');
const { runTurn } = require('../electron/agent');

async function main() {
  const modelDir = process.argv[2];
  if (!modelDir) throw new Error('usage: node scripts/e2e-agent.js <modelDir>');
  const backend = new FennecBackend();

  console.log('runtime:', backend.runtime);
  console.log('python :', backend.python);

  const workspace = await fsp.mkdtemp(path.join(os.tmpdir(), 'fennec-e2e-'));
  await fsp.writeFile(path.join(workspace, 'calc.py'), 'def add(a, b):\n    return a - b\n', 'utf8');
  console.log('workspace:', workspace);

  const started = await backend.start(modelDir, { port: 8790, kvSlots: 1 });
  console.log('engine  :', started.state, '| model id:', started.modelId);

  const events = [];
  const result = await runTurn({
    port: started.port,
    model: started.modelId,
    apiKey: backend.apiKey,
    workspace,
    messages: [
      {
        role: 'user',
        content:
          'The file calc.py has a bug: add() subtracts instead of adding. Read the file, fix it with edit_file, then answer with one sentence.',
      },
    ],
    systemPrompt:
      'You are a coding agent. Use the tools to inspect and edit files. Keep the answer to one sentence after the fix.',
    settings: { temperature: 0.1, maxTokens: 512, autoApproveWrites: true, autoApproveCommands: true },
    approve: async () => true,
    onEvent: (e) => {
      events.push(e);
      if (e.type === 'tool') console.log(`  tool ${e.phase}: ${e.name}`);
      if (e.type === 'usage') console.log(`  usage: ${e.completionTokens} tok, ${e.tokensPerSecond} tok/s`);
      if (e.type === 'error') console.log('  error:', e.message);
    },
  });

  const after = await fsp.readFile(path.join(workspace, 'calc.py'), 'utf8');
  const toolsUsed = [...new Set(events.filter((e) => e.type === 'tool' && e.phase === 'start').map((e) => e.name))];
  const fileFixed = /a \+ b/.test(after);

  console.log('\n--- result ---');
  console.log('answer    :', (result.text || '').trim().slice(0, 300));
  console.log('tools used:', toolsUsed.join(', ') || '(none)');
  console.log('file fixed:', fileFixed);
  console.log('file now  :', JSON.stringify(after.trim()));

  await backend.stop();
  fs.rmSync(workspace, { recursive: true, force: true });

  if (!fileFixed) {
    console.error('\nFAIL: the agent did not fix the file');
    process.exit(1);
  }
  console.log('\nPASS: engine started, tools ran, file edited');
}

main().catch((err) => {
  console.error('FAIL:', err.message);
  process.exit(1);
});
