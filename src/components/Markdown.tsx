import { useMemo } from 'react';
import { marked } from 'marked';
import hljs from 'highlight.js';

marked.setOptions({ gfm: true, breaks: true });

function escapeHtml(s: string) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export default function Markdown({ text }: { text: string }) {
  const html = useMemo(() => {
    const renderer = new marked.Renderer();
    renderer.code = ({ text: code, lang }: { text: string; lang?: string }) => {
      const language = (lang || '').trim();
      let highlighted;
      try {
        highlighted = language && hljs.getLanguage(language)
          ? hljs.highlight(code, { language }).value
          : hljs.highlightAuto(code).value;
      } catch {
        highlighted = escapeHtml(code);
      }
      return `<div class="code"><div class="code-head"><span>${escapeHtml(language || 'code')}</span></div><pre><code class="hljs">${highlighted}</code></pre></div>`;
    };
    const raw = marked.parse(text || '', { renderer, async: false }) as string;
    // local model output is untrusted markup: drop anything executable
    return raw.replace(/<script[\s\S]*?<\/script>/gi, '').replace(/ on[a-z]+="[^"]*"/gi, '');
  }, [text]);

  return <div className="markdown" dangerouslySetInnerHTML={{ __html: html }} />;
}
