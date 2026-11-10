import { useMemo } from 'react';
import { marked } from 'marked';
import hljs from 'highlight.js';
import DOMPurify from 'dompurify';

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
    // Sanitize the final HTML, including syntax-highlighter output. No active
    // content, images (tracking/local file reads), CSS, or relative/custom URLs.
    return DOMPurify.sanitize(raw, {
      ALLOWED_TAGS: ['p', 'br', 'hr', 'strong', 'em', 'del', 's', 'blockquote', 'ul', 'ol', 'li',
        'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'pre', 'code', 'div', 'span', 'a',
        'table', 'thead', 'tbody', 'tr', 'th', 'td'],
      ALLOWED_ATTR: ['class', 'href', 'title'],
      ALLOWED_URI_REGEXP: /^https?:\/\//i,
      ALLOW_DATA_ATTR: false,
    });
  }, [text]);

  return <div className="markdown" dangerouslySetInnerHTML={{ __html: html }} />;
}
