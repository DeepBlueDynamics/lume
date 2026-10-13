'use strict';
// A small Markdown renderer for Ask answers: headings, paragraphs, bold, italic,
// inline code, code blocks, lists, tables, links and rules. Every character is
// HTML-escaped first and only these constructs become markup, so model output
// can never inject HTML or script.
(function (root) {
  function escapeHtml(text) {
    return String(text)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  // Inline markup on already-escaped text. Code spans are cut out first so their
  // contents stay literal.
  function inline(escaped) {
    const codes = [];
    let out = escaped.replace(/`([^`]+)`/g, (_, code) => {
      codes.push(`<code>${code}</code>`);
      return `\u0000${codes.length - 1}\u0000`;
    });
    out = out
      .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>')
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(/__([^_]+)__/g, '<strong>$1</strong>')
      .replace(/(^|[^*\w])\*([^*\s][^*]*)\*(?!\w)/g, '$1<em>$2</em>')
      .replace(/(^|[^_\w])_([^_\s][^_]*)_(?!\w)/g, '$1<em>$2</em>');
    return out.replace(/\u0000(\d+)\u0000/g, (_, i) => codes[Number(i)]);
  }

  function splitRow(line) {
    let row = line.trim();
    if (row.startsWith('|')) row = row.slice(1);
    if (row.endsWith('|')) row = row.slice(0, -1);
    return row.split('|').map(cell => cell.trim());
  }

  const isTableRule = line => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line);

  function render(markdown) {
    const lines = escapeHtml(String(markdown || '').replace(/\r\n?/g, '\n')).split('\n');
    const html = [];
    let i = 0;
    let paragraph = [];
    const flush = () => {
      if (paragraph.length) html.push(`<p>${inline(paragraph.join(' '))}</p>`);
      paragraph = [];
    };

    while (i < lines.length) {
      const line = lines[i];

      const fence = line.match(/^\s*```/);
      if (fence) {
        flush();
        const body = [];
        i++;
        while (i < lines.length && !/^\s*```/.test(lines[i])) body.push(lines[i++]);
        i++;
        html.push(`<pre><code>${body.join('\n')}</code></pre>`);
        continue;
      }

      if (!line.trim()) { flush(); i++; continue; }

      const heading = line.match(/^(#{1,4})\s+(.*)$/);
      if (heading) {
        flush();
        const level = Math.min(heading[1].length + 2, 6);
        html.push(`<h${level}>${inline(heading[2])}</h${level}>`);
        i++;
        continue;
      }

      if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { flush(); html.push('<hr>'); i++; continue; }

      if (line.includes('|') && i + 1 < lines.length && isTableRule(lines[i + 1])) {
        flush();
        const head = splitRow(line);
        i += 2;
        const rows = [];
        while (i < lines.length && lines[i].includes('|') && lines[i].trim()) rows.push(splitRow(lines[i++]));
        html.push('<div class="md-table"><table><thead><tr>' +
          head.map(c => `<th>${inline(c)}</th>`).join('') + '</tr></thead><tbody>' +
          rows.map(r => '<tr>' + head.map((_, k) => `<td>${inline(r[k] || '')}</td>`).join('') + '</tr>').join('') +
          '</tbody></table></div>');
        continue;
      }

      const bullet = /^\s*[-*+]\s+/;
      const numbered = /^\s*\d+[.)]\s+/;
      if (bullet.test(line) || numbered.test(line)) {
        flush();
        const ordered = numbered.test(line);
        const marker = ordered ? numbered : bullet;
        const items = [];
        while (i < lines.length && marker.test(lines[i])) {
          let item = lines[i++].replace(marker, '');
          // Indented continuation lines belong to the item.
          while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !bullet.test(lines[i]) && !numbered.test(lines[i])) {
            item += ' ' + lines[i++].trim();
          }
          items.push(`<li>${inline(item)}</li>`);
        }
        html.push(ordered ? `<ol>${items.join('')}</ol>` : `<ul>${items.join('')}</ul>`);
        continue;
      }

      paragraph.push(line.trim());
      i++;
    }
    flush();
    return html.join('\n');
  }

  const api = {render, escapeHtml};
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.LumeMarkdown = api;
})(typeof window !== 'undefined' ? window : globalThis);
