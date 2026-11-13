<script lang="ts">
  import { untrack } from 'svelte';
  import { KeyRound, Link, Table2, LoaderCircle, RotateCcw, ZoomIn, ZoomOut, Maximize } from '@lucide/svelte';
  import type { ColumnInfo, ForeignKeyInfo, TableProperties } from '../api';
  import { catalog, loadColumns } from '../catalog.svelte';
  import type { Conn } from '../connections.svelte';
  import { displayName, type TableRef } from '../tableActions';
  import { tableKey } from '../tableId';

  /** `onopen`: double-click on a related table. */
  let { conn, table, columns, info, onopen }: { conn: Conn; table: TableRef; columns: ColumnInfo[]; info: TableProperties; onopen?: (t: TableRef) => void } = $props();

  const W = 240, HEAD = 30, ROW = 22, FOOT = 6, GX = 120, GY = 28, M = 24;

  type Node = { key: string; ref: TableRef; self: boolean };
  type Edge = { fk: ForeignKeyInfo; from: string; to: string };

  const keyOf = (schema: string | null | undefined, name: string) => `${schema ?? ''}.${name}`;
  const selfKey = untrack(() => keyOf(table.schema, table.name));
  const refOf = (schema: string | null | undefined, name: string): TableRef => ({ database: table.database, schema: schema ?? null, name, kind: 'table' });

  // Left lane: tables that point here. Right lane: tables this one points at (wins when both apply).
  const graph = $derived.by(() => {
    const right = new Map<string, Node>();
    const left = new Map<string, Node>();
    for (const f of info.foreignKeys) {
      const k = keyOf(f.refSchema, f.refTable);
      if (k !== selfKey && !right.has(k)) right.set(k, { key: k, ref: refOf(f.refSchema, f.refTable), self: false });
    }
    for (const f of info.references) {
      const k = keyOf(f.schema, f.table);
      if (k !== selfKey && !right.has(k) && !left.has(k)) left.set(k, { key: k, ref: refOf(f.schema, f.table), self: false });
    }
    const edges: Edge[] = [
      ...info.foreignKeys.map((fk) => ({ fk, from: selfKey, to: keyOf(fk.refSchema, fk.refTable) })),
      // A self reference is in both lists; draw it once.
      ...info.references.filter((fk) => keyOf(fk.schema, fk.table) !== selfKey).map((fk) => ({ fk, from: keyOf(fk.schema, fk.table), to: selfKey })),
    ];
    return { left: [...left.values()], right: [...right.values()], center: { key: selfKey, ref: table, self: true } as Node, edges };
  });

  // Columns of the related tables come from the catalog, which the properties view shares.
  const related = $derived([...graph.left, ...graph.right].map((n) => ({ n, load: catalog.columns[tableKey({ connId: conn.id, database: table.database, schema: n.ref.schema, name: n.ref.name })] })));
  const ready = $derived(related.every(({ load }) => !!load && load.status !== 'loading'));
  $effect(() => {
    for (const { n } of related) void loadColumns(conn.id, table.database, n.ref.schema, n.ref.name);
  });

  const colsOf = (k: string): ColumnInfo[] | string => {
    if (k === selfKey) return columns;
    const hit = related.find(({ n }) => n.key === k)?.load;
    return hit?.status === 'error' ? hit.error : (hit?.items ?? []);
  };
  const heightOf = (k: string) => {
    const c = colsOf(k);
    return HEAD + (typeof c === 'string' || !c.length ? 1 : c.length) * ROW + FOOT;
  };

  // Columns that take part in a foreign key, per table, for the link marker.
  const fkCols = $derived.by(() => {
    const m = new Map<string, Set<string>>();
    for (const e of graph.edges) {
      const s = m.get(e.from) ?? new Set();
      e.fk.columns.forEach((c) => s.add(c));
      m.set(e.from, s);
    }
    return m;
  });

  function layout() {
    const lanes = [graph.left, [graph.center], graph.right];
    const laneH = lanes.map((l) => l.reduce((h, n) => h + heightOf(n.key), 0) + Math.max(0, l.length - 1) * GY);
    const H = Math.max(...laneH);
    const p: Record<string, { x: number; y: number }> = {};
    let x = M;
    lanes.forEach((lane, i) => {
      if (!lane.length) return;
      let y = M + (H - laneH[i]) / 2;
      for (const n of lane) {
        p[n.key] = { x, y };
        y += heightOf(n.key) + GY;
      }
      x += W + GX;
    });
    return p;
  }

  let pos = $state<Record<string, { x: number; y: number }>>({});
  $effect(() => {
    catalog.rev;
    if (ready) pos = untrack(layout);
  });

  const nodes = $derived([graph.center, ...graph.left, ...graph.right]);
  const size = $derived.by(() => {
    let w = 0, h = 0;
    for (const n of nodes) {
      const p = pos[n.key];
      if (!p) continue;
      w = Math.max(w, p.x + W);
      h = Math.max(h, p.y + heightOf(n.key));
    }
    // Room for the self-reference loop on the right.
    return { w: w + M + 48, h: h + M };
  });

  /** Vertical middle of a column row, or of the header when the column is not listed. */
  function anchorY(k: string, col: string | undefined) {
    const c = colsOf(k);
    const i = typeof c === 'string' || col == null ? -1 : c.findIndex((x) => x.name === col);
    return pos[k].y + (i < 0 ? HEAD / 2 : HEAD + i * ROW + ROW / 2);
  }

  // The many side (the foreign key's table) gets a crow's foot, the one side two bars.
  function path(e: Edge) {
    const a = pos[e.from], b = pos[e.to];
    if (!a || !b) return null;
    const sy = anchorY(e.from, e.fk.columns[0]);
    const ty = anchorY(e.to, e.fk.refColumns[0]);
    if (e.from === e.to) {
      const x = a.x + W, out = 36;
      return {
        line: `M ${x} ${sy} C ${x + out} ${sy}, ${x + out} ${ty}, ${x} ${ty}`,
        ends: foot(x, sy, 1) + bars(x, ty, 1),
      };
    }
    const toRight = a.x + W / 2 < b.x + W / 2;
    const sx = toRight ? a.x + W : a.x, sd = toRight ? 1 : -1;
    const tx = toRight ? b.x : b.x + W, td = -sd;
    const k = Math.max(40, Math.abs(tx - sx) / 2);
    return {
      line: `M ${sx} ${sy} L ${sx + sd * 12} ${sy} C ${sx + sd * k} ${sy}, ${tx + td * k} ${ty}, ${tx + td * 12} ${ty} L ${tx} ${ty}`,
      ends: foot(sx, sy, sd) + bars(tx, ty, td),
    };
  }
  const foot = (x: number, y: number, d: number) => `M ${x + d * 12} ${y} L ${x} ${y - 6} M ${x + d * 12} ${y} L ${x} ${y + 6}`;
  const bars = (x: number, y: number, d: number) => `M ${x + d * 6} ${y - 6} L ${x + d * 6} ${y + 6} M ${x + d * 10} ${y - 6} L ${x + d * 10} ${y + 6}`;

  const label = (e: Edge) =>
    `${e.fk.name}\n${displayName(refOf(e.fk.schema ?? table.schema, e.fk.table))} (${e.fk.columns.join(', ')}) → ${displayName(refOf(e.fk.refSchema, e.fk.refTable))} (${e.fk.refColumns.join(', ')})`;

  let hover = $state<number | null>(null);
  const lit = $derived(hover == null ? null : graph.edges[hover]);
  const litCol = (k: string, c: string) =>
    !!lit && ((lit.from === k && lit.fk.columns.includes(c)) || (lit.to === k && lit.fk.refColumns.includes(c)));

  // The last grabbed box draws above the others.
  let top = $state<string | null>(null);
  // Drag offsets are in diagram units, so a drag tracks the pointer at any zoom.
  let drag: { key: string; dx: number; dy: number } | null = null;
  function grab(e: PointerEvent, key: string) {
    if (e.button !== 0) return;
    e.preventDefault(); // no text selection while the box moves over other boxes
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    drag = { key, dx: e.clientX / zoom - pos[key].x, dy: e.clientY / zoom - pos[key].y };
    top = key;
  }
  function move(e: PointerEvent) {
    if (!drag) return;
    pos[drag.key] = { x: Math.max(0, e.clientX / zoom - drag.dx), y: Math.max(0, e.clientY / zoom - drag.dy) };
  }

  const ZMIN = 0.25, ZMAX = 2, ZSTEP = 1.2;
  let zoom = $state(1);
  let scroller = $state<HTMLDivElement>();

  // The scroll position to restore once the new scale is in the DOM.
  let pendingScroll: { x: number; y: number; px: number; py: number } | null = null;

  /** Sets the zoom and keeps the diagram point under (px, py), relative to the scroller, in place. */
  function zoomTo(z: number, px?: number, py?: number) {
    const el = scroller;
    const next = Math.min(ZMAX, Math.max(ZMIN, z));
    if (!el || next === zoom) return;
    px ??= el.clientWidth / 2;
    py ??= el.clientHeight / 2;
    pendingScroll = { x: (el.scrollLeft + px) / zoom, y: (el.scrollTop + py) / zoom, px, py };
    zoom = next;
  }

  // After the render: the scaled diagram has its new size.
  $effect(() => {
    zoom;
    const el = scroller;
    const p = pendingScroll;
    if (!el || !p) return;
    pendingScroll = null;
    el.scrollLeft = p.x * zoom - p.px;
    el.scrollTop = p.y * zoom - p.py;
  });
  function fit() {
    if (!scroller) return;
    zoomTo(Math.min(1, (scroller.clientWidth - 8) / size.w, (scroller.clientHeight - 8) / size.h));
  }

  // ⌘/Ctrl + wheel and trackpad pinch zoom at the pointer. Needs a non-passive listener to stop the page zoom.
  $effect(() => {
    const el = scroller;
    if (!el) return;
    const onwheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      const r = el.getBoundingClientRect();
      zoomTo(zoom * Math.exp(-e.deltaY * 0.01), e.clientX - r.left, e.clientY - r.top);
    };
    el.addEventListener('wheel', onwheel, { passive: false });
    return () => el.removeEventListener('wheel', onwheel);
  });
</script>

<div class="relative h-full bg-sunken">
  {#if !ready}
    <div class="grid h-full place-items-center text-faint"><LoaderCircle size={16} class="animate-spin" /></div>
  {:else}
    <div class="absolute top-2 right-4 z-20 flex items-center gap-0.5 rounded-md border border-line bg-surface p-0.5 text-[12px] shadow-sm">
      <button class="btn btn-ghost size-7 p-0" title="Zoom out" aria-label="Zoom out" disabled={zoom <= ZMIN} onclick={() => zoomTo(zoom / ZSTEP)}><ZoomOut size={13} /></button>
      <button class="btn btn-ghost h-7 w-12 px-0 tabular-nums" title="Reset zoom to 100%" onclick={() => zoomTo(1)}>{Math.round(zoom * 100)}%</button>
      <button class="btn btn-ghost size-7 p-0" title="Zoom in" aria-label="Zoom in" disabled={zoom >= ZMAX} onclick={() => zoomTo(zoom * ZSTEP)}><ZoomIn size={13} /></button>
      <button class="btn btn-ghost size-7 p-0" title="Fit to view" aria-label="Fit to view" onclick={fit}><Maximize size={13} /></button>
      <span class="mx-0.5 h-4 w-px bg-line"></span>
      <button class="btn btn-ghost h-7 px-2" title="Reset layout" onclick={() => (pos = layout())}><RotateCcw size={13} /> Reset layout</button>
    </div>
    {#if !graph.edges.length}
      <div class="absolute top-3 left-3 z-20 text-[12px] text-faint">This table has no foreign keys and no other table references it.</div>
    {/if}
    <div class="absolute inset-0 overflow-auto" bind:this={scroller}>
    <div style="width:{size.w * zoom}px;height:{size.h * zoom}px">
    <div class="relative origin-top-left" style="width:{size.w}px;height:{size.h}px;transform:scale({zoom})">
      <svg class="pointer-events-none absolute inset-0 text-faint" width={size.w} height={size.h} aria-hidden="true">
        {#each graph.edges as e, i (i)}
          {@const p = path(e)}
          {#if p}
            <g class="pointer-events-auto {hover === i ? 'text-accent' : ''}" fill="none" stroke="currentColor" stroke-width={hover === i ? 2 : 1.25} role="presentation" onpointerenter={() => (hover = i)} onpointerleave={() => (hover = null)}>
              <title>{label(e)}</title>
              <path d={p.line} stroke="transparent" stroke-width="10" />
              <path d={p.line} />
              <path d={p.ends} />
            </g>
          {/if}
        {/each}
      </svg>
      {#each nodes as n (n.key)}
        {@const p = pos[n.key]}
        {@const cols = colsOf(n.key)}
        {#if p}
          <div
            class="absolute flex flex-col overflow-hidden rounded-md border bg-surface text-[12px] shadow-sm {n.self ? 'border-accent' : 'border-line'}"
            style="left:{p.x}px;top:{p.y}px;width:{W}px;height:{heightOf(n.key)}px;z-index:{top === n.key ? 2 : 1}"
          >
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="flex shrink-0 cursor-grab items-center gap-1.5 border-b border-line bg-raised px-2 font-medium text-ink select-none active:cursor-grabbing"
              style="height:{HEAD}px"
              title={n.self ? displayName(n.ref) : `${displayName(n.ref)} — double-click to open`}
              onpointerdown={(e) => grab(e, n.key)}
              onpointermove={move}
              onpointerup={() => (drag = null)}
              ondblclick={() => !n.self && onopen?.(n.ref)}
            >
              <Table2 size={13} class="shrink-0 {n.self ? 'text-accent' : 'text-faint'}" />
              <span class="truncate">{displayName(n.ref)}</span>
            </div>
            {#if typeof cols === 'string'}
              <div class="truncate px-2 text-red-600 dark:text-red-400" style="line-height:{ROW}px" title={cols}>{cols}</div>
            {:else}
              {#each cols as c (c.name)}
                <div class="flex items-center gap-1.5 px-2 {litCol(n.key, c.name) ? 'bg-accent/12' : ''}" style="height:{ROW}px">
                  <span class="flex w-3 shrink-0 justify-center">
                    {#if c.isPk}<KeyRound size={11} class="text-accent" />{:else if fkCols.get(n.key)?.has(c.name)}<Link size={11} class="text-faint" />{/if}
                  </span>
                  <span class="min-w-0 truncate {c.isPk ? 'font-medium text-ink' : 'text-ink'}">{c.name}</span>
                  <span class="ml-auto min-w-0 shrink truncate pl-2 font-mono text-[11px] text-faint" title={c.dataType}>{c.dataType}</span>
                </div>
              {:else}
                <div class="px-2 text-faint" style="line-height:{ROW}px">No columns</div>
              {/each}
            {/if}
          </div>
        {/if}
      {/each}
    </div>
    </div>
    </div>
  {/if}
</div>
