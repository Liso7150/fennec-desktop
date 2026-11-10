import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import Markdown from './components/Markdown';
import ToolCard from './components/ToolCard';
import FileTree from './components/FileTree';
import EnginePanel from './components/EnginePanel';
import type { AgentEvent, BackendState, ChatMessage, ModelInfo, Settings, Todo, TreeNode } from './types';

const uid = () => Math.random().toString(36).slice(2, 10);

const EMPTY_BACKEND: BackendState = {
  runtime: null, python: '', port: 8184, model: null, state: 'stopped',
  lastError: null, plan: null, uptimeMs: 0, logPath: null,
};

function greeting(): string {
  return [
    'Ready when you are.',
    '',
    'Open a folder, pick a Colibri model and start the engine. The model never loads whole —',
    'Colibri keeps a hot expert tier in VRAM, a warm tier in RAM, and pages the rest from SSD',
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
  const [attached, setAttached] = useState<{ path: string; content: string }[]>([]);
  const streamId = useRef<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    (async () => {
      const info = await window.colibri.info();
      setSettings(info.settings);
      setBackend(info.backend);
      void refreshModels();
      if (info.settings.workspace) void loadTree();
    })();

    window.colibri.agent.onEvent((event) => handleEvent(event));
    window.colibri.agent.onApproval((req) => setApproval(req));
  }, []);

  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight, behavior: 'smooth' });
  }, [messages]);

  const refreshModels = useCallback(async () => {
    const list = await window.colibri.listModels();
    setModels(list);
  }, []);

  const loadTree = useCallback(async () => {
    setTree(await window.colibri.files.tree());
  }, []);

  const patch = useCallback(async (next: Partial<Settings>) => {
    const merged = await window.colibri.saveSettings(next);
    setSettings(merged);
    return merged;
  }, []);

  const handleEvent = useCallback((event: AgentEvent) => {
    if (event.type === 'usage') {
      setTokensPerSecond(event.tokensPerSecond);
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
    const res = await window.colibri.pickWorkspace();
    if (!res) return;
    setSettings((s) => (s ? { ...s, workspace: res.workspace, recentWorkspaces: res.recentWorkspaces } : s));
    await loadTree();
  };

  const startEngine = async (modelDir: string) => {
    if (!settings) return;
    setBackend({ ...EMPTY_BACKEND, state: 'starting', runtime: backend.runtime, python: backend.python, port: settings.port });
    const res = await window.colibri.engine.start({ modelDir, port: settings.port, kvSlots: settings.kvSlots });
    setBackend(res.backend);
    if (res.settings) setSettings(res.settings);
    if (!res.ok) {
      setMessages((prev) => [
        ...prev,
        { id: uid(), role: 'assistant', content: `The engine did not start.\n\n\`\`\`\n${res.error}\n\`\`\``, tools: [], error: res.error },
      ]);
    }
  };

  const stopEngine = async () => setBackend(await window.colibri.engine.stop());

  const attachFile = async (rel: string) => {
    const file = await window.colibri.files.read(rel);
    const clipped = file.content.length > 24000 ? `${file.content.slice(0, 24000)}\n… truncated` : file.content;
    setAttached((prev) => (prev.some((f) => f.path === rel) ? prev : [...prev, { path: rel, content: clipped }]));
  };

  const send = async () => {
    const text = input.trim();
    if (!text || busy || !settings) return;
    const userMessage: ChatMessage = { id: uid(), role: 'user', content: text, tools: [] };
    const reply: ChatMessage = { id: uid(), role: 'assistant', content: '', tools: [], streaming: true };
    const history = [...messages.filter((m) => m.role !== 'assistant' || m.content || m.tools.length), userMessage];
    setMessages((prev) => [...prev, userMessage, reply]);
    setInput('');
    setBusy(true);
    streamId.current = reply.id;

    const extraContext = attached.map((f) => `--- ${f.path} ---\n${f.content}`).join('\n\n');
    setAttached([]);

    const payload = history.map((m) => ({ role: m.role, content: m.content || '(no text)' }));
    const res = await window.colibri.agent.send({ messages: payload, extraContext });
    if (!res.ok) {
      setBusy(false);
      setMessages((prev) => prev.map((m) => (m.id === reply.id ? { ...m, streaming: false, error: res.error || (res.cancelled ? 'stopped' : 'failed') } : m)));
      streamId.current = null;
    }
  };

  const stop = async () => {
    await window.colibri.agent.stop();
    setBusy(false);
  };

  const answerApproval = async (approved: boolean) => {
    if (!approval) return;
    await window.colibri.agent.approve(approval.id, approved);
    setApproval(null);
  };

  const status = useMemo(() => {
    if (backend.state === 'ready') return { tone: 'ok', label: 'engine ready' };
    if (backend.state === 'starting') return { tone: 'warn', label: 'loading…' };
    if (backend.state === 'failed') return { tone: 'bad', label: 'engine failed' };
    return { tone: 'idle', label: 'engine stopped' };
  }, [backend.state]);

  if (!settings) return <div className="boot">starting Colibri Code…</div>;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo">◧</span>
          <div>
            <strong>Colibri Code</strong>
            <span className="tiny muted">local coding agent</span>
          </div>
        </div>

        <button className="btn ghost" onClick={openFolder}>
          {settings.workspace ? settings.workspace.split(/[\\/]/).pop() : 'Open folder'}
        </button>

        <div className="spacer" />

        <span className={`pill pill-${status.tone}`}>
          <span className="dot" />
          {status.label}
        </span>
        {tokensPerSecond !== null && <span className="tiny muted">{tokensPerSecond.toFixed(1)} tok/s</span>}
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
                  {message.content && <Markdown text={message.content} />}
                  {message.streaming && !message.content && !message.tools.length && (
                    <div className="thinking"><span /><span /><span /></div>
                  )}
                  {message.error && <div className="error-box">{message.error}</div>}
                  {message.ms && <div className="tiny muted">{(message.ms / 1000).toFixed(1)}s</div>}
                </div>
              </article>
            ))}
          </div>

          <div className="composer">
            <textarea
              value={input}
              placeholder={
                backend.state === 'ready'
                  ? 'Ask for a change, or paste an error…  (Enter to send, Shift+Enter for a new line)'
                  : 'Ask for a change — the local engine starts automatically on the first message…'
              }
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
              rows={Math.min(10, Math.max(2, input.split('\n').length))}
            />
            <div className="composer-actions">
              <span className="tiny muted">
                {settings.workspace ? settings.workspace : 'no folder open'}
                {attached.length ? ` · ${attached.length} file(s) attached` : ''}
              </span>
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
                        await window.colibri.useWorkspace(w);
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
