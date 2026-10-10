// Minimal, safe, dependency-free markdown for chat output.
// Handles: fenced code blocks, inline code, **bold**, *italic*, [links](url).
// Everything else is escaped and newlines are preserved.

export type Block =
  | { type: 'text'; html: string }
  | { type: 'code'; lang: string; code: string };

const ESCAPE_MAP: Record<string, string> = {
  '&': '&amp;',
  '<': '&lt;',
  '>': '&gt;',
  '"': '&quot;',
  "'": '&#39;',
};

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ESCAPE_MAP[c]!);
}

function inlineMarkdown(s: string): string {
  let x = escapeHtml(s);
  // inline code first (avoid matching markdown inside it)
  const codeSlots: string[] = [];
  x = x.replace(/`([^`\n]+)`/g, (_, code: string) => {
    const i = codeSlots.push(code) - 1;
    return `\u0000C${i}\u0000`;
  });
  // links
  x = x.replace(
    /\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,
    '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>',
  );
  // bold before italic
  x = x.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>');
  x = x.replace(/(^|[^*\w])\*([^*\n]+)\*(?!\*)/g, '$1<em>$2</em>');
  // restore inline code
  x = x.replace(/\u0000C(\d+)\u0000/g, (_, i: string) => `<code>${codeSlots[+i] ?? ''}</code>`);
  return x;
}

export function parseMarkdown(src: string): Block[] {
  const blocks: Block[] = [];
  const lines = src.split('\n');
  let inCode = false;
  let codeLang = '';
  let codeBuf: string[] = [];
  let textBuf: string[] = [];

  const flushText = () => {
    if (textBuf.length === 0) return;
    const html = inlineMarkdown(textBuf.join('\n')).replace(/\n/g, '<br>');
    blocks.push({ type: 'text', html });
    textBuf = [];
  };

  for (const line of lines) {
    const fence = line.match(/^```(\w*)\s*$/);
    if (fence && !inCode) {
      flushText();
      inCode = true;
      codeLang = fence[1] ?? '';
      codeBuf = [];
    } else if (fence && inCode) {
      blocks.push({ type: 'code', lang: codeLang, code: codeBuf.join('\n') });
      inCode = false;
      codeLang = '';
      codeBuf = [];
    } else if (inCode) {
      codeBuf.push(line);
    } else {
      textBuf.push(line);
    }
  }
  if (inCode) {
    flushText();
    blocks.push({ type: 'code', lang: codeLang, code: codeBuf.join('\n') });
  }
  flushText();
  return blocks;
}
