'use strict';
// Local speech-to-text: spawn the fennec-core faster-whisper sidecar on demand.
const { spawn, execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');

class SpeechToText {
  constructor(config) {
    this.config = config;
    this.process = null;
    this.url = 'http://127.0.0.1:8183';
    this.logPath = path.join(config.cache_root, 'fennec', 'stt.log');
  }

  script() {
    return [
      path.join(this.config.suite_root, 'fennec-core', 'fennec_core', 'stt.py'),
      path.join(this.config.engine_root, 'fennec_core', 'stt.py'),
    ].find((candidate) => fs.existsSync(candidate)) || null;
  }

  health(timeout = 800) {
    return new Promise((resolve) => {
      const req = http.get(`${this.url}/health`, { timeout }, (res) => {
        let body = '';
        res.on('data', (chunk) => { body += chunk; });
        res.on('end', () => {
          try { resolve(JSON.parse(body)); } catch { resolve({ status: 'down' }); }
        });
      });
      req.on('error', () => resolve({ status: 'down' }));
      req.on('timeout', () => { req.destroy(); resolve({ status: 'down' }); });
    });
  }

  async ensure() {
    const health = await this.health();
    if (health.status === 'ok') return health;
    if (!this.process || this.process.exitCode != null) {
      const script = this.script();
      if (!script) throw new Error('fennec-core is not installed beside the suite; cannot start speech recognition');
      if (!this.config.python || !fs.existsSync(this.config.python)) throw new Error('the shared Python runtime is missing');
      fs.mkdirSync(path.dirname(this.logPath), { recursive: true });
      const log = fs.openSync(this.logPath, 'w');
      const env = { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1',
        HF_HOME: path.join(this.config.cache_root, 'huggingface') };
      this.process = spawn(this.config.python, [script, '--port', '8183'], {
        cwd: this.config.engine_root, env, windowsHide: true,
        stdio: ['ignore', log, log],
      });
      fs.closeSync(log);
    }
    const deadline = Date.now() + 300000;
    while (Date.now() < deadline) {
      const status = await this.health();
      if (status.status === 'ok') return status;
      if (this.process && this.process.exitCode != null) break;
      await new Promise((resolve) => setTimeout(resolve, 600));
    }
    throw new Error('speech model did not become ready; see ' + this.logPath);
  }

  async transcribe(audio, contentType = 'audio/webm') {
    await this.ensure();
    return await new Promise((resolve, reject) => {
      const req = http.request(`${this.url}/transcribe`, {
        method: 'POST', timeout: 180000,
        headers: { 'Content-Type': contentType, 'Content-Length': audio.length },
      }, (res) => {
        const chunks = [];
        res.on('data', (chunk) => chunks.push(chunk));
        res.on('end', () => {
          let data = {};
          try { data = JSON.parse(Buffer.concat(chunks).toString('utf8')); } catch { /* handled below */ }
          if (res.statusCode !== 200 || typeof data.text !== 'string') {
            reject(new Error(data.error || `speech sidecar returned HTTP ${res.statusCode}`));
          } else resolve(data);
        });
      });
      req.on('error', reject);
      req.on('timeout', () => req.destroy(new Error('speech transcription timed out')));
      req.end(audio);
    });
  }

  stop() {
    const child = this.process;
    this.process = null;
    if (!child || child.exitCode != null) return;
    if (process.platform === 'win32') {
      execFile('taskkill', ['/PID', String(child.pid), '/T', '/F'], () => {});
    } else {
      child.kill('SIGTERM');
    }
  }
}

module.exports = { SpeechToText };
