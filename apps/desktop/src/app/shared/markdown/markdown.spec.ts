import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
  it('renders common Markdown', () => {
    const html = renderMarkdown('# Title\n\n**bold** and `code`\n\n- a\n- b');
    expect(html).toContain('<h1>Title</h1>');
    expect(html).toContain('<strong>bold</strong>');
    expect(html).toContain('<code>code</code>');
    expect(html).toContain('<li>a</li>');
  });

  it('highlights fenced code with a known language', () => {
    const html = renderMarkdown('```rust\nfn main() {}\n```');
    expect(html).toContain('class="md-code"');
    expect(html).toContain('<span class="md-code-lang">rust</span>');
    expect(html).toContain('hljs-keyword');
  });

  it('escapes code in unknown languages', () => {
    const html = renderMarkdown('```brainfuck\n<b>++</b>\n```');
    expect(html).toContain('&lt;b&gt;++&lt;/b&gt;');
  });

  it('shows raw HTML as text', () => {
    const html = renderMarkdown('Hello <img src=x onerror=alert(1)> <script>alert(1)</script>');
    expect(html).not.toContain('<img');
    expect(html).not.toContain('<script');
    expect(html).toContain('&lt;script&gt;');
  });

  it('renders links and images inertly', () => {
    const html = renderMarkdown('[site](https://example.com) ![pixel](https://t.example/p.gif)');
    expect(html).not.toContain('<a ');
    expect(html).not.toContain('<img');
    expect(html).toContain('<span class="md-link" title="https://example.com">site</span>');
    expect(html).toContain('[pixel]');
  });

  it('turns reasoning sections into collapsible blocks, also while streaming', () => {
    const done = renderMarkdown('<think>step 1</think>Answer');
    expect(done).toContain('<details class="md-think"><summary>Reasoning</summary>');
    expect(done).toContain('<p>Answer</p>');

    const streaming = renderMarkdown('<think>still thinking');
    expect(streaming).toContain('still thinking');
    expect(streaming).not.toContain('&lt;think&gt;');
  });
});
