import { useCallback, useEffect, useState } from 'react';
import type {
  DependencyStatus, EngineStatus, InstallResult, Prerequisite, PythonStatus,
  SetupDetection, SetupModel, SetupResult,
} from './types';

const STEPS = [
  { key: 'welcome', label: 'Welcome' },
  { key: 'prereq', label: 'Prerequisites' },
  { key: 'folder', label: 'Install folder' },
  { key: 'python', label: 'Python' },
  { key: 'deps', label: 'Dependencies' },
  { key: 'engine', label: 'Engine' },
  { key: 'models', label: 'Models' },
  { key: 'review', label: 'Review' },
] as const;

type StepKey = (typeof STEPS)[number]['key'];

const gb = (value: number) => (value >= 1000 ? `${(value / 1000).toFixed(1)} TB` : `${value.toFixed(1)} GB`);

export default function SetupWizard({ detection, firstRun, onClose }: {
  detection: SetupDetection;
  firstRun: boolean;
  onClose: () => void;
}) {
  const [step, setStep] = useState<StepKey>('welcome');
  const [root, setRoot] = useState(detection.roots.root);
  const [engineRoot, setEngineRoot] = useState(detection.roots.engine);
  const [modelsRoot, setModelsRoot] = useState(detection.roots.models);
  const [cacheRoot, setCacheRoot] = useState(detection.roots.cache);
  const [python, setPython] = useState('');
  const [pythonStatus, setPythonStatus] = useState<PythonStatus | null>(null);
  const [prereqs, setPrereqs] = useState<Prerequisite[]>([]);
  const [prereqLog, setPrereqLog] = useState('');
  const [deps, setDeps] = useState<DependencyStatus | null>(null);
  const [depsLog, setDepsLog] = useState('');
  const [engine, setEngine] = useState<EngineStatus | null>(null);
  const [models, setModels] = useState<SetupModel[]>(detection.models);
  const [model, setModel] = useState('');
  const [backend, setBackend] = useState(detection.device.metal ? 'metal' : detection.device.vulkan ? 'vulkan' : 'cpu');
  const [ctx, setCtx] = useState(detection.device.ramGb >= 16 ? 4096 : 2048);
  const [policy, setPolicy] = useState('quality');
  const [rootCheck, setRootCheck] = useState<{ ok: boolean; freeBytes: number; error?: string } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<SetupResult | null>(null);

  const index = STEPS.findIndex((entry) => entry.key === step);
  const go = (next: StepKey) => { setError(null); setStep(next); };
  const back = () => { const previous = STEPS[index - 1]; if (previous) go(previous.key); };

  const refreshPrereqs = useCallback(async (interpreter = '') => {
    setPrereqs(await window.fennec.setup.prerequisites({ python: interpreter }));
  }, []);

  useEffect(() => {
    void refreshPrereqs(python || '');
  }, [refreshPrereqs, python]);

  useEffect(() => {
    if (step !== 'python' || pythonStatus) return;
    let cancelled = false;
    setBusy('python');
    window.fennec.setup.findPython().then((result) => {
      if (cancelled) return;
      setPythonStatus(result);
      if (result.ok && result.command) setPython(result.command);
    }).finally(() => { if (!cancelled) setBusy(null); });
    return () => { cancelled = true; };
  }, [step, pythonStatus]);

  useEffect(() => {
    if (step !== 'engine') return;
    window.fennec.setup.validateEngine(engineRoot).then(setEngine);
  }, [step, engineRoot]);

  useEffect(() => {
    if (step !== 'models') return;
    window.fennec.setup.scanModels(modelsRoot).then((found) => {
      setModels(found);
      if (!model && found.length) setModel(found[0].path);
    });
  }, [step, modelsRoot, model]);

  const chooseFolder = async (setter: (value: string) => void, title: string, value: string) => {
    const picked = await window.fennec.setup.pickFolder({ title, defaultPath: value });
    if (picked) setter(picked);
  };

  const checkPython = async () => {
    if (!python.trim()) return;
    setBusy('python-check');
    setError(null);
    const result = await window.fennec.setup.checkPython(python.trim());
    setPythonStatus(result);
    setBusy(null);
  };

  const checkDeps = async () => {
    if (!pythonStatus?.command) return;
    setBusy('deps-check');
    setError(null);
    const result = await window.fennec.setup.checkDeps(pythonStatus.command);
    setDeps(result);
    setBusy(null);
  };

  const installDeps = async () => {
    if (!pythonStatus?.command) return;
    setBusy('deps-install');
    setError(null);
    setDepsLog('Installing Python dependencies…');
    let result: InstallResult;
    if (pythonStatus.hasPip === false) {
      const pip = await window.fennec.setup.ensurePip(pythonStatus.command);
      setDepsLog(`${pip.output || pip.error || ''}\n\nInstalling dependencies…`);
      if (!pip.ok) {
        setBusy(null);
        setError(pip.error || 'pip could not be enabled');
        return;
      }
    }
    result = await window.fennec.setup.installDeps({ python: pythonStatus.command, venv: pythonStatus.venv });
    setDepsLog(`${depsLog}\n${result.output || result.error || ''}`.trim());
    if (result.ok) setDeps(await window.fennec.setup.checkDeps(pythonStatus.command));
    else setError(result.error || `pip exited with code ${result.code ?? 1}`);
    setBusy(null);
  };

  const installPrerequisite = async (item: Prerequisite) => {
    setBusy(item.id);
    setError(null);
    setPrereqLog('');
    const result = await window.fennec.setup.installPrerequisite(item.id);
    setPrereqLog(result.error ? `${result.error}\n\n${result.command || ''}\n\n${result.output || ''}`.trim() : (result.output || 'Done.'));
    if (!result.ok) setError(result.error || 'Install failed');
    if (item.id === 'python' && result.ok) {
      const found = await window.fennec.setup.findPython();
      setPythonStatus(found);
      if (found.ok && found.command) setPython(found.command);
      await refreshPrereqs(found.command || '');
    } else {
      await refreshPrereqs(pythonStatus?.command || '');
    }
    setBusy(null);
  };

  const next = async () => {
    setError(null);
    if (step === 'folder') {
      const check = await window.fennec.setup.validateRoot(root);
      setRootCheck(check);
      if (!check.ok) { setError(check.error || 'That folder is not writable'); return; }
      if (!engineRoot || engineRoot === detection.roots.engine) setEngineRoot(`${root.replace(/[\\/]+$/, '')}/engine`.replace(/\\/g, '/'));
      if (!modelsRoot || modelsRoot === detection.roots.models) setModelsRoot(`${root.replace(/[\\/]+$/, '')}/models`.replace(/\\/g, '/'));
      if (!cacheRoot || cacheRoot === detection.roots.cache) setCacheRoot(`${root.replace(/[\\/]+$/, '')}/cache`.replace(/\\/g, '/'));
    }
    if (step === 'python' && !pythonStatus?.ok) { setError('Validate a Python 3.10+ interpreter first'); return; }
    if (step === 'deps' && !deps?.ok) { setError('Install the missing dependencies first'); return; }
    if (step === 'engine') {
      const result = await window.fennec.setup.validateEngine(engineRoot);
      setEngine(result);
      if (!result.ok) { setError(`Missing: ${result.missing.join(', ') || 'engine checkout'}`); return; }
    }
    const nextStep = STEPS[index + 1];
    if (nextStep) go(nextStep.key);
  };

  const finish = async (values: Record<string, unknown>) => {
    setBusy('save');
    setError(null);
    const result = await window.fennec.setup.save(values);
    setBusy(null);
    if (!result.ok) { setError(result.error || 'The configuration could not be saved'); return; }
    setSaved(result);
  };

  const saveConfig = () => finish({
    root, engineRoot, modelsRoot, cacheRoot, python: pythonStatus?.command || '',
    model, backend, ctx, policy,
  });

  if (saved) {
    return (
      <div className="setup">
        <div className="setup-card">
          <div className="setup-brand"><span className="logo">◧</span> Fennec setup</div>
          <h2>Fennec is configured</h2>
          <p className="muted">Saved {saved.path}. Fennec Desktop restarts now to use it.</p>
          <pre className="setup-log">{JSON.stringify(saved.config, null, 2)}</pre>
          <p className="muted tiny">Restarting… if the window stays open, close and start Fennec Desktop again.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="setup">
      <div className="setup-card">
        <div className="setup-brand">
          <span className="logo">◧</span>
          <div>
            <strong>Fennec setup</strong>
            <span className="tiny muted">{firstRun ? 'first run' : 're-run'} · {detection.device.platform}/{detection.device.arch}</span>
          </div>
          {!firstRun && <button className="icon-btn setup-close" onClick={onClose} title="Close">✕</button>}
        </div>

        <ol className="setup-rail">
          {STEPS.map((entry, position) => (
            <li key={entry.key} className={position === index ? 'active' : position < index ? 'done' : ''}>
              <button onClick={() => { if (position <= index) go(entry.key); }} disabled={position > index}>
                <span>{position < index ? '✓' : position + 1}</span>{entry.label}
              </button>
            </li>
          ))}
        </ol>

        <div className="setup-body">
          {error && <div className="error-box">{error}</div>}

          {step === 'welcome' && (
            <>
              <h2>Welcome to Fennec</h2>
              <p className="muted">
                This wizard finds what is missing on this machine, asks where the engine and models live,
                and writes one shared configuration every Fennec app reads. Nothing is downloaded or
                installed without an explicit click.
              </p>
              <div className="setup-grid">
                <div><span className="setup-label">Detected</span><strong>{detection.device.platform} · {detection.device.arch}</strong></div>
                <div><span className="setup-label">CPU / RAM</span><strong>{detection.device.cores} cores · {gb(detection.device.ramGb)}</strong></div>
                <div><span className="setup-label">GPU</span><strong>{detection.device.metal ? 'Metal' : detection.device.vulkan ? 'Vulkan loader found' : 'CPU only'}</strong></div>
                <div><span className="setup-label">Free space</span><strong>{gb(detection.device.freeGb)}</strong></div>
              </div>
              <p className="muted tiny">Config file: <code>{detection.configPath}</code></p>
              <p className="muted tiny">
                The engine stays upstream <code>JustVugg/colibri</code>; fine-tuning uses{' '}
                <code>lyogavin/airllm</code>. No Ollama, no bundled weights.
              </p>
            </>
          )}

          {step === 'prereq' && (
            <>
              <h2>Prerequisites</h2>
              <p className="muted">Python runs the engine and the shared core. Git is only needed to clone or update the engine.</p>
              {prereqs.map((item) => (
                <div className="setup-item" key={item.id}>
                  <div>
                    <strong>{item.name}</strong>
                    <span className={`tiny ${item.installed ? 'ok-text' : 'warn-text'}`}>
                      {item.installed ? 'found' : item.required ? 'missing' : 'not found'}
                    </span>
                    <div className="tiny muted">{item.detail}</div>
                  </div>
                  {!item.installed && (
                    <div className="setup-actions">
                      {item.install && (
                        <button className="btn" disabled={busy === item.id} onClick={() => void installPrerequisite(item)}>
                          {busy === item.id ? 'Installing…' : 'Install automatically'}
                        </button>
                      )}
                      <button className="btn ghost" onClick={() => void window.fennec.setup.openDownload(item.id)}>
                        Download page
                      </button>
                    </div>
                  )}
                </div>
              ))}
              <p className="muted tiny">On Linux administrator rights are never requested silently; the exact command is shown for you to run.</p>
              {prereqLog && <pre className="setup-log">{prereqLog}</pre>}
            </>
          )}

          {step === 'folder' && (
            <>
              <h2>Install folder</h2>
              <p className="muted">Everything derived from this root: <code>engine/</code>, <code>suite/</code>, <code>models/</code>, <code>cache/</code>.</p>
              <label className="setup-field">
                <span>Root folder</span>
                <div className="setup-input">
                  <input value={root} onChange={(e) => setRoot(e.target.value)} spellCheck={false} />
                  <button className="btn ghost" onClick={() => void chooseFolder(setRoot, 'Choose the install folder', root)}>Browse…</button>
                </div>
              </label>
              {rootCheck && <p className={`tiny ${rootCheck.ok ? 'ok-text' : 'bad-text'}`}>{rootCheck.ok ? `${gb(rootCheck.freeBytes / 1e9)} free` : rootCheck.error}</p>}
              <div className="setup-grid">
                <div><span className="setup-label">Engine</span><code>{engineRoot}</code></div>
                <div><span className="setup-label">Models</span><code>{modelsRoot}</code></div>
                <div><span className="setup-label">Suite</span><code>{root.replace(/[\\/]+$/, '')}/suite</code></div>
                <div><span className="setup-label">Cache</span><code>{cacheRoot}</code></div>
              </div>
              <p className="muted tiny">Model weights are not bundled. A 35B int4 container needs about 23 GB, so keep the models folder on a drive with room.</p>
            </>
          )}

          {step === 'python' && (
            <>
              <h2>Python</h2>
              <p className="muted">Fennec runs the engine on <code>python</code>; no separate virtual environment is required.</p>
              <label className="setup-field">
                <span>Interpreter</span>
                <div className="setup-input">
                  <input value={python} onChange={(e) => { setPython(e.target.value); setPythonStatus(null); }} placeholder="python.exe or /usr/bin/python3" spellCheck={false} />
                  <button className="btn ghost" onClick={() => void chooseFolder(setPython, 'Choose the Python interpreter', python)}>Browse…</button>
                  <button className="btn" disabled={busy === 'python-check'} onClick={() => void checkPython()}>
                    {busy === 'python-check' ? 'Checking…' : 'Check'}
                  </button>
                </div>
              </label>
              {busy === 'python' && <p className="muted tiny">Looking for Python 3.10+…</p>}
              {pythonStatus && (
                pythonStatus.ok ? (
                  <p className="tiny ok-text">
                    Python {pythonStatus.version} · {pythonStatus.venv ? 'virtual environment' : 'system interpreter'} ·
                    {pythonStatus.hasPip ? ' pip available' : ' pip missing'}
                  </p>
                ) : (
                  <p className="tiny bad-text">{pythonStatus.error || 'No usable Python found.'}</p>
                )
              )}
              {pythonStatus && !pythonStatus.ok && (
                <button className="link-btn" onClick={() => void window.fennec.setup.openDownload('python')}>Open the python.org download page</button>
              )}
              {pythonStatus?.ok && !pythonStatus.hasPip && (
                <p className="muted tiny">pip is missing; the dependency step can enable it with <code>ensurepip</code>.</p>
              )}
              {!!pythonStatus?.tried?.length && !pythonStatus.ok && (
                <p className="muted tiny">Tried: {pythonStatus.tried.join(', ')}</p>
              )}
            </>
          )}

          {step === 'deps' && (
            <>
              <h2>Python dependencies</h2>
              <p className="muted">The shared core needs fastapi, uvicorn, psutil and pydantic.</p>
              <button className="btn" disabled={busy === 'deps-check' || !pythonStatus?.ok} onClick={() => void checkDeps()}>
                {busy === 'deps-check' ? 'Checking…' : 'Check dependencies'}
              </button>
              {deps && (
                <div className="setup-deps">
                  {(deps.modules || []).map((name) => (
                    <span key={name} className={deps.missing?.includes(name) ? 'chip chip-bad' : 'chip chip-ok'}>
                      {name} {deps.missing?.includes(name) ? '· missing' : '· ok'}
                    </span>
                  ))}
                </div>
              )}
              {deps && !deps.ok && (
                <button className="btn primary" disabled={busy === 'deps-install'} onClick={() => void installDeps()}>
                  {busy === 'deps-install' ? 'Installing…' : 'Install missing dependencies'}
                </button>
              )}
              {(depsLog || deps?.error) && <pre className="setup-log">{depsLog || deps?.error}</pre>}
            </>
          )}

          {step === 'engine' && (
            <>
              <h2>Engine folder</h2>
              <p className="muted">The engine is upstream <code>JustVugg/colibri</code>. Fennec never renames or forks it.</p>
              <label className="setup-field">
                <span>Engine checkout</span>
                <div className="setup-input">
                  <input value={engineRoot} onChange={(e) => { setEngineRoot(e.target.value); setEngine(null); }} spellCheck={false} />
                  <button className="btn ghost" onClick={() => void chooseFolder(setEngineRoot, 'Choose the engine checkout', engineRoot)}>Browse…</button>
                </div>
              </label>
              <p className={`tiny ${engine?.ok ? 'ok-text' : 'warn-text'}`}>
                {engine?.ok ? 'Engine found (c/coli and c/openai_server.py).' : `Needs c/coli and c/openai_server.py${engine?.missing?.length ? `; missing ${engine.missing.join(', ')}` : ''}.`}
              </p>
              <div className="row gap">
                <button className="btn" onClick={async () => setEngine(await window.fennec.setup.validateEngine(engineRoot))}>Check folder</button>
                <button className="btn ghost" disabled={busy === 'clone'} onClick={async () => {
                  setBusy('clone');
                  setError(null);
                  const result = await window.fennec.setup.cloneEngine(engineRoot);
                  setEngine(await window.fennec.setup.validateEngine(engineRoot));
                  if (!result.ok) setError(result.error || result.output || 'Clone failed');
                  setBusy(null);
                }}>
                  {busy === 'clone' ? 'Cloning…' : 'Clone upstream (needs Git)'}
                </button>
              </div>
              {engine && !engine.ok && (
                <p className="muted tiny">You can also run <code>python install.py</code> from fennec-installer and pick this engine folder.</p>
              )}
            </>
          )}

          {step === 'models' && (
            <>
              <h2>Models folder</h2>
              <label className="setup-field">
                <span>Models</span>
                <div className="setup-input">
                  <input value={modelsRoot} onChange={(e) => setModelsRoot(e.target.value)} spellCheck={false} />
                  <button className="btn ghost" onClick={() => void chooseFolder(setModelsRoot, 'Choose the models folder', modelsRoot)}>Browse…</button>
                  <button className="btn" onClick={async () => setModels(await window.fennec.setup.scanModels(modelsRoot))}>Scan</button>
                </div>
              </label>
              {models.length ? (
                <div className="setup-models">
                  {models.map((entry) => (
                    <label key={entry.path} className={`setup-model${model === entry.path ? ' selected' : ''}`}>
                      <input type="radio" name="model" checked={model === entry.path} onChange={() => setModel(entry.path)} />
                      <span>
                        <strong>{entry.name}</strong>
                        <span className="tiny muted">{entry.sizeGb} GB · {entry.shards} weight file(s) · {entry.ready ? 'ready' : 'incomplete'}</span>
                        <code className="tiny">{entry.path}</code>
                      </span>
                    </label>
                  ))}
                </div>
              ) : (
                <p className="muted tiny">No model folders found here. You can finish now; the engine cannot start until a model is installed.</p>
              )}
              {models.length > 0 && (
                <button className="link-btn" onClick={() => setModel('')}>No model for now</button>
              )}
              <div className="setup-grid">
                <label className="setup-field slim">
                  <span>Backend</span>
                  <select value={backend} onChange={(e) => setBackend(e.target.value)}>
                    {['auto', 'vulkan', 'metal', 'cpu'].map((option) => <option key={option} value={option}>{option}</option>)}
                  </select>
                </label>
                <label className="setup-field slim">
                  <span>Context</span>
                  <input type="number" min={0} max={1048576} step={1024} value={ctx} onChange={(e) => setCtx(Number(e.target.value))} />
                </label>
                <label className="setup-field slim">
                  <span>Policy</span>
                  <select value={policy} onChange={(e) => setPolicy(e.target.value)}>
                    {['quality', 'balanced', 'experimental-fast'].map((option) => <option key={option} value={option}>{option}</option>)}
                  </select>
                </label>
              </div>
            </>
          )}

          {step === 'review' && (
            <>
              <h2>Review</h2>
              <p className="muted">This is written to <code>{detection.configPath}</code>. An existing file is backed up first.</p>
              <div className="setup-grid">
                <div><span className="setup-label">Install root</span><code>{root}</code></div>
                <div><span className="setup-label">Engine</span><code>{engineRoot}</code></div>
                <div><span className="setup-label">Models</span><code>{modelsRoot}</code></div>
                <div><span className="setup-label">Cache</span><code>{cacheRoot}</code></div>
                <div><span className="setup-label">Python</span><code>{pythonStatus?.command || python || 'python from PATH'}</code></div>
                <div><span className="setup-label">Model</span><code>{model || 'none yet'}</code></div>
                <div><span className="setup-label">Backend / context</span><strong>{backend} · {ctx === 0 ? 'automatic' : ctx}</strong></div>
                <div><span className="setup-label">Policy</span><strong>{policy}</strong></div>
              </div>
              <pre className="setup-log">{JSON.stringify({
                engine_root: engineRoot, suite_root: `${root.replace(/[\\/]+$/, '')}/suite`, models_root: modelsRoot,
                cache_root: cacheRoot, python: pythonStatus?.command || python || '(PATH)', model: model || '',
                backend, ctx, policy,
              }, null, 2)}</pre>
              <p className="muted tiny">No API keys are stored here; secrets stay in the environment.</p>
            </>
          )}
        </div>

        <div className="setup-foot">
          <button className="btn ghost" onClick={back} disabled={index === 0 || Boolean(busy)}>Back</button>
          <div className="spacer" />
          {step !== 'review' && step !== 'welcome' && step !== 'models' && step !== 'python' && (
            <button className="link-btn" onClick={() => {
              const remaining = ['python', 'deps', 'engine'].includes(step);
              if (!remaining) go('review');
            }}>Skip details</button>
          )}
          {step === 'welcome' && !firstRun && <button className="btn ghost" onClick={onClose}>Cancel</button>}
          {step === 'welcome' && (
            <button className="btn primary" onClick={() => go('prereq')}>Start</button>
          )}
          {step === 'review' ? (
            <button className="btn primary" disabled={busy === 'save'} onClick={() => void saveConfig()}>
              {busy === 'save' ? 'Saving…' : 'Save and restart'}
            </button>
          ) : step !== 'welcome' && step !== 'models' && (
            <button className="btn primary" onClick={() => void next()}>Next</button>
          )}
          {step === 'models' && (
            <button className="btn primary" onClick={() => go('review')}>Continue</button>
          )}
          {step === 'python' && (
            <button className="btn" onClick={() => go('review')}>Skip</button>
          )}
        </div>
      </div>
    </div>
  );
}
