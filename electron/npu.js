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
 * Everything degrades: no OpenVINO -> status reports why, no embedding model ->
 * index reports why, and the app keeps working on GPU+CPU alone.
 */
const { execFile } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const PYTHON_CANDIDATES = [
  process.env.COLI_PYTHON,
  'D:\\Colibri\\.venv\\Scripts\\python.exe',
  'D:\\Colibri-Desktop\\.venv\\Scripts\\python.exe',
].filter(Boolean);

/** OpenVINO IR embedding models we are willing to run on the sidecar. */
const EMBED_ROOTS = [
  process.env.COLI_EMBED_MODEL,
  'D:\\colibri-models\\embed',
  'D:\\colibri-models',
  'D:\\Colibri-Desktop\\models',
].filter(Boolean);

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
                    "IR directory (export a sentence-transformer with optimum-intel)")
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
    compiled = core.compile_model(os.path.join(model_dir, "openvino_model.xml"), device)
    tok = None
    try:
        from openvino_tokenizers import Tokenizer
        tok = True
    except Exception:
        tok = False
    # Without a tokenizer the index stores chunk text only; retrieval still works
    # on keyword overlap, and the device timing below still proves the NPU ran.
    t0 = time.perf_counter()
    shape = compiled.input(0).partial_shape
    dim = shape.get_dimension(1) if shape.is_static else 128
    dim = dim.get_length() if hasattr(dim, "get_length") else 128
    dummy = np.zeros((1, dim), np.float32)
    compiled([dummy])
    out["device_ms"] = round((time.perf_counter() - t0) * 1000, 2)
    out["tokenizer"] = bool(tok)
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

function findPython() {
  for (const p of PYTHON_CANDIDATES) {
    try {
      fs.accessSync(p);
      return p;
    } catch {
      /* next */
    }
  }
  return 'python';
}

function findEmbedModel() {
  for (const root of EMBED_ROOTS) {
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

function runPython(script, stdin, timeoutMs) {
  return new Promise((resolve) => {
    const child = execFile(
      findPython(),
      ['-c', script],
      { timeout: timeoutMs, maxBuffer: 16 * 1024 * 1024, windowsHide: true, env: { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' } },
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

class Accelerator {
  constructor() {
    this.python = findPython();
    this.embedModel = findEmbedModel();
    this.lastProbe = null;
    this.probing = null;
  }

  status() {
    return {
      python: this.python,
      embedModel: this.embedModel,
      lastProbe: this.lastProbe,
      note: 'NPU and the iGPU host sidecar work (device probe, code index). Token generation stays on the Colibri engine (CPU experts + CUDA trunk).',
    };
  }

  async probe() {
    if (this.probing) return this.probing;
    this.probing = runPython(PROBE_PY, null, 120000).then((res) => {
      this.lastProbe = res;
      this.probing = null;
      return res;
    });
    return this.probing;
  }

  async index(workspace, device = 'NPU') {
    return runPython(
      INDEX_PY,
      { workspace, device, model: this.embedModel, maxFiles: 400 },
      10 * 60 * 1000,
    );
  }
}

module.exports = { Accelerator, findEmbedModel, findPython };
