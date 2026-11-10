import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import Markdown from './components/Markdown';
import ToolCard from './components/ToolCard';
import FileTree from './components/FileTree';
import EnginePanel from './components/EnginePanel';
import type { AgentEvent, BackendState, ChatMessage, ChatPayloadMessage, ModelInfo, Settings, Todo, TreeNode, UpdateInfo } from './types';

const uid = () => Math.random().toString(36).slice(2, 10);

const EMPTY_BACKEND: BackendState = {
  runtime: null, python: '', port: 8184, model: null, state: 'stopped',
  lastError: null, plan: null, uptimeMs: 0, logPath: null,
};

function greeting(): string {
  return [
    'Ready when you are.',
    '',
    'Open a folder, pick a Fennec model and start the engine. The model never loads whole —',
    'Fennec keeps a hot expert tier in VRAM, a warm tier in RAM, and pages the rest from SSD',
    'as the router asks for it.',
  ].join('\n');
}

export default function App() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [backend, setBackend] = useState<BackendState>(EMPTY_BACKEND);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [messages, setMessages] = useState<ChatMessage[]>([
    { id: uid(), role: 'assistant', content: greeting(), tools: [] },
  ]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [todos, setTodos] = useState<Todo[]>([]);
  const [tab, setTab] = useState<'engine' | 'files'>('engine');
  const [approval, setApproval] = useState<{ id: string; name: string; args: Record<string, unknown> } | null>(null);
  const [tokensPerSecond, setTokensPerSecond] = useState<number | null>(null);
  const [contextSaved, setContextSaved] = useState<{ tokens: number; seconds: number; before: number; after: number } | null>(null);
  const [attached, setAttached] = useState<{ path: string; content: string }[]>([]);
  const [image, setImage] = useState<{ name: string; dataUrl: string } | null>(null);
  const [recording, setRecording] = useState(false);
  const [sttBusy, setSttBusy] = useState(false);
  const [audioNotice, setAudioNotice] = useState<string | null>(null);
  const [speakingId, setSpeakingId] = useState<string | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [updateLog, setUpdateLog] = useState('');
  const streamId = useRef<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const filePicker = useRef<HTMLInputElement>(null);
  const imagePicker = useRef<HTMLInputElement>(null);
  const recorder = useRef<MediaRecorder | null>(null);
  const chunks = useRef<Blob[]>([]);

  useEffect(() => {
    (async () => {
      const info = await window.fennec.info();
      setSettings(info.settings);
      setBackend(info.backend);
      void refreshModels();
      if (info.settings.workspace) void loadTree();
    })();

    window.fennec.agent.onEvent((event) => handleEvent(event));
    window.fennec.agent.onApproval((req) => setApproval(req));
    // The main process checks the release feed at startup and on use.
    window.fennec.updates.onInfo((info) => setUpdateInfo(info));
  }, []);

  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight, behavior: 'smooth' });
  }, [messages]);

  const refreshModels = useCallback(async () => {
    const list = await window.fennec.listModels();
    setModels(list);
  }, []);

  const loadTree = useCallback(async () => {
    setTree(await window.fennec.files.tree());
  }, []);

  const patch = useCallback(async (next: Partial<Settings>) => {
    const merged = await window.fennec.saveSettings(next);
    setSettings(merged);
    return merged;
  }, []);

  const handleEvent = useCallback((event: AgentEvent) => {
    if (event.type === 'usage') {
      setTokensPerSecond(event.tokensPerSecond);
      if (event.contextCharsBefore) {
        setContextSaved({
          tokens: event.contextSavedTokens ?? 0,
          seconds: event.contextSavedSeconds ?? 0,
          before: event.contextCharsBefore,
          after: event.contextCharsAfter ?? event.contextCharsBefore,
        });
      }
      return;
    }
    if (event.type === 'todos') {
      setTodos(event.todos);
      return;
    }
    if (event.type === 'error') {
      setMessages((prev) => {
        const id = streamId.current;
        if (!id) return prev;
        return prev.map((m) => (m.id === id ? { ...m, error: event.message, streaming: false } : m));
      });
      setBusy(false);
      return;
    }
    if (event.type === 'done') {
      setMessages((prev) => prev.map((m) => (m.id === streamId.current ? { ...m, streaming: false, ms: event.ms } : m)));
      streamId.current = null;
      setBusy(false);
      void loadTree();
      return;
    }

    setMessages((prev) => {
      const id = streamId.current;
      if (!id) return prev;
      return prev.map((msg) => {
        if (msg.id !== id) return msg;
        if (event.type === 'assistant') return { ...msg, content: msg.content + event.text };
        if (event.type === 'reasoning') return { ...msg, reasoning: (msg.reasoning || '') + event.text };
        if (event.type === 'step') return msg;
        if (event.type === 'tool') {
          const calls = [...msg.tools];
          const at = calls.findIndex((c) => c.id === event.id);
          if (event.phase === 'start') {
            calls.push({ id: event.id, name: event.name, args: event.args || {}, phase: 'running' });
          } else if (at >= 0) {
            calls[at] = {
              ...calls[at],
              phase: event.phase === 'error' ? 'error' : 'done',
              output: event.output ?? calls[at].output,
              message: event.message ?? calls[at].message,
            };
          }
          return { ...msg, tools: calls };
        }
        return msg;
      });
    });
  }, [loadTree]);

  const openFolder = async () => {
    const res = await window.fennec.pickWorkspace();
    if (!res) return;
    setSettings((s) => (s ? { ...s, workspace: res.workspace, recentWorkspaces: res.recentWorkspaces } : s));
    await loadTree();
  };

  const startEngine = async (modelDir: string) => {
    if (!settings) return;
    setBackend({ ...EMPTY_BACKEND, state: 'starting', runtime: backend.runtime, python: backend.python, port: settings.port });
    const res = await window.fennec.engine.start({ modelDir, port: settings.port, kvSlots: settings.kvSlots });
    setBackend(res.backend);
    if (res.settings) setSettings(res.settings);
    if (!res.ok) {
      setMessages((prev) => [
        ...prev,
        { id: uid(), role: 'assistant', content: `The engine did not start.\n\n\`\`\`\n${res.error}\n\`\`\``, tools: [], error: res.error },
      ]);
    }
  };

  const stopEngine = async () => setBackend(await window.fennec.engine.stop());

  const attachFile = async (rel: string) => {
    const file = await window.fennec.files.read(rel);
    const clipped = file.content.length > 24000 ? `${file.content.slice(0, 24000)}\n… truncated` : file.content;
    setAttached((prev) => (prev.some((f) => f.path === rel) ? prev : [...prev, { path: rel, content: clipped }]));
  };

  const attachTextFromDisk = async (file: File) => {
    if (file.size > 1_000_000) {
      setAudioNotice('Text attachments must be at most 1 MB.');
      return;
    }
    const text = await file.text();
    if (text.includes('\0')) {
      setAudioNotice('That looks like a binary file; attach text or an image.');
      return;
    }
    const clipped = text.length > 24000 ? `${text.slice(0, 24000)}\n… truncated` : text;
    setAttached((prev) => [...prev.filter((f) => f.path !== file.name), { path: file.name, content: clipped }]);
  };

  const attachImage = async (file: File) => {
    if (!/^image\/(png|jpe?g|webp|gif)$/i.test(file.type)) {
      setAudioNotice('Use a PNG, JPEG, WebP or GIF image.');
      return;
    }
    if (file.size > 6_000_000) {
      setAudioNotice('Images must be at most 6 MB.');
      return;
    }
    const dataUrl = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(reader.error);
      reader.readAsDataURL(file);
    });
    setAudioNotice(null);
    setImage({ name: file.name, dataUrl });
  };

  const toggleMic = async () => {
    setAudioNotice(null);
    if (recording) {
      recorder.current?.stop();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const mime = ['audio/webm;codecs=opus', 'audio/webm', 'audio/ogg']
        .find((type) => MediaRecorder.isTypeSupported(type)) || '';
      const rec = new MediaRecorder(stream, mime ? { mimeType: mime } : undefined);
      chunks.current = [];
      rec.ondataavailable = (event) => {
        if (event.data.size) chunks.current.push(event.data);
      };
      rec.onstop = async () => {
        stream.getTracks().forEach((track) => track.stop());
        setRecording(false);
        const blob = new Blob(chunks.current, { type: rec.mimeType || 'audio/webm' });
        if (!blob.size) return;
        setSttBusy(true);
        try {
          const result = await window.fennec.stt.transcribe(await blob.arrayBuffer(), blob.type);
          if (!result.ok) throw new Error(result.error || 'transcription failed');
          if (result.text) setInput((prev) => (prev ? `${prev} ` : '') + result.text);
        } catch (err) {
          setAudioNotice(`Speech recognition failed: ${(err as Error).message}`);
        } finally {
          setSttBusy(false);
        }
      };
      rec.start();
      recorder.current = rec;
      setRecording(true);
    } catch (err) {
      setAudioNotice(`Microphone unavailable: ${(err as Error).message}`);
    }
  };

  const speak = (message: ChatMessage) => {
    if (!('speechSynthesis' in window)) {
      setAudioNotice('This system has no speech synthesis.');
      return;
    }
    window.speechSynthesis.cancel();
    if (speakingId === message.id) {
      setSpeakingId(null);
      return;
    }
    const text = message.content.replace(/```[\s\S]*?```/g, ' code block ').slice(0, 20000);
    if (!text.trim()) return;
    const utterance = new SpeechSynthesisUtterance(text);
    utterance.rate = 1.03;
    utterance.lang = 'en-US';
    utterance.onend = () => setSpeakingId(null);
    utterance.onerror = () => setSpeakingId(null);
    setSpeakingId(message.id);
    window.speechSynthesis.speak(utterance);
  };

  const checkUpdates = async () => {
    setUpdateBusy(true);
    setUpdateInfo(await window.fennec.updates.check());
    setUpdateBusy(false);
  };

  const applyUpdates = async () => {
    setUpdateBusy(true);
    setUpdateLog('Pulling the suite checkouts…');
    const result = await window.fennec.updates.apply();
    const lines = result.results.map((row) => `${row.ok ? '✓' : '✗'} ${row.repo}\n${row.output}`).join('\n\n');
    const credits = (result.upstream || []).map((entry) => `${entry.name} — ${entry.role}: ${entry.url}`).join('\n');
    setUpdateLog(`${lines}${result.error ? `\n\n${result.error}` : ''}${credits ? `\n\nUpstream (not pulled):\n${credits}` : ''}`);
    setUpdateBusy(false);
  };

  const send = async () => {
    const text = input.trim();
    if (!text || busy || !settings) return;
    const userMessage: ChatMessage = {
      id: uid(), role: 'user', content: text, tools: [], image: image || undefined,
    };
    const reply: ChatMessage = { id: uid(), role: 'assistant', content: '', tools: [], streaming: true };
    const history = [...messages.filter((m) => m.role !== 'assistant' || m.content || m.tools.length), userMessage];
    setMessages((prev) => [...prev, userMessage, reply]);
    setInput('');
    setBusy(true);
    streamId.current = reply.id;

    const extraContext = attached.map((f) => `--- ${f.path} ---\n${f.content}`).join('\n\n');
    setAttached([]);

    const payload: ChatPayloadMessage[] = history.map((m) => {
      if (m.id === userMessage.id && image) {
        return {
          role: m.role,
          content: [
            { type: 'text' as const, text: m.content },
            { type: 'image_url' as const, image_url: { url: image.dataUrl } },
          ],
        };
      }
      return { role: m.role, content: m.content || '(no text)' };
    });
    setImage(null);
    setContextSaved(null);
    const res = await window.fennec.agent.send({ messages: payload, extraContext });
    if (!res.ok) {
      setBusy(false);
      setMessages((prev) => prev.map((m) => (m.id === reply.id ? { ...m, streaming: false, error: res.error || (res.cancelled ? 'stopped' : 'failed') } : m)));
      streamId.current = null;
    }
  };

  const stop = async () => {
    await window.fennec.agent.stop();
    setBusy(false);
  };

  const answerApproval = async (approved: boolean) => {
    if (!approval) return;
    await window.fennec.agent.approve(approval.id, approved);
    setApproval(null);
  };

  const status = useMemo(() => {
    if (backend.state === 'ready') return { tone: 'ok', label: 'engine ready' };
    if (backend.state === 'starting') return { tone: 'warn', label: 'loading…' };
    if (backend.state === 'failed') return { tone: 'bad', label: 'engine failed' };
    return { tone: 'idle', label: 'engine stopped' };
  }, [backend.state]);

  if (!settings) return <div className="boot">starting Fennec Desktop…</div>;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo">◧</span>
          <div>
            <strong>Fennec Desktop</strong>
            <span className="tiny muted">local coding agent</span>
          </div>
        </div>

        <button className="btn ghost" onClick={openFolder}>
          {settings.workspace ? settings.workspace.split(/[\\/]/).pop() : 'Open folder'}
        </button>

        <div className="spacer" />

        {updateInfo?.available && (
          <button
            className="btn ghost"
            title={`${updateInfo.notes || 'Release notes on GitHub'}`}
            onClick={() => void window.fennec.updates.open(updateInfo.url || 'https://github.com/Liso7150/fennec-desktop/releases')}
          >
            {updateInfo.latest} available
          </button>
        )}

        <span className={`pill pill-${status.tone}`}>
          <span className="dot" />
          {status.label}
        </span>
        {tokensPerSecond !== null && <span className="tiny muted">{tokensPerSecond.toFixed(1)} tok/s</span>}
        {contextSaved && contextSaved.tokens > 0 && (
          <span
            className="tiny muted"
            title={
              'Context compaction: this turn sent ' +
              contextSaved.before.toLocaleString() +
              ' characters instead of ' +
              contextSaved.after.toLocaleString() +
              ', so ' +
              contextSaved.tokens.toLocaleString() +
              ' tokens were not re-sent. Seconds are estimated from the engine\'s own prefill rate on this turn.'
            }
          >
            ctx −{Math.max(0, Math.round((1 - contextSaved.after / contextSaved.before) * 100))}% · −
            {contextSaved.tokens >= 1000 ? `${(contextSaved.tokens / 1000).toFixed(1)}k` : contextSaved.tokens} tok
            {contextSaved.seconds >= 1 ? ` · ≈${Math.round(contextSaved.seconds)} s` : ''}
          </span>
        )}
        {backend.state === 'ready' ? (
          <button className="btn ghost" onClick={stopEngine}>Stop</button>
        ) : (
          <button
            className="btn primary"
            disabled={!settings.modelDir}
            onClick={() => startEngine(settings.modelDir)}
          >
            Start engine
          </button>
        )}
      </header>

      <div className="main">
        <aside className="sidebar">
          <div className="side-head">
            <span>Explorer</span>
            <button className="icon-btn" onClick={openFolder} title="Open folder">＋</button>
          </div>
          <FileTree nodes={tree} onOpen={attachFile} />
          {!!attached.length && (
            <div className="attached">
              <div className="side-head"><span>Context</span></div>
              {attached.map((f) => (
                <div className="attached-item" key={f.path}>
                  <code>{f.path}</code>
                  <button className="icon-btn" onClick={() => setAttached((prev) => prev.filter((x) => x.path !== f.path))}>✕</button>
                </div>
              ))}
            </div>
          )}
          {!!todos.length && (
            <div className="todos">
              <div className="side-head"><span>Plan</span></div>
              {todos.map((t, i) => (
                <div className={`todo todo-${t.status}`} key={i}>
                  <span className="todo-mark">{t.status === 'done' ? '✓' : t.status === 'in_progress' ? '◐' : '○'}</span>
                  <span>{t.task}</span>
                </div>
              ))}
            </div>
          )}
        </aside>

        <section className="chat">
          <div className="transcript" ref={scroller}>
            {messages.map((message) => (
              <article className={`msg msg-${message.role}`} key={message.id}>
                <div className="avatar">{message.role === 'user' ? 'you' : '◧'}</div>
                <div className="msg-body">
                  {message.reasoning && (
                    <details className="reasoning">
                      <summary>reasoning</summary>
                      <pre>{message.reasoning}</pre>
                    </details>
                  )}
                  {message.tools.map((call) => (
                    <ToolCard key={call.id} call={call} />
                  ))}
                  {message.image && (
                    <img className="msg-image" src={message.image.dataUrl} alt={message.image.name} />
                  )}
                  {message.content && <Markdown text={message.content} />}
                  {message.streaming && !message.content && !message.tools.length && (
                    <div className="thinking"><span /><span /><span /></div>
                  )}
                  {message.error && <div className="error-box">{message.error}</div>}
                  {(message.ms || (message.role === 'assistant' && message.content)) && (
                    <div className="msg-actions">
                      {message.role === 'assistant' && message.content && (
                        <button className="link-btn" onClick={() => speak(message)}>
                          {speakingId === message.id ? 'Stop reading' : 'Read aloud'}
                        </button>
                      )}
                      {message.ms && <span className="tiny muted">{(message.ms / 1000).toFixed(1)}s</span>}
                    </div>
                  )}
                </div>
              </article>
            ))}
          </div>

          <div className="composer">
            <input
              ref={filePicker}
              type="file"
              accept=".txt,.md,.json,.js,.jsx,.ts,.tsx,.css,.html,.py,.rs,.go,.java,.c,.h,.cpp,.yaml,.yml,.toml,.sh,.sql,.csv"
              hidden
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = '';
                if (file) void attachTextFromDisk(file);
              }}
            />
            <input
              ref={imagePicker}
              type="file"
              accept="image/png,image/jpeg,image/webp,image/gif"
              hidden
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = '';
                if (file) void attachImage(file);
              }}
            />
            {image && (
              <div className="image-chip">
                <img src={image.dataUrl} alt={image.name} />
                <span>{image.name}</span>
                <button className="icon-btn" title="Remove image" onClick={() => setImage(null)}>✕</button>
              </div>
            )}
            {audioNotice && (
              <div className="notice">
                {audioNotice}
                <button className="icon-btn" onClick={() => setAudioNotice(null)}>✕</button>
              </div>
            )}
            <textarea
              value={input}
              placeholder={
                backend.state === 'ready'
                  ? 'Ask for a change, or paste an error…  (Enter to send, Shift+Enter for a new line)'
                  : 'Ask for a change — the local engine starts automatically on the first message…'
              }
              onChange={(e) => setInput(e.target.value)}
              onPaste={(e) => {
                const file = Array.from(e.clipboardData.files).find((f) => f.type.startsWith('image/'));
                if (file) {
                  e.preventDefault();
                  void attachImage(file);
                }
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
              rows={Math.min(10, Math.max(2, input.split('\n').length))}
            />
            <div className="composer-actions">
              <div className="composer-tools">
                <button className="btn ghost" title="Attach a text file" disabled={busy} onClick={() => filePicker.current?.click()}>
                  Attach
                </button>
                <button className="btn ghost" title="Attach one image (vision)" disabled={busy} onClick={() => imagePicker.current?.click()}>
                  Image
                </button>
                <button
                  className={`btn ghost${recording ? ' recording' : ''}`}
                  title="Dictate with the local microphone"
                  disabled={busy || sttBusy}
                  onClick={() => void toggleMic()}
                >
                  {recording ? 'Stop recording' : sttBusy ? 'Transcribing…' : 'Mic'}
                </button>
                <span className="tiny muted">
                  {settings.workspace ? settings.workspace : 'no folder open'}
                  {attached.length ? ` · ${attached.length} file(s) attached` : ''}
                </span>
              </div>
              {busy ? (
                <button className="btn danger" onClick={stop}>Stop</button>
              ) : (
                <button className="btn primary" onClick={send} disabled={!input.trim()}>Send</button>
              )}
            </div>
          </div>
        </section>

        <aside className="rightpanel">
          <div className="tabs">
            <button className={tab === 'engine' ? 'active' : ''} onClick={() => setTab('engine')}>Engine</button>
            <button className={tab === 'files' ? 'active' : ''} onClick={() => setTab('files')}>Files</button>
          </div>
          {tab === 'engine' ? (
            <EnginePanel
              backend={backend}
              models={models}
              settings={settings}
              onRefreshModels={async () => {
                await refreshModels();
              }}
              onStart={startEngine}
              onStop={stopEngine}
              onSettings={patch}
            />
          ) : (
            <div className="panel-body">
              <section className="card">
                <div className="card-head"><h3>Recent folders</h3></div>
                {settings.recentWorkspaces?.length ? (
                  settings.recentWorkspaces.map((w) => (
                    <button
                      key={w}
                      className="recent"
                      onClick={async () => {
                        await window.fennec.useWorkspace(w);
                        setSettings((s) => (s ? { ...s, workspace: w } : s));
                        await loadTree();
                      }}
                    >
                      {w}
                    </button>
                  ))
                ) : (
                  <div className="muted">Nothing yet</div>
                )}
              </section>
              <section className="card">
                <div className="card-head">
                  <h3>Updates</h3>
                  <button className="btn ghost" disabled={updateBusy} onClick={() => void checkUpdates()}>
                    {updateBusy ? 'Working…' : 'Check release'}
                  </button>
                </div>
                {updateInfo && (
                  <div className="update-box">
                    {updateInfo.ok === false ? (
                      <div className="error-box">{updateInfo.error || 'Could not reach GitHub.'}</div>
                    ) : updateInfo.upToDate ? (
                      <div className="muted tiny">Version {updateInfo.current} is current.</div>
                    ) : (
                      <>
                        <div className="tiny">
                          <strong>{updateInfo.latest}</strong> is available — you have {updateInfo.current}.
                        </div>
                        {updateInfo.notes && <pre className="update-notes">{updateInfo.notes}</pre>}
                        <button
                          className="link-btn"
                          onClick={() => void window.fennec.updates.open(
                            updateInfo.url || 'https://github.com/Liso7150/fennec-desktop/releases',
                          )}
                        >
                          Open the release page
                        </button>
                      </>
                    )}
                  </div>
                )}
                <div className="row gap">
                  <button className="btn" disabled={updateBusy} onClick={() => void applyUpdates()}>
                    Update apps from GitHub
                  </button>
                </div>
                <p className="muted tiny">
                  Fast-forwards the local suite checkouts (desktop, core, CLI, setup hub and the web workspace)
                  with <code>git pull --ff-only</code>. Upstream stays credited and is never pulled: the engine
                  base idea is <code>JustVugg/colibri</code>, fine-tuning is <code>lyogavin/airllm</code>.
                </p>
                {updateLog && <pre className="update-log">{updateLog}</pre>}
              </section>
              <section className="card">
                <div className="card-head"><h3>How it works</h3></div>
                <p className="muted tiny">
                  Every request goes to the Colibri engine on <code>127.0.0.1:{settings.port}</code>. It is an
                  OpenAI-compatible endpoint that starts on demand and stops when this app closes, so nothing
                  keeps running in the background. The same engine can serve the JetBrains plugin and other tools.
                </p>
              </section>
            </div>
          )}
        </aside>
      </div>

      {approval && (
        <div className="modal-backdrop">
          <div className="modal">
            <h3>Allow {approval.name.replace(/_/g, ' ')}?</h3>
            <pre className="modal-args">{JSON.stringify(approval.args, null, 2)}</pre>
            <div className="row gap end">
              <button className="btn ghost" onClick={() => answerApproval(false)}>Deny</button>
              <button className="btn primary" onClick={() => answerApproval(true)}>Allow</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
