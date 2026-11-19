import assert from 'node:assert/strict';
import { gzipSync } from 'node:zlib';
import { build } from 'vite';

// Inspect the actual production chunks: a dynamic import in the source is not
// enough if another component still imports the same library statically.
const { output } = await build();
const chunks = new Map(output.filter((item) => item.type === 'chunk').map((chunk) => [chunk.fileName, chunk]));

function eagerChunks(entry) {
  assert.ok(entry, 'Missing build entry');
  const seen = new Set();
  function visit(chunk) {
    if (seen.has(chunk)) return;
    seen.add(chunk);
    for (const file of chunk.imports) visit(chunks.get(file));
  }
  visit(entry);
  return [...seen];
}

function excludes(eager, packages) {
  for (const name of packages) {
    const path = `/node_modules/${name}/`;
    assert.ok(!eager.some((chunk) => chunk.moduleIds.some((id) => id.includes(path))), `${eager[0].name} eagerly loads ${name}`);
  }
}

const all = [...chunks.values()];
const startup = eagerChunks(all.find((chunk) => chunk.isEntry && chunk.facadeModuleId?.endsWith('/index.html')));
console.log('Startup JS (bytes):', startup.reduce((sum, c) => sum + Buffer.byteLength(c.code), 0),
  'gzip:', startup.reduce((sum, c) => sum + gzipSync(c.code).length, 0));
excludes(startup, ['echarts', 'jspdf', 'svg2pdf.js']);
excludes(eagerChunks(all.find((chunk) => chunk.moduleIds.some((id) => id.endsWith('/ChartView.svelte')))), ['jspdf', 'svg2pdf.js']);
excludes(eagerChunks(all.find((chunk) => chunk.moduleIds.some((id) => id.endsWith('/lib/report.ts')))), ['echarts', 'svg2pdf.js']);
