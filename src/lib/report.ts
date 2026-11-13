// One assistant reply as a PDF: its Markdown text and its charts, in order. Tool steps stay out.
import { marked, type Token, type Tokens } from 'marked';
import { jsPDF } from 'jspdf';
import { svg2pdf } from 'svg2pdf.js';
import { api } from './api';
import { chartSvg } from './charts';
import type { ChartPart, Part } from './chatModel';

const PAGE_MARGIN = 50;
const BODY = 10.5;
const INK = '#1c1b19';
const MUTED = '#5f5b55';
const LINE = '#e5e3df';
const SUNKEN = '#f7f6f3';

type Style = 'normal' | 'bold' | 'italic' | 'bolditalic';
type Run = { text: string; mono?: boolean; style: Style };

// The built-in PDF fonts cover Windows-1252 only. Common symbols get a look-alike; anything else becomes "?".
const CP1252_EXTRA = '€‚ƒ„…†‡ˆ‰Š‹ŒŽ‘’“”•–—˜™š›œžŸ';
const LOOK_ALIKE: Record<string, string> = { '−': '-', '≈': '~', '≤': '<=', '≥': '>=', '→': '->', '←': '<-', '×': 'x', '✓': 'v', '✔': 'v', '\u00a0': ' ', '\u200b': '' };
export function pdfText(s: string): string {
  let out = '';
  for (const ch of s) {
    const c = ch.codePointAt(0)!;
    if (c === 9 || c === 10 || (c >= 0x20 && c <= 0x7e) || (c >= 0xa0 && c <= 0xff) || CP1252_EXTRA.includes(ch)) out += ch;
    else out += LOOK_ALIKE[ch] ?? '?';
  }
  return out;
}

const decode = (s: string) => s.replace(/&(amp|lt|gt|quot|#39);/g, (_, e: string) => ({ amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" })[e]!);
const add = (s: Style, b: 'bold' | 'italic'): Style => (s === 'normal' ? b : s === b ? s : 'bolditalic');

function runs(tokens: Token[] | undefined, style: Style = 'normal'): Run[] {
  const out: Run[] = [];
  for (const t of tokens ?? []) {
    switch (t.type) {
      case 'strong':
        out.push(...runs(t.tokens, add(style, 'bold')));
        break;
      case 'em':
        out.push(...runs(t.tokens, add(style, 'italic')));
        break;
      case 'codespan':
        out.push({ text: decode(t.text), mono: true, style: 'normal' });
        break;
      case 'br':
        out.push({ text: '\n', style });
        break;
      case 'link':
      case 'del':
        out.push(...runs(t.tokens, style));
        break;
      case 'image':
        out.push({ text: t.text, style });
        break;
      default:
        if ('tokens' in t && t.tokens?.length) out.push(...runs(t.tokens, style));
        else if ('text' in t) out.push({ text: decode(String(t.text)), style });
    }
  }
  return out;
}

const plain = (tokens: Token[] | undefined) => runs(tokens).map((r) => r.text).join('');

class Writer {
  y = PAGE_MARGIN;
  readonly width: number;
  readonly bottom: number;
  constructor(readonly doc: jsPDF) {
    this.width = doc.internal.pageSize.getWidth() - 2 * PAGE_MARGIN;
    this.bottom = doc.internal.pageSize.getHeight() - PAGE_MARGIN;
  }

  room(h: number) {
    if (this.y + h <= this.bottom) return;
    this.doc.addPage();
    this.y = PAGE_MARGIN;
  }

  font(run: Pick<Run, 'mono' | 'style'>, size: number) {
    this.doc.setFont(run.mono ? 'courier' : 'helvetica', run.mono ? 'normal' : run.style);
    this.doc.setFontSize(run.mono ? size * 0.92 : size);
  }

  rich(rs: Run[], size: number, x = PAGE_MARGIN, width = this.width, color = INK) {
    const lh = size * 1.45;
    type Piece = Run & { w: number };
    const lines: Piece[][] = [[]];
    let used = 0;
    for (const r of rs) {
      for (const word of pdfText(r.text).split(/(\n|\s+)/)) {
        if (!word) continue;
        if (word === '\n') {
          lines.push([]);
          used = 0;
          continue;
        }
        const space = /^\s+$/.test(word);
        if (space && used === 0) continue;
        this.font(r, size);
        const w = this.doc.getTextWidth(space ? ' ' : word);
        if (!space && used + w > width && used > 0) {
          const line = lines[lines.length - 1];
          while (line.length && /^\s+$/.test(line[line.length - 1].text)) line.pop();
          lines.push([]);
          used = 0;
        }
        lines[lines.length - 1].push({ ...r, text: space ? ' ' : word, w });
        used += w;
      }
    }
    this.doc.setTextColor(color);
    for (const line of lines) {
      this.room(lh);
      let cx = x;
      for (const p of line) {
        this.font(p, size);
        this.doc.text(p.text, cx, this.y + size);
        cx += p.w;
      }
      this.y += lh;
    }
  }

  block(t: Token, x = PAGE_MARGIN, width = this.width) {
    const doc = this.doc;
    switch (t.type) {
      case 'heading': {
        const size = t.depth === 1 ? 17 : t.depth === 2 ? 13.5 : 11.5;
        this.y += t.depth === 1 ? 6 : 8;
        this.room(size * 3);
        this.rich(runs(t.tokens, 'bold'), size, x, width);
        this.y += 3;
        break;
      }
      case 'paragraph':
      case 'text':
        this.rich(runs('tokens' in t && t.tokens ? t.tokens : [{ type: 'text', raw: t.raw, text: t.text } as Token]), BODY, x, width);
        this.y += 6;
        break;
      case 'list': {
        const l = t as Tokens.List;
        l.items.forEach((item, i) => {
          const mark = l.ordered ? `${(Number(l.start) || 1) + i}.` : '•';
          this.room(BODY * 1.45);
          doc.setFont('helvetica', 'normal');
          doc.setFontSize(BODY);
          doc.setTextColor(MUTED);
          doc.text(mark, x + 4, this.y + BODY);
          const startY = this.y;
          for (const sub of item.tokens) {
            if (sub.type === 'list') this.block(sub, x + 16, width - 16);
            else if (sub.type === 'text' || sub.type === 'paragraph') this.rich(runs('tokens' in sub && sub.tokens ? sub.tokens : []), BODY, x + 16, width - 16);
            else this.block(sub, x + 16, width - 16);
          }
          if (this.y === startY) this.y += BODY * 1.45;
          this.y += 2;
        });
        this.y += 4;
        break;
      }
      case 'code': {
        doc.setFont('courier', 'normal');
        doc.setFontSize(8.5);
        const lines: string[] = pdfText(t.text)
          .split('\n')
          .flatMap((l: string) => doc.splitTextToSize(l || ' ', width - 16) as string[]);
        const lh = 8.5 * 1.4;
        for (let i = 0; i < lines.length; ) {
          this.room(lh + 12);
          const fit = Math.max(1, Math.min(lines.length - i, Math.floor((this.bottom - this.y - 12) / lh)));
          doc.setFillColor(SUNKEN);
          doc.setDrawColor(LINE);
          doc.roundedRect(x, this.y, width, fit * lh + 12, 3, 3, 'FD');
          doc.setFont('courier', 'normal');
          doc.setFontSize(8.5);
          doc.setTextColor(INK);
          lines.slice(i, i + fit).forEach((l, j) => doc.text(l, x + 8, this.y + 6 + 8.5 + j * lh));
          this.y += fit * lh + 12;
          i += fit;
        }
        this.y += 8;
        break;
      }
      case 'table':
        this.table(t as Tokens.Table, x, width);
        break;
      case 'blockquote': {
        const top = this.y;
        for (const sub of (t as Tokens.Blockquote).tokens) this.block(sub, x + 12, width - 12);
        doc.setDrawColor(LINE);
        doc.setLineWidth(2);
        if (this.y > top) doc.line(x + 2, top, x + 2, this.y - 6);
        doc.setLineWidth(0.5);
        break;
      }
      case 'hr':
        this.room(14);
        doc.setDrawColor(LINE);
        doc.setLineWidth(0.5);
        doc.line(x, this.y + 6, x + width, this.y + 6);
        this.y += 14;
        break;
      case 'space':
        break;
      default:
        if ('text' in t && t.text) this.rich([{ text: decode(String(t.text)), style: 'normal' }], BODY, x, width);
    }
  }

  table(t: Tokens.Table, x: number, width: number) {
    const doc = this.doc;
    const size = 9;
    const pad = 5;
    const lh = size * 1.35;
    const head = t.header.map((c) => pdfText(plain(c.tokens)));
    const body = t.rows.map((r) => r.map((c) => pdfText(plain(c.tokens))));
    const natural = head.map((h, i) => {
      doc.setFont('helvetica', 'bold');
      doc.setFontSize(size);
      let w = doc.getTextWidth(h);
      doc.setFont('helvetica', 'normal');
      for (const r of body) w = Math.max(w, doc.getTextWidth(r[i] ?? ''));
      // 1 pt of slack: splitTextToSize breaks a word that fills the width exactly.
      return Math.max(w + 2 * pad + 1, 36);
    });
    const total = natural.reduce((a, b) => a + b, 0);
    // Too wide: columns narrower than an equal share keep their width; the wide ones share the rest.
    const fair = width / natural.length;
    const narrow = natural.filter((w) => w <= fair).reduce((a, b) => a + b, 0);
    const widths = total > width ? natural.map((w) => (w <= fair ? w : (w / (total - narrow)) * (width - narrow))) : natural;
    const wrap = (cells: string[], bold: boolean) => {
      doc.setFont('helvetica', bold ? 'bold' : 'normal');
      doc.setFontSize(size);
      return cells.map((c, i) => doc.splitTextToSize(c, widths[i] - 2 * pad) as string[]);
    };
    const row = (cells: string[][], bold: boolean) => {
      const h = Math.max(...cells.map((c) => c.length)) * lh + 2 * pad - 2;
      if (bold) {
        doc.setFillColor(SUNKEN);
        doc.rect(x, this.y, widths.reduce((a, b) => a + b, 0), h, 'F');
      }
      doc.setFont('helvetica', bold ? 'bold' : 'normal');
      doc.setFontSize(size);
      doc.setTextColor(bold ? MUTED : INK);
      let cx = x;
      cells.forEach((lines, i) => {
        const right = t.align[i] === 'right';
        lines.forEach((l, j) => doc.text(l, right ? cx + widths[i] - pad : cx + pad, this.y + pad + size - 1 + j * lh, { align: right ? 'right' : 'left' }));
        cx += widths[i];
      });
      this.y += h;
      doc.setDrawColor(LINE);
      doc.setLineWidth(0.5);
      doc.line(x, this.y, x + widths.reduce((a, b) => a + b, 0), this.y);
    };
    const header = wrap(head, true);
    const headerH = Math.max(...header.map((c) => c.length)) * lh + 2 * pad;
    this.room(headerH + lh + 2 * pad);
    row(header, true);
    for (const r of body) {
      const cells = wrap(r, false);
      const h = Math.max(...cells.map((c) => c.length)) * lh + 2 * pad;
      if (this.y + h > this.bottom) {
        this.room(this.bottom);
        row(header, true);
      }
      row(cells, false);
    }
    this.y += 10;
  }

  async chart(svg: string, height: number) {
    this.room(height + 10);
    // svg2pdf reads styles of a node in the document.
    const host = document.createElement('div');
    host.style.cssText = 'position:fixed;left:-10000px;top:0;';
    host.innerHTML = svg;
    document.body.append(host);
    try {
      await svg2pdf(host.firstElementChild as Element, this.doc, { x: PAGE_MARGIN, y: this.y, width: this.width, height });
    } finally {
      host.remove();
    }
    this.y += height + 14;
  }
}

export interface ReportInput {
  title: string;
  meta: string;
  parts: Part[];
}

function printable(p: ChartPart): ChartPart {
  const text = (v: unknown) => (typeof v === 'string' ? pdfText(v) : v);
  return {
    ...p,
    spec: { ...p.spec, title: p.spec.title && pdfText(p.spec.title) },
    rows: p.rows.map((r) => r.map(text) as typeof r),
  };
}

export async function replyPdf({ title, meta, parts }: ReportInput): Promise<{ bytes: Uint8Array<ArrayBuffer>; title: string }> {
  const doc = new jsPDF({ unit: 'pt', format: 'a4' });
  const w = new Writer(doc);

  // The reply's first heading is the document title when it is a level-1 heading at the very start.
  const docs = parts.map((p) => (p.kind === 'text' ? marked.lexer(p.text, { gfm: true, breaks: true }) : null));
  const first = docs.find((d) => d)?.find((t) => t.type !== 'space');
  const own = first?.type === 'heading' && first.depth === 1 ? first : null;
  const heading = own ? plain(own.tokens) : title;

  doc.setProperties({ title: pdfText(heading), creator: 'sandb' });
  w.rich([{ text: heading, style: 'bold' }], 19);
  w.rich([{ text: meta, style: 'normal' }], 9, PAGE_MARGIN, w.width, MUTED);
  w.y += 4;
  doc.setDrawColor(LINE);
  doc.setLineWidth(0.5);
  doc.line(PAGE_MARGIN, w.y, PAGE_MARGIN + w.width, w.y);
  w.y += 14;

  for (const [i, p] of parts.entries()) {
    if (p.kind === 'text') {
      for (const t of docs[i]!) if (t !== own) w.block(t);
    } else if (p.kind === 'chart') {
      const height = w.width * 0.5;
      // Drawn larger than the page and scaled down, so the labels come out small and sharp.
      await w.chart(chartSvg(printable(p), 720, 360), height);
    }
  }

  const pages = doc.getNumberOfPages();
  for (let n = 1; n <= pages; n++) {
    doc.setPage(n);
    doc.setFont('helvetica', 'normal');
    doc.setFontSize(8);
    doc.setTextColor(MUTED);
    doc.text(`Page ${n} of ${pages}`, PAGE_MARGIN + w.width, doc.internal.pageSize.getHeight() - 24, { align: 'right' });
    doc.text('sandb', PAGE_MARGIN, doc.internal.pageSize.getHeight() - 24);
  }
  return { bytes: new Uint8Array(doc.output('arraybuffer')), title: heading };
}

export function pdfName(title: string): string {
  const slug = title
    .normalize('NFKD')
    .replace(/[^\w\s-]/g, '')
    .trim()
    .toLowerCase()
    .replace(/[\s_-]+/g, '-')
    .slice(0, 60)
    .replace(/-+$/, '');
  return `${slug || 'report'}.pdf`;
}

/**
 * Renders one reply and hands the PDF to the user: the desktop app writes it through the save dialog,
 * a browser downloads it. Returns the line to show when the file was saved; throws otherwise.
 */
export async function exportReply(parts: Part[], question: string, meta: string): Promise<string> {
  const title = question.length > 90 ? `${question.slice(0, 87)}...` : question || 'Report';
  const { bytes, title: heading } = await replyPdf({ title, meta, parts });
  const name = pdfName(heading);
  if (!api.capabilities.fileExport) {
    const url = URL.createObjectURL(new Blob([bytes], { type: 'application/pdf' }));
    const a = Object.assign(document.createElement('a'), { href: url, download: name });
    a.click();
    URL.revokeObjectURL(url);
    return '';
  }
  const path = await api.savePdf(name, bytes);
  return path ? `Saved to ${path}` : '';
}
