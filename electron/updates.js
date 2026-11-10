'use strict';
// Update checks for the desktop app and the git checkouts of the shared suite.
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const DESKTOP_REPO = 'Liso7150/colibri-code';
const SUITE_REPOS = ['colibri-code', 'colibri-core', 'colibri-cli', 'colibri-installer',
  'colibri-finetune', 'colibri-jetbrains', 'colibri-coding-agent'];
const ENV_SETUP_DIR = 'Colibri-Enviorment-Setup';

function gitPull(dir, timeout = 120000) {
  return new Promise((resolve) => {
    execFile('git', ['-C', dir, 'pull', '--ff-only'], { timeout, windowsHide: true, maxBuffer: 1 << 20 },
      (error, stdout, stderr) => resolve({
        code: error ? (error.code ?? 1) : 0,
        output: `${stdout || ''}${stderr || ''}`.trim(),
      }));
  });
}

async function checkDesktop(current) {
  let res;
  try {
    res = await fetch(`https://api.github.com/repos/${DESKTOP_REPO}/releases/latest`, {
      headers: { 'User-Agent': 'Colibri-Code', Accept: 'application/vnd.github+json' },
      signal: AbortSignal.timeout(15000),
    });
  } catch (err) {
    throw new Error(`cannot reach GitHub: ${err.message}`);
  }
  if (!res.ok) throw new Error(`GitHub API returned HTTP ${res.status}`);
  const data = await res.json();
  const latest = String(data.tag_name || '').replace(/^v/, '');
  return {
    current,
    latest,
    upToDate: Boolean(latest) && latest === current,
    url: data.html_url || `https://github.com/${DESKTOP_REPO}/releases`,
    name: data.name || latest,
    notes: String(data.body || '').slice(0, 1200),
    published: data.published_at || null,
  };
}

async function updateSuite(suiteRoot) {
  const roots = [suiteRoot, path.join(path.dirname(suiteRoot), ENV_SETUP_DIR)];
  const results = [];
  const seen = new Set();
  for (const root of roots) {
    const dirs = root === suiteRoot
      ? SUITE_REPOS.map((name) => [name, path.join(root, name)])
      : [[ENV_SETUP_DIR, path.join(root)]];
    for (const [name, dir] of dirs) {
      if (seen.has(dir) || !fs.existsSync(path.join(dir, '.git'))) continue;
      seen.add(dir);
      const result = await gitPull(dir);
      results.push({
        repo: name,
        ok: result.code === 0,
        output: result.output.split('\n').slice(-6).join('\n'),
      });
    }
  }
  if (!results.length) throw new Error('no git checkouts found under ' + suiteRoot);
  return { ok: results.every((row) => row.ok), results };
}

module.exports = { checkDesktop, updateSuite };
