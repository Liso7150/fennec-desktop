import { useState } from 'react';
import type { ToolCall } from '../types';

const ICONS: Record<string, string> = {
  read_file: '▤',
  write_file: '✎',
  edit_file: '✎',
  list_directory: '▦',
  search_files: '⌕',
  run_command: '❯',
  todo_write: '☑',
};

function summarize(call: ToolCall): string {
  const args = call.args || {};
  if (typeof args.path === 'string') return args.path;
  if (typeof args.command === 'string') return args.command;
  if (typeof args.pattern === 'string') return `/${args.pattern}/`;
  if (Array.isArray(args.todos)) return `${(args.todos as unknown[]).length} items`;
  return '';
}

export default function ToolCard({ call }: { call: ToolCall }) {
  const [open, setOpen] = useState(false);
  const target = summarize(call);
  const state = call.phase === 'running' ? 'running' : call.phase === 'error' ? 'error' : call.phase === 'done' ? 'done' : 'pending';

  return (
    <div className={`tool tool-${state}`}>
      <button className="tool-head" onClick={() => setOpen((v) => !v)}>
        <span className="tool-icon">{ICONS[call.name] || '⚙'}</span>
        <span className="tool-name">{call.name.replace(/_/g, ' ')}</span>
        {target && <code className="tool-target">{target.length > 90 ? `${target.slice(0, 90)}…` : target}</code>}
        <span className="tool-badge">
          {state === 'running' ? <span className="spin" /> : state === 'error' ? 'failed' : state === 'done' ? 'done' : ''}
        </span>
        <span className="tool-chevron">{open ? '▾' : '▸'}</span>
      </button>
      {open && (
        <div className="tool-body">
          {call.message && <div className="tool-error">{call.message}</div>}
          {call.output && <pre className="tool-output">{call.output}</pre>}
          {!call.output && !call.message && <div className="muted">no output yet</div>}
        </div>
      )}
    </div>
  );
}
