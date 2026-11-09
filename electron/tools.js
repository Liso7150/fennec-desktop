'use strict';
/**
 * The agent's hands: file and shell tools, sandboxed to the open workspace.
 *
 * Every path is resolved against the workspace root and refused when it escapes,
 * so a confused model cannot rewrite C:\Windows. Shell commands go through an
 * approval gate owned by the renderer unless the user turned the gate off.
 */
const fs = require('node:fs/promises');
const path = require('node:path');
const { exec } = require('node:child_process');

const IGNORE = new Set(['node_modules', '.git', 'dist', 'build', '__pycache__', '.venv', 'release']);
const MAX_READ_BYTES = 400_000;
const MAX_OUTPUT = 20_000;

function resolveInside(root, target) {
  const abs = path.resolve(root, target || '.');
  const rel = path.relative(root, abs);
  if (rel.startsWith('..') || path.isAbsolute(rel)) {
    throw new Error(`path escapes the workspace: ${target}`);
  }
  return abs;
}

function clip(text, limit = MAX_OUTPUT) {
  if (text.length <= limit) return text;
  return `${text.slice(0, limit)}\n… truncated (${text.length - limit} chars omitted)`;
}

async function readFile(root, { path: rel, offset = 1, limit = 2000 }) {
  const abs = resolveInside(root, rel);
  const stat = await fs.stat(abs);
  if (stat.isDirectory()) throw new Error(`${rel} is a directory; use list_directory`);
  if (stat.size > MAX_READ_BYTES * 4) throw new Error(`${rel} is ${(stat.size / 1e6).toFixed(1)} MB, too large to read`);
  const text = await fs.readFile(abs, 'utf8');
  const lines = text.split('\n');
  const from = Math.max(1, offset);
  const slice = lines.slice(from - 1, from - 1 + limit);
  const body = slice.map((l, i) => `${String(from + i).padStart(5)}| ${l}`).join('\n');
  const note = lines.length > from - 1 + limit ? `\n… ${lines.length} lines total` : '';
  return `${rel} (${lines.length} lines)\n${body}${note}`;
}

async function writeFile(root, { path: rel, content }) {
  const abs = resolveInside(root, rel);
  await fs.mkdir(path.dirname(abs), { recursive: true });
  await fs.writeFile(abs, content, 'utf8');
  return `wrote ${content.length} bytes to ${rel}`;
}

async function editFile(root, { path: rel, old_string, new_string, replace_all = false }) {
  const abs = resolveInside(root, rel);
  const text = await fs.readFile(abs, 'utf8');
  const count = text.split(old_string).length - 1;
  if (count === 0) throw new Error(`old_string not found in ${rel}`);
  if (count > 1 && !replace_all) throw new Error(`old_string appears ${count} times in ${rel}; add more context or set replace_all`);
  const next = replace_all ? text.split(old_string).join(new_string) : text.replace(old_string, new_string);
  await fs.writeFile(abs, next, 'utf8');
  return `edited ${rel} (${replace_all ? count : 1} replacement${count > 1 ? 's' : ''})`;
}

async function listDirectory(root, { path: rel = '.', depth = 1 }) {
  const abs = resolveInside(root, rel || '.');
  const out = [];
  async function walk(dir, prefix, level) {
    const entries = await fs.readdir(dir, { withFileTypes: true });
    entries.sort((a, b) => Number(b.isDirectory()) - Number(a.isDirectory()) || a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (IGNORE.has(entry.name)) continue;
      const childRel = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        out.push(`${childRel}/`);
        if (level < depth) await walk(path.join(dir, entry.name), childRel, level + 1);
      } else {
        let size = 0;
        try {
          size = (await fs.stat(path.join(dir, entry.name))).size;
        } catch {
          /* ignore */
        }
        out.push(`${childRel} (${size} B)`);
      }
      if (out.length > 500) return;
    }
  }
  await walk(abs, rel === '.' ? '' : rel.replace(/\/$/, ''), 1);
  return out.join('\n') || '(empty)';
}

async function searchFiles(root, { pattern, path: rel = '.', glob }) {
  // A tiny regex grep: no ripgrep dependency, good enough for a repo walk.
  const abs = resolveInside(root, rel || '.');
  let rx;
  try {
    rx = new RegExp(pattern, 'i');
  } catch (e) {
    throw new Error(`invalid pattern: ${e.message}`);
  }
  const exts = glob ? glob.split(',').map((g) => g.trim().replace(/^\*\.?/, '.')) : null;
  const hits = [];
  async function walk(dir) {
    let entries = [];
    try {
      entries = await fs.readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (hits.length >= 200) return;
      if (IGNORE.has(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(full);
        continue;
      }
      if (exts && !exts.some((e) => entry.name.endsWith(e))) continue;
      let stat;
      try {
        stat = await fs.stat(full);
      } catch {
        continue;
      }
      if (stat.size > MAX_READ_BYTES) continue;
      let text;
      try {
        text = await fs.readFile(full, 'utf8');
      } catch {
        continue;
      }
      text.split('\n').forEach((line, i) => {
        if (hits.length < 200 && rx.test(line)) {
          hits.push(`${path.relative(root, full).replace(/\\/g, '/')}:${i + 1}: ${line.trim().slice(0, 200)}`);
        }
      });
    }
  }
  await walk(abs);
  return hits.length ? hits.join('\n') : `no matches for /${pattern}/`;
}

function runCommand(root, { command, cwd = '.', timeout_ms = 120000 }) {
  const dir = resolveInside(root, cwd || '.');
  return new Promise((resolve) => {
    exec(
      command,
      { cwd: dir, timeout: Math.min(timeout_ms, 600000), maxBuffer: 8 * 1024 * 1024, windowsHide: true },
      (err, stdout, stderr) => {
        const code = err ? (err.code ?? 1) : 0;
        resolve({
          code,
          output: clip(`${stdout}${stderr ? `\n[stderr]\n${stderr}` : ''}`.trim() || '(no output)'),
          display: clip(`${stdout}${stderr}`.trim(), 4000),
        });
      },
    );
  });
}

const TOOL_SPECS = [
  {
    type: 'function',
    function: {
      name: 'read_file',
      description: 'Read a UTF-8 text file from the workspace. Returns numbered lines.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'Workspace-relative path' },
          offset: { type: 'integer', description: 'First line (1-based)' },
          limit: { type: 'integer', description: 'Max lines to return' },
        },
        required: ['path'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'write_file',
      description: 'Create or overwrite a file with the complete contents. Prefer edit_file for small changes.',
      parameters: {
        type: 'object',
        properties: { path: { type: 'string' }, content: { type: 'string' } },
        required: ['path', 'content'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'edit_file',
      description: 'Replace an exact string in a file. old_string must match exactly, including indentation.',
      parameters: {
        type: 'object',
        properties: {
          path: { type: 'string' },
          old_string: { type: 'string' },
          new_string: { type: 'string' },
          replace_all: { type: 'boolean' },
        },
        required: ['path', 'old_string', 'new_string'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'list_directory',
      description: 'List files and folders under a workspace path.',
      parameters: {
        type: 'object',
        properties: { path: { type: 'string' }, depth: { type: 'integer', description: '1-3' } },
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'search_files',
      description: 'Regex search across workspace files, returns path:line matches.',
      parameters: {
        type: 'object',
        properties: {
          pattern: { type: 'string' },
          path: { type: 'string' },
          glob: { type: 'string', description: 'Comma-separated extensions, e.g. ".ts,.tsx"' },
        },
        required: ['pattern'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'run_command',
      description: 'Run a shell command in the workspace and return its output. Use for builds, tests and git.',
      parameters: {
        type: 'object',
        properties: {
          command: { type: 'string' },
          cwd: { type: 'string' },
          timeout_ms: { type: 'integer' },
        },
        required: ['command'],
      },
    },
  },
  {
    type: 'function',
    function: {
      name: 'todo_write',
      description: 'Record the plan for the current task as a checklist. Call it again as items complete.',
      parameters: {
        type: 'object',
        properties: {
          todos: {
            type: 'array',
            items: {
              type: 'object',
              properties: {
                task: { type: 'string' },
                status: { type: 'string', enum: ['pending', 'in_progress', 'done'] },
              },
              required: ['task', 'status'],
            },
          },
        },
        required: ['todos'],
      },
    },
  },
];

const WRITE_TOOLS = new Set(['write_file', 'edit_file', 'run_command']);

module.exports = {
  TOOL_SPECS,
  WRITE_TOOLS,
  readFile,
  writeFile,
  editFile,
  listDirectory,
  searchFiles,
  runCommand,
  resolveInside,
};
