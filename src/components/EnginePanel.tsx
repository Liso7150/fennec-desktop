import { Fragment, useEffect, useState } from 'react';
import type { AcceleratorStatus, BackendState, ModelInfo, ProbeResult, Settings } from '../types';

const DEVICES = ['NPU', 'GPU.0', 'GPU.1', 'CPU'];

interface Props {
  backend: BackendState;
  models: ModelInfo[];
  settings: Settings;
  onRefreshModels: () => Promise<void>;
  onStart: (modelDir: string) => Promise<void>;
  onStop: () => Promise<void>;
  onSettings: (patch: Partial<Settings>) => void;
}

type Report = { ok: boolean; report?: any; message?: string };

function Check({ name, status, detail }: { name: string; status: string; detail?: string }) {
  const tone = status === 'ok' ? 'ok' : status === 'warn' ? 'warn' : status === 'skip' ? 'skip' : 'bad';
  return (
    <div className={`check check-${tone}`}>
      <span className="check-dot" />
      <span className="check-name">{name}</span>
      <span className="check-detail">{detail}</span>
    </div>
  );
}

export default function EnginePanel({ backend, models, settings, onRefreshModels, onStart, onStop, onSettings }: Props) {
  const [selected, setSelected] = useState(settings.modelDir || '');
  const [doctor, setDoctor] = useState<Report | null>(null);
  const [busy, setBusy] = useState(false);
  const [stats, setStats] = useState<{ metrics: Record<string, number> } | null>(null);
  const [log, setLog] = useState('');
  const [accel, setAccel] = useState<AcceleratorStatus | null>(null);
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [probeBusy, setProbeBusy] = useState(false);
  const [indexBusy, setIndexBusy] = useState(false);
  const [indexMsg, setIndexMsg] = useState('');
  const [depsBusy, setDepsBusy] = useState(false);
  const [depsMsg, setDepsMsg] = useState('');

  useEffect(() => {
    if (settings.modelDir) setSelected(settings.modelDir);
  }, [settings.modelDir]);

  useEffect(() => {
    void window.fennec.npu.status().then(setAccel);
  }, []);

  const runProbe = async () => {
    setProbeBusy(true);
    try {
      const res = await window.fennec.npu.probe();
      setProbe(res);
      setAccel(await window.fennec.npu.status());
    } finally {
      setProbeBusy(false);
    }
  };

  const buildIndex = async () => {
    setIndexBusy(true);
    setIndexMsg('');
    try {
      const res = await window.fennec.npu.index(settings.npuDevice || 'NPU');
      setIndexMsg(
        res.ok
          ? `${res.files} files / ${res.chunks} chunks indexed · ${res.passes} model passes on `
            + `${res.device} at ${res.device_ms} ms each (${res.mode}${res.tokenizer ? '' : ', no tokenizer wired'})`
          : res.error || 'index failed',
      );
    } finally {
      setIndexBusy(false);
    }
  };

  const installDeps = async () => {
    setDepsBusy(true);
    setDepsMsg('Installing with pip — this can take a few minutes…');
    try {
      const res = await window.fennec.npu.ensure();
      const ready = res.ready ? ' (sidecar ready)' : ' (still not importable)';
      setDepsMsg(res.ok
        ? `${(res.packages || []).join(', ')} → ${res.python}${ready}`
        : `pip failed: ${res.output || res.error || 'unknown error'}`);
      setAccel(await window.fennec.npu.status());
    } finally {
      setDepsBusy(false);
    }
  };

  const installModel = async () => {
    setDepsBusy(true);
    setDepsMsg('Downloading the OpenVINO IR embedding model…');
    try {
      const res = await window.fennec.npu.fetchModel();
      setDepsMsg(res.ok
        ? `embedding model at ${res.model || res.dest}`
        : `download failed: ${res.error || 'unknown error'}`);
      setAccel(await window.fennec.npu.status());
    } finally {
      setDepsBusy(false);
    }
  };

  useEffect(() => {
    const tick = async () => {
      if (backend.state !== 'ready') return;
      try {
        const s = await window.fennec.engine.stats();
        setStats({ metrics: s.metrics });
      } catch {
        /* ignore */
      }
    };
    tick();
    const id = setInterval(tick, 3000);
    return () => clearInterval(id);
  }, [backend.state]);

  const inspect = async () => {
    if (!selected) return;
    setBusy(true);
    try {
      const res = await window.fennec.engine.doctor(selected);
      setDoctor(res as Report);
    } finally {
      setBusy(false);
    }
  };

  const checks: any[] = doctor?.report?.checks || [];
  const plan = backend.plan as any;
  const tps = stats?.metrics?.tokens_per_second ?? stats?.metrics?.completion_tokens_per_second;
  const hits = stats?.metrics?.expert_hit_rate ?? stats?.metrics?.hit_rate;

  return (
    <div className="panel-body">
      <section className="card">
        <div className="card-head">
          <h3>Engine</h3>
          <span className={`pill pill-${backend.state}`}>{backend.state}</span>
        </div>
        <div className="row gap">
          <select
            className="select"
            value={selected}
            onChange={(e) => setSelected(e.target.value)}
            onFocus={onRefreshModels}
          >
            <option value="">— choose a model —</option>
            {models.map((m) => (
              <option key={m.dir} value={m.dir}>
                {m.name} · {m.sizeGb} GB{m.container ? ' · fennec' : m.gguf ? ' · gguf' : ' · hf shards'}
              </option>
            ))}
          </select>
        </div>
        <div className="row gap">
          <button className="btn" onClick={inspect} disabled={!selected || busy}>Inspect plan</button>
          {backend.state === 'ready' ? (
            <button className="btn danger" onClick={onStop}>Stop engine</button>
          ) : (
            <button
              className="btn primary"
              disabled={!selected || backend.state === 'starting'}
              onClick={() => onStart(selected)}
            >
              {backend.state === 'starting' ? 'Loading…' : 'Start engine'}
            </button>
          )}
        </div>
        {backend.lastError && <div className="error-box">{backend.lastError}</div>}
        <div className="kv">
          <span>runtime</span><code>{backend.runtime || 'not found'}</code>
          <span>python</span><code>{backend.python}</code>
          <span>endpoint</span><code>http://127.0.0.1:{backend.port}/v1</code>
        </div>
      </section>

      {plan && (
        <section className="card">
          <div className="card-head"><h3>Placement plan</h3><span className="muted tiny">demand-paged experts</span></div>
          <div className="plan-grid">
            <div><span className="plan-label">VRAM hot tier</span><span className="plan-value">{plan.vram_hot_gb ?? plan.vram_gb ?? '—'} GB</span></div>
            <div><span className="plan-label">RAM warm tier</span><span className="plan-value">{plan.ram_warm_gb ?? plan.ram_gb ?? '—'} GB</span></div>
            <div><span className="plan-label">Cold on SSD</span><span className="plan-value">{plan.disk_cold_gb ?? plan.cold_gb ?? '—'} GB</span></div>
            <div><span className="plan-label">Residency</span><span className="plan-value">{plan.hit_pct ?? plan.residency ?? '—'}%</span></div>
          </div>
        </section>
      )}

      {backend.state === 'ready' && (
        <section className="card">
          <div className="card-head"><h3>Live</h3><span className="muted tiny">/metrics</span></div>
          <div className="plan-grid">
            <div><span className="plan-label">tokens/s</span><span className="plan-value">{tps ? Number(tps).toFixed(1) : '—'}</span></div>
            <div><span className="plan-label">expert hit</span><span className="plan-value">{hits ? `${(Number(hits) * 100).toFixed(0)}%` : '—'}</span></div>
            <div><span className="plan-label">uptime</span><span className="plan-value">{Math.round(backend.uptimeMs / 60000)}m</span></div>
          </div>
        </section>
      )}

      {!!checks.length && (
        <section className="card">
          <div className="card-head"><h3>Doctor</h3><span className="muted tiny">{doctor?.report?.policy || ''}</span></div>
          {checks.map((c, i) => (
            <Check key={i} name={c.id || c.name} status={c.status} detail={c.detail || c.message} />
          ))}
        </section>
      )}

      {doctor && !checks.length && (
        <section className="card"><pre className="log">{doctor.message || JSON.stringify(doctor.report, null, 2)}</pre></section>
      )}

      <section className="card">
        <div className="card-head">
          <h3>NPU &amp; accelerator sidecar</h3>
          <span className="muted tiny">off the token path</span>
        </div>
        <label className="field">
          <span>Device</span>
          <select
            className="select"
            value={settings.npuDevice || 'NPU'}
            onChange={(e) => onSettings({ npuDevice: e.target.value })}
          >
            {DEVICES.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </label>
        <label className="toggle">
          <input
            type="checkbox"
            checked={settings.npuEnabled !== false}
            onChange={(e) => onSettings({ npuEnabled: e.target.checked })}
          />
          <span>Keep the NPU available for sidecar work</span>
        </label>
        <div className="row gap">
          <button className="btn" onClick={runProbe} disabled={probeBusy || settings.npuEnabled === false}>
            {probeBusy ? 'Probing…' : 'Probe devices'}
          </button>
          <button className="btn" onClick={buildIndex} disabled={indexBusy || settings.npuEnabled === false}>
            {indexBusy ? 'Indexing…' : 'Build code index'}
          </button>
        </div>
        {indexMsg && <div className="muted tiny">{indexMsg}</div>}
        {probe?.error && <div className="error-box">{probe.error}</div>}
        {!!probe?.devices?.length && (
          <div className="kv">
            {probe.devices.map((d) => (
              <Fragment key={d.device}>
                <span>
                  {d.device} {d.name ? `· ${d.name}` : ''}
                </span>
                <code>
                  {d.status === 'absent'
                    ? 'not present'
                    : d.status === 'error'
                      ? d.error
                      : `compile ${d.compile_ms} ms · run ${d.infer_ms} ms`}
                </code>
              </Fragment>
            ))}
          </div>
        )}
        <div className="kv">
          <span>sidecar python</span>
          <code>{accel?.python || 'not found'}</code>
          <span>openvino</span>
          <code>{accel?.deps?.openvino || (accel?.missingDeps?.length ? 'missing' : 'not checked')}</code>
          <span>embedding model</span>
          <code>{accel?.embedModel || `not installed (looked in ${accel?.embedTarget || 'models_root/embed'})`}</code>
        </div>
        <div className="row gap">
          <button className="btn" onClick={() => void installDeps()} disabled={depsBusy || settings.npuEnabled === false}>
            {depsBusy ? 'Working…' : 'Install sidecar deps'}
          </button>
          <button className="btn" onClick={() => void installModel()} disabled={depsBusy || settings.npuEnabled === false}>
            Install embedding model
          </button>
        </div>
        {depsMsg && <div className="muted tiny">{depsMsg}</div>}
        <div className="muted tiny">
          {(accel?.packages || []).join(', ')} are pip-installed into whichever interpreter answers — the shared
          venv when it exists, otherwise the plain <code>python</code> from PATH. The model is the prebuilt IR
          build of <code>{accel?.embedModelId}</code> (<code>{accel?.embedIrRepo}</code>), downloaded with
          huggingface_hub; no exporter, and nothing pulls torch into that interpreter.
        </div>
        {accel?.note && <p className="muted tiny">{accel.note}</p>}
      </section>

      <section className="card">
        <div className="card-head">
          <h3>Options</h3>
        </div>
        <label className="field">
          <span>KV slots</span>
          <input
            type="number"
            min={1}
            max={8}
            value={settings.kvSlots}
            onChange={(e) => onSettings({ kvSlots: Number(e.target.value) })}
          />
        </label>
        <label className="field">
          <span>Port</span>
          <input
            type="number"
            value={settings.port}
            onChange={(e) => onSettings({ port: Number(e.target.value) })}
          />
        </label>
        <label className="field">
          <span>Temperature</span>
          <input
            type="number"
            step="0.05"
            value={settings.temperature}
            onChange={(e) => onSettings({ temperature: Number(e.target.value) })}
          />
        </label>
        <label className="toggle">
          <input
            type="checkbox"
            checked={settings.autoApproveWrites}
            onChange={(e) => onSettings({ autoApproveWrites: e.target.checked })}
          />
          <span>Apply file edits without asking</span>
        </label>
        <label className="toggle">
          <input
            type="checkbox"
            checked={settings.autoApproveCommands}
            onChange={(e) => onSettings({ autoApproveCommands: e.target.checked })}
          />
          <span>Run shell commands without asking</span>
        </label>
        <p className="muted tiny">
          Token generation stays on the Colibri engine: experts on the CPU tail, dense trunk in
          VRAM. The NPU and iGPU take the batched sidecar work below instead.
        </p>
      </section>

      <section className="card">
        <div className="card-head">
          <h3>Engine log</h3>
          <button className="btn tiny-btn" onClick={async () => setLog(await window.fennec.engine.log())}>Load</button>
        </div>
        <pre className="log">{log || 'press Load to read the tail'}</pre>
      </section>
    </div>
  );
}
