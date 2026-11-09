'use strict';
// Starts the Vite dev server, waits for it, then launches Electron against it.
const { spawn } = require('node:child_process');
const http = require('node:http');
const path = require('node:path');
const electron = require('electron');

const root = path.join(__dirname, '..');
const vite = spawn(process.execPath, [path.join(root, 'node_modules', 'vite', 'bin', 'vite.js')], {
  cwd: root,
  stdio: 'inherit',
  env: { ...process.env, ELECTRON_DEV: '1' },
});

function waitFor(url, tries = 60) {
  return new Promise((resolve, reject) => {
    const attempt = (n) => {
      http
        .get(url, (res) => {
          res.resume();
          resolve();
        })
        .on('error', () => (n <= 0 ? reject(new Error(`dev server never answered at ${url}`)) : setTimeout(() => attempt(n - 1), 400)));
    };
    attempt(tries);
  });
}

(async () => {
  await waitFor('http://127.0.0.1:5173');
  const app = spawn(electron, ['.'], {
    cwd: root,
    stdio: 'inherit',
    env: { ...process.env, ELECTRON_DEV: '1' },
  });
  app.on('exit', (code) => {
    vite.kill();
    process.exit(code ?? 0);
  });
})();
