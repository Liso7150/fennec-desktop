'use strict';
/**
 * Accelerator sidecar (NPU / iGPU / dGPU via OpenVINO).
 *
 * The Colibri C engine generates tokens on CPU (MoE expert tail) + CUDA GPU
 * (dense trunk). The Intel AI Boost NPU has no path into that engine, and
 * autoregressive text on it was measured as incoherent, so it is kept OFF the
 * generation path on purpose. It is still a real device, and this module gives
 * it real work: device health probing and the workspace embedding index, both
 * of which are batched matmuls rather than autoregressive decode.
 *
 * Everything degrades: no OpenVINO -> status reports which interpreters were
 * tried and what is missing, no embedding model -> index reports why, and the
 * app keeps working on GPU+CPU alone.
 */
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

/** Packages the sidecar imports; installed with pip into the chosen interpreter. */
const SIDECAR_PACKAGES = Object.freeze(['openvino', 'openvino-tokenizers', 'numpy']);
/** Needed only to fetch the pre-exported IR model. Deliberately not optimum-intel:
 *  that pulls nncf + torch and pins an old transformers into the shared venv. */
const MODEL_PACKAGES = Object.freeze(['huggingface-hub']);
/** The sentence-transformer this sidecar models, and the public IR build of it. */
const EMBED_MODEL_ID = 'BAAI/bge-base-en-v1.5';
const EMBED_IR_REPO = 'OpenVINO/bge-base-en-v1.5-int8-ov';
const EMBED_DIR_NAME = 'bge-base-en-v1.5-int8-ov';

const CHECK_PY = String.raw`
import json, sys
out = {"python": sys.executable or "", "version": "%d.%d.%d" % sys.version_info[:3]}
for pkg in ("openvino", "openvino_tokenizers", "numpy", "huggingface_hub"):
    try:
        mod = __import__(pkg)
        out[pkg] = getattr(mod, "__version__", "?")
    except Exception:
        out[pkg] = None
print(json.dumps(out))
`;

/** Downloads a ready OpenVINO IR repo into dest. No export step, no torch. */
const FETCH_PY = String.raw`
import json, os, sys
args = json.loads(sys.stdin.read() or "{}")
repo = args.get("repo") or ""
dest = args.get("dest") or ""
out = {"ok": False, "repo": repo, "dest": dest}
if not repo or not dest:
    out["error"] = "repo and dest are required"
    print(json.dumps(out)); sys.exit(0)
try:
    from huggingface_hub import snapshot_download
except Exception as exc:
    out["error"] = "huggingface_hub not importable: %s" % exc
    print(json.dumps(out)); sys.exit(0)
try:
    snapshot_download(repo_id=repo, local_dir=dest,
                      allow_patterns=["*.xml", "*.bin", "*.json", "*.txt", "*.model"])
    out["ok"] = os.path.isfile(os.path.join(dest, "openvino_model.xml"))
    if not out["ok"]:
        out["error"] = "%s has no openvino_model.xml" % repo
except Exception as exc:
    out["error"] = "download failed: %s" % str(exc)[:400]
print(json.dumps(out))
`;

/** OpenVINO IR embedding models we are willing to run on the sidecar. */
function embedRoots(config = {}) {
  return [
    process.env.COLI_EMBED_MODEL,
    config.models_root && path.join(config.models_root, 'embed'),
    config.models_root,
  ].filter(Boolean);
}

const PROBE_PY = String.raw`
import json, sys, time
out = {"ok": False, "devices": [], "installed": False}
try:
    import openvino as ov
except Exception as exc:
    out["error"] = "openvino not importable: %s" % exc
    print(json.dumps(out)); sys.exit(0)
out["installed"] = True
out["version"] = getattr(ov, "__version__", "?")
try:
    core = ov.Core()
    names = list(core.available_devices)
except Exception as exc:
    out["error"] = "Core() failed: %s" % exc
    print(json.dumps(out)); sys.exit(0)
out["available"] = names
want = ["CPU", "GPU.0", "GPU.1", "NPU"]
try:
    import numpy as np
    from openvino import opset13 as ops
    a = ops.parameter([1, 256], np.float32, name="a")
    b = ops.parameter([256, 256], np.float32, name="b")
    mm = ops.matmul(a, b, False, False)
    model = ov.Model([mm], [a, b], "probe")
    x = np.ones((1, 256), np.float32)
    w = np.full((256, 256), 1.0 / 256.0, np.float32)
except Exception as exc:
    out["error"] = "graph build failed: %s" % exc
    print(json.dumps(out)); sys.exit(0)
for dev in want:
    rec = {"device": dev}
    if dev not in names:
        rec.update({"status": "absent"})
        out["devices"].append(rec); continue
    try:
        rec["name"] = core.get_property(dev, "FULL_DEVICE_NAME")
    except Exception:
        pass
    try:
        t0 = time.perf_counter()
        compiled = core.compile_model(model, dev)
        rec["compile_ms"] = round((time.perf_counter() - t0) * 1000, 1)
        t1 = time.perf_counter()
        res = compiled([x, w])[0]
        rec["infer_ms"] = round((time.perf_counter() - t1) * 1000, 2)
        rec["value"] = float(res[0][0]) if hasattr(res, "__len__") else None
        rec["status"] = "ok"
    except Exception as exc:
        rec["status"] = "error"
        rec["error"] = str(exc)[:300]
    out["devices"].append(rec)
out["ok"] = any(d.get("status") == "ok" for d in out["devices"] if d["device"] == "NPU")
print(json.dumps(out))
`;

const INDEX_PY = String.raw`
import json, os, sys, time
args = json.loads(sys.stdin.read() or "{}")
root = args.get("workspace") or ""
device = args.get("device") or "NPU"
model_dir = args.get("model") or ""
out = {"ok": False, "device": device, "chunks": 0, "files": 0}
if not root or not os.path.isdir(root):
    out["error"] = "workspace not set"; print(json.dumps(out)); sys.exit(0)
if not model_dir or not os.path.isdir(model_dir):
    out["error"] = ("no OpenVINO embedding model found; set COLI_EMBED_MODEL to an "
                    "IR directory or use Install embedding model in the sidecar panel")
    print(json.dumps(out)); sys.exit(0)
try:
    import openvino as ov
    import numpy as np
except Exception as exc:
    out["error"] = "openvino/numpy missing: %s" % exc
    print(json.dumps(out)); sys.exit(0)

SKIP = {"node_modules", ".git", "dist", "build", "__pycache__", ".venv", "release", "target"}
TEXT = {".py", ".js", ".ts", ".tsx", ".jsx", ".java", ".kt", ".c", ".h", ".cpp", ".cs",
        ".go", ".rs", ".rb", ".php", ".md", ".json", ".yml", ".yaml", ".toml", ".sql"}
MAX_FILES = int(args.get("maxFiles") or 400)
MAX_BYTES = 256 * 1024

def walk(root):
    for base, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in SKIP and not d.startswith(".")]
        for f in files:
            if os.path.splitext(f)[1].lower() in TEXT:
                yield os.path.join(base, f)

def chunks(text, size=1200, overlap=200):
    buf, i = [], 0
    while i < len(text):
        buf.append(text[i:i + size])
        i += max(1, size - overlap)
    return [c for c in buf if c.strip()]

files = list(walk(root))[:MAX_FILES]
docs = []
for fp in files:
    try:
        if os.path.getsize(fp) > MAX_BYTES:
            continue
        with open(fp, "r", encoding="utf-8", errors="ignore") as fh:
            txt = fh.read()
    except OSError:
        continue
    for n, ch in enumerate(chunks(txt)):
        docs.append({"path": os.path.relpath(fp, root).replace("\\\\", "/"), "chunk": n, "text": ch})
out["files"] = len(files)
out["chunks"] = len(docs)
if not docs:
    out["error"] = "nothing to index"; print(json.dumps(out)); sys.exit(0)
try:
    core = ov.Core()
    model = core.read_model(os.path.join(model_dir, "openvino_model.xml"))
    # The IR has dynamic batch/sequence axes and the NPU compiler refuses them,
    # so pin a static [1, seq] shape by input name before compiling.
    seq = int(args.get("seq") or 64)
    try:
        model.reshape({i.get_any_name(): [1, seq] for i in model.inputs})
    except Exception as exc:
        out["reshape_error"] = str(exc)[:200]
    compiled = None
    for attempt in (device, "CPU"):
        try:
            compiled = core.compile_model(model, attempt)
            if attempt != device:
                out["fallback_from"] = device
                device = attempt
            break
        except Exception as exc:
            out["error"] = "compile failed on %s: %s" % (attempt, str(exc)[:200])
    if compiled is None:
        print(json.dumps(out)); sys.exit(0)
    out["error"] = None
    inputs = []
    for inp in compiled.inputs:
        dims = [d.get_length() for d in inp.get_partial_shape()]
        dtype = np.int64 if inp.get_element_type().is_integral() else np.float32
        inputs.append(np.zeros(dims, dtype))
    passes = max(1, min(len(docs), 8))
    t0 = time.perf_counter()
    for _ in range(passes):
        result = compiled(inputs)
    vec = np.array(list(result.values())[0])
    out["device"] = device
    out["device_ms"] = round((time.perf_counter() - t0) * 1000 / passes, 2)
    out["passes"] = passes
    out["inputs"] = len(inputs)
    out["embedding_shape"] = [int(n) for n in vec.shape]
    # Token ids are padding zeros: with no tokenizer wired this run measures the
    # device, it does not embed the chunk text, so the index below stays text.
    out["tokenizer"] = False
    out["mode"] = "benchmark"
    out["ok"] = True
except Exception as exc:
    out["error"] = "index run failed on %s: %s" % (device, str(exc)[:300])
idx = os.path.join(os.path.expanduser("~"), ".colibri-code", "index", "workspace.json")
os.makedirs(os.path.dirname(idx), exist_ok=True)
try:
    with open(idx, "w", encoding="utf-8") as fh:
        json.dump({"root": root, "device": device, "docs": docs[:4000]}, fh)
    out["index"] = idx
except OSError as exc:
    out["warning"] = str(exc)[:200]
print(json.dumps(out))
`;

/**
 * Interpreters worth trying, best guess first. The engine venv is only the
 * preferred candidate, not a requirement: a plain `python` from PATH is tried
 * too, and the first one that can actually import OpenVINO wins.
 */
function pythonCandidates(config = {}) {
  const engineRoot = config.engine_root || '';
  const raw = [
    process.env.COLIBRI_PYTHON,
    process.env.COLI_PYTHON,
    config.python,
    engineRoot && path.join(engineRoot, '.venv', process.platform === 'win32' ? 'Scripts/python.exe' : 'bin/python'),
    'python',
  ].filter(Boolean);
  const found = [];
  for (const candidate of raw) {
    const key = candidate.replace(/\\/g, '/').toLowerCase();
    if (found.some((seen) => seen.replace(/\\/g, '/').toLowerCase() === key)) continue;
    if (candidate === 'python') {
      found.push(candidate);
      continue;
    }
    try {
      fs.accessSync(candidate);
    } catch {
      continue;
    }
    found.push(candidate);
  }
  return found;
}

function findPython(config = {}) {
  return pythonCandidates(config)[0] || 'python';
}

/** pip arguments for the sidecar packages; `--user` only outside a virtualenv. */
function installArgs(python, packages, config = {}) {
  const env = { PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' };
  if (config.cache_root) env.PIP_CACHE_DIR = path.join(String(config.cache_root).replace(/\\/g, '/'), 'pip');
  // A virtualenv keeps pyvenv.cfg beside Scripts/, so check both levels. A bare
  // name on PATH has no directory of its own, so it is never treated as a venv.
  const insideVenv = path.isAbsolute(python)
    && [path.dirname(python), path.join(path.dirname(python), '..')]
      .some((dir) => fs.existsSync(path.join(dir, 'pyvenv.cfg')));
  const argv = [python, '-m', 'pip', 'install', '--disable-pip-version-check', '--no-input',
    ...(insideVenv ? [] : ['--user']), ...packages];
  return { argv, env, insideVenv };
}

/** Where the fetched IR model lives: the download and the lookup share this. */
function embedTarget(config = {}) {
  const root = config.models_root;
  if (!root) return null;
  return path.join(String(root).replace(/\\/g, '/'), 'embed', EMBED_DIR_NAME);
}

function findEmbedModel(config = {}) {
  for (const root of embedRoots(config)) {
    try {
      if (!fs.statSync(root).isDirectory()) continue;
    } catch {
      continue;
    }
    const direct = path.join(root, 'openvino_model.xml');
    if (fs.existsSync(direct)) return root;
    let kids = [];
    try {
      kids = fs.readdirSync(root, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const kid of kids) {
      if (!kid.isDirectory()) continue;
      const candidate = path.join(root, kid.name, 'openvino_model.xml');
      if (fs.existsSync(candidate)) return path.join(root, kid.name);
    }
  }
  return null;
}

function runPython(script, stdin, timeoutMs, python = 'python', extraEnv = {}) {
  return new Promise((resolve) => {
    const child = execFile(
      python,
      ['-c', script],
      {
        timeout: timeoutMs,
        maxBuffer: 16 * 1024 * 1024,
        windowsHide: true,
        env: { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1', ...extraEnv },
      },
      (err, stdout, stderr) => {
        const text = (stdout || '').trim();
        const last = text.split('\n').filter(Boolean).pop();
        if (!last) {
          resolve({ ok: false, error: (stderr || String(err || 'no output')).slice(-1500) });
          return;
        }
        try {
          resolve(JSON.parse(last));
        } catch {
          resolve({ ok: false, error: text.slice(-1500) });
        }
      },
    );
    if (stdin !== null && stdin !== undefined) {
      child.stdin.write(JSON.stringify(stdin));
    }
    try {
      child.stdin.end();
    } catch {
      /* already closed */
    }
  });
}

/** Run an argv list (pip) and keep the tail of its output. */
function runArgs(argv, timeoutMs, extraEnv = {}) {
  return new Promise((resolve) => {
    const [command, ...args] = argv;
    execFile(
      command,
      args,
      {
        timeout: timeoutMs,
        maxBuffer: 32 * 1024 * 1024,
        windowsHide: true,
        env: { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1', ...extraEnv },
      },
      (error, stdout, stderr) => {
        const output = `${stdout || ''}\n${stderr || ''}`.split('\n').filter(Boolean).slice(-12).join('\n');
        resolve({ ok: !error, code: error ? (error.code ?? 1) : 0, output });
      },
    );
  });
}

class Accelerator {
  constructor(config = {}) {
    this.config = config;
    this.python = findPython(config);
    this.embedModel = findEmbedModel(config);
    this.lastProbe = null;
    this.probing = null;
    this.resolution = null;
    this.checks = [];
    this.missingDeps = [];
  }

  /** First interpreter that can import OpenVINO+numpy; memoized, runs on demand only. */
  async resolve() {
    if (this.resolution) return this.resolution;
    this.resolution = (async () => {
      const checks = [];
      for (const candidate of pythonCandidates(this.config)) {
        const res = await runPython(CHECK_PY, null, 30000, candidate);
        const row = { python: candidate, resolved: res.python || candidate, ...res };
        checks.push(row);
        if (row.openvino && row.numpy) {
          this.python = row.resolved || candidate;
          this.deps = row;
          this.checks = checks;
          this.missingDeps = [];
          return { ok: true, python: this.python, deps: row, checks };
        }
      }
      // Nothing usable: keep the preferred interpreter so pip installs land where
      // the other apps look, and report exactly what is missing.
      const best = checks.find((row) => row.openvino) || checks[0] || {};
      this.python = best.resolved || findPython(this.config);
      this.deps = best;
      this.checks = checks;
      this.missingDeps = SIDECAR_PACKAGES.filter((pkg) => !best[pkg.replace(/-/g, '_')]);
      return { ok: false, python: this.python, deps: best, checks, missing: this.missingDeps };
    })().catch((err) => {
      this.resolution = null;
      return { ok: false, error: String(err && err.message ? err.message : err) };
    });
    return this.resolution;
  }

  status() {
    const checked = this.checks.length ? this.checks : pythonCandidates(this.config);
    return {
      python: this.python,
      pythonChecked: checked,
      deps: this.deps || null,
      missingDeps: this.missingDeps,
      packages: SIDECAR_PACKAGES,
      modelPackages: MODEL_PACKAGES,
      embedModel: this.embedModel,
      embedTarget: embedTarget(this.config),
      embedModelId: EMBED_MODEL_ID,
      embedIrRepo: EMBED_IR_REPO,
      lastProbe: this.lastProbe,
      note: 'NPU and the iGPU host sidecar work (device probe, code index). Token generation stays on the Colibri engine '
        + '(CPU experts + optional GPU trunk). The index run is a shape-pinned benchmark until a tokenizer is wired, '
        + 'and the agent still reads code with grep today.',
    };
  }

  async probe() {
    if (this.probing) return this.probing;
    this.probing = this.resolve().then((picked) =>
      runPython(PROBE_PY, null, 120000, picked.python || this.python)
    ).then((res) => {
      if (res && res.ok === false && res.error && /openvino not importable/.test(res.error)) {
        res.hint = `Install the sidecar packages into ${this.python}: ${SIDECAR_PACKAGES.join(' ')}`;
      }
      this.lastProbe = res;
      this.probing = null;
      return res;
    });
    return this.probing;
  }

  async index(workspace, device = 'NPU') {
    const picked = await this.resolve();
    return runPython(
      INDEX_PY,
      { workspace, device, model: this.embedModel, maxFiles: 400 },
      10 * 60 * 1000,
      picked.python || this.python,
    );
  }

  /** pip install the sidecar packages (plus the model fetcher when asked). */
  async install({ includeModelTooling = false } = {}) {
    const packages = includeModelTooling
      ? [...SIDECAR_PACKAGES, ...MODEL_PACKAGES]
      : [...SIDECAR_PACKAGES];
    const python = this.python || findPython(this.config);
    const { argv, env, insideVenv } = installArgs(python, packages, this.config);
    const result = await runArgs(argv, 15 * 60 * 1000, env);
    this.resolution = null;
    this.checks = [];
    const recheck = await this.resolve();
    return {
      ...result,
      python: this.python,
      mode: insideVenv ? 'virtualenv' : 'user site',
      packages,
      ready: Boolean(recheck.ok),
      deps: this.deps,
    };
  }

  /**
   * Fetch the prebuilt OpenVINO IR embedding model into models_root/embed.
   * huggingface_hub is already a core dependency of the engine venv, so this
   * never pulls torch/optimum into an interpreter shared with other apps.
   */
  async fetchEmbedModel() {
    const dest = embedTarget(this.config);
    if (!dest) return { ok: false, error: 'models_root is not configured, so there is nowhere to put the model' };
    await this.resolve();
    if (!(this.deps && this.deps.huggingface_hub)) {
      const installed = await this.install({ includeModelTooling: true });
      if (!(this.deps && this.deps.huggingface_hub)) {
        return {
          ok: false,
          error: `could not install ${MODEL_PACKAGES.join(', ')} into ${this.python}`,
          output: installed.output,
        };
      }
    }
    const env = this.config.cache_root
      ? { HF_HOME: path.join(String(this.config.cache_root).replace(/\\/g, '/'), 'huggingface') }
      : {};
    const result = await runPython(FETCH_PY, { repo: EMBED_IR_REPO, dest }, 15 * 60 * 1000, this.python, env);
    this.embedModel = findEmbedModel(this.config);
    return { ...result, model: this.embedModel };
  }
}

module.exports = {
  Accelerator,
  findEmbedModel,
  findPython,
  pythonCandidates,
  installArgs,
  embedTarget,
  SIDECAR_PACKAGES,
  MODEL_PACKAGES,
  EMBED_MODEL_ID,
  EMBED_IR_REPO,
};
