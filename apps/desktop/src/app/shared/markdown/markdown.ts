import { Pipe, type PipeTransform } from '@angular/core';
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import go from 'highlight.js/lib/languages/go';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import markdown from 'highlight.js/lib/languages/markdown';
import powershell from 'highlight.js/lib/languages/powershell';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import sql from 'highlight.js/lib/languages/sql';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';
import { Marked, type RendererObject, type Tokens } from 'marked';

const LANGUAGES = {
  bash,
  c,
  cpp,
  csharp,
  css,
  go,
  java,
  javascript,
  json,
  markdown,
  powershell,
  python,
  rust,
  sql,
  typescript,
  xml,
  yaml,
};
for (const [name, language] of Object.entries(LANGUAGES)) {
  hljs.registerLanguage(name, language);
}

export function escapeHtml(text: string): string {
  return text
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

/**
 * Model output is untrusted. The renderer therefore:
 * - shows raw HTML as text instead of interpreting it;
 * - renders links as inert text with the target in a tooltip (no navigation
 *   of the app window, no requests to unknown hosts);
 * - renders images as their alt text (no tracking pixels, CSP blocks them anyway).
 * Angular sanitizes the resulting HTML once more when it is bound.
 */
const renderer: RendererObject = {
  html({ text }: Tokens.HTML | Tokens.Tag) {
    return escapeHtml(text);
  },
  link({ href, tokens }: Tokens.Link) {
    const label = this.parser.parseInline(tokens);
    return `<span class="md-link" title="${escapeHtml(href)}">${label}</span>`;
  },
  image({ text, href }: Tokens.Image) {
    return `<span class="md-image" title="${escapeHtml(href)}">[${escapeHtml(text || 'image')}]</span>`;
  },
  code({ text, lang }: Tokens.Code) {
    const language = (lang ?? '').trim().split(/\s+/)[0]?.toLowerCase() ?? '';
    const highlighted = language && hljs.getLanguage(language);
    const body = highlighted
      ? hljs.highlight(text, { language, ignoreIllegals: true }).value
      : escapeHtml(text);
    const label = language ? `<span class="md-code-lang">${escapeHtml(language)}</span>` : '';
    return `<pre class="md-code">${label}<code class="hljs">${body}</code></pre>`;
  },
};

const parser = new Marked({ gfm: true, breaks: true, async: false, renderer });

/** `<think>` sections emitted by reasoning models (closed or still streaming). */
const THINK_SECTION = /<think>([\s\S]*?)(?:<\/think>|$)/g;

/** Renders untrusted Markdown to HTML. Reasoning sections become collapsible. */
export function renderMarkdown(source: string): string {
  let html = '';
  let cursor = 0;
  for (const match of source.matchAll(THINK_SECTION)) {
    html += parser.parse(source.slice(cursor, match.index)) as string;
    const reasoning = (match[1] ?? '').trim();
    if (reasoning) {
      html += `<details class="md-think"><summary>Reasoning</summary>${parser.parse(reasoning) as string}</details>`;
    }
    cursor = match.index + match[0].length;
  }
  html += parser.parse(source.slice(cursor)) as string;
  return html;
}

@Pipe({ name: 'markdown' })
export class MarkdownPipe implements PipeTransform {
  transform(value: string): string {
    return renderMarkdown(value);
  }
}
