<script lang="ts">
  import { ChevronDown, ChevronRight, Copy, LoaderCircle } from '@lucide/svelte';
  import { copyValue, ejsonText, ejsonWrapper } from '../results';

  /** The page's documents as relaxed Extended JSON, one entry each; `more`/`onloadmore` are the run's Load more. `name` labels a lone value shown instead of documents. */
  let {
    documents,
    name,
    more = false,
    loading = false,
    onloadmore,
  }: { documents: unknown[]; name?: string; more?: boolean; loading?: boolean; onloadmore?: () => void } = $props();

  // Open nodes by path (`0.address.city`): a path enters the map only when the user expands it, so a
  // collapsed subtree costs nothing — no child is built or rendered until then.
  let open = $state<Record<string, boolean>>({});
  // Long strings collapse to a cut preview until clicked.
  let full = $state<Record<string, boolean>>({});
  // A lone document starts open; the flag sticks when a page load brings more documents.
  $effect.pre(() => {
    if (documents.length === 1) open['0'] = true;
  });

  const isOpen = (path: string) => open[path] ?? false;
  const toggle = (path: string) => (open[path] = !open[path]);

  // Arrays, plain objects and documents have children; EJSON wrappers and scalars are leaves.
  const isBranch = (v: unknown) =>
    (Array.isArray(v) && v.length > 0) ||
    (v !== null && typeof v === 'object' && !Array.isArray(v) && !ejsonWrapper(v) && Object.keys(v).length > 0);

  /** Children of an open branch, built only while it is open. */
  const kidsOf = (v: unknown, path: string): { path: string; label: string; value: unknown }[] =>
    Array.isArray(v)
      ? v.map((x, i) => ({ path: `${path}.${i}`, label: String(i), value: x }))
      : Object.entries(v as Record<string, unknown>).map(([k, x]) => ({ path: `${path}.${k}`, label: k, value: x }));

  /** Preview of a collapsed document: its _id in mongosh style, cut short. */
  const idOf = (doc: unknown) => {
    const id = doc !== null && typeof doc === 'object' && !Array.isArray(doc) ? (doc as Record<string, unknown>)._id : undefined;
    const text = id === undefined ? 'no _id' : ejsonText(id);
    return text.length > 48 ? text.slice(0, 47) + '…' : text;
  };

  const copyNode = (v: unknown) => navigator.clipboard?.writeText(copyValue(v)).catch(() => {});

  const NUMERIC = new Set(['$numberLong', '$numberDecimal', '$numberInt', '$numberDouble']);
  /** Syntax color of a value, in the editor's palette. For a wrapper (`ObjectId("…")`) it is the color of its argument. */
  const tone = (v: unknown) => {
    if (v === null) return 'italic text-faint';
    if (typeof v === 'string') return 'tok-str';
    if (typeof v === 'number') return 'tok-num';
    if (typeof v === 'boolean') return 'tok-kw';
    const w = ejsonWrapper(v);
    if (w) return NUMERIC.has(w[0]) ? 'tok-num' : 'tok-str';
    return 'text-faint';
  };
  /** `Name(args)` parts of a wrapper's mongosh text, so the name and the argument take different colors. */
  const callOf = (v: unknown, text: string) => (ejsonWrapper(v) ? /^(\w+)\((.*)\)$/s.exec(text) : null);
  /** Size of a branch: `3 items` or `4 fields`. */
  const sizeOf = (v: unknown) => {
    const n = Array.isArray(v) ? v.length : Object.keys(v as object).length;
    return Array.isArray(v) ? `${n} item${n === 1 ? '' : 's'}` : `${n} field${n === 1 ? '' : 's'}`;
  };
  const idValue = (doc: unknown) =>
    doc !== null && typeof doc === 'object' && !Array.isArray(doc) ? (doc as Record<string, unknown>)._id : undefined;
</script>

<div class="min-w-0 p-2 font-mono text-[12px] leading-5">
  {#if documents.length}
    {#each documents as doc, i (i)}
      <div class="border-b border-line/70 py-0.5 last:border-b-0">
        {@render node(String(i), name ?? String(i), doc, 0, name ? '' : idOf(doc))}
      </div>
    {/each}
  {:else}
    <div class="text-muted">No documents</div>
  {/if}
  {#if more && onloadmore}
    <div class="pt-1 pl-0.5">
      <button class="btn btn-ghost h-7 px-2 text-[12px]" disabled={loading} onclick={() => onloadmore?.()}>
        {#if loading}<LoaderCircle size={13} class="animate-spin" /> Loading more…{:else}<ChevronDown size={13} /> Load more{/if}
      </button>
    </div>
  {/if}
</div>

{#snippet val(v: unknown, text: string)}
  {@const call = callOf(v, text)}
  {#if call}<span class="tok-type">{call[1]}</span><span class="text-faint">(</span><span class={tone(v)}>{call[2]}</span><span class="text-faint">)</span>{:else}<span class={tone(v)}>{text}</span>{/if}
{/snippet}

{#snippet node(path: string, label: string, value: unknown, depth: number, hint: string)}
  {@const doc = depth === 0 && !name}
  <div class="group/n flex items-start gap-0.5 rounded px-0.5 hover:bg-hover">
    {#if isBranch(value)}
      <button
        class="flex size-5 shrink-0 items-center justify-center rounded text-faint hover:bg-active hover:text-ink"
        aria-expanded={isOpen(path)}
        aria-label={isOpen(path) ? `Collapse ${label}` : `Expand ${label}`}
        onclick={() => toggle(path)}
      >
        {#if isOpen(path)}<ChevronDown size={13} />{:else}<ChevronRight size={13} />{/if}
      </button>
      <button class="flex min-w-0 flex-1 items-baseline gap-2 truncate text-left" onclick={() => toggle(path)}>
        <span class="shrink-0 {doc ? 'min-w-[2ch] text-faint tabular-nums' : 'font-medium text-ink'}">{label}</span>
        {#if doc && hint && !isOpen(path)}
          <span class="truncate"><span class="mr-1.5 text-faint">_id:</span>{@render val(idValue(value), hint)}</span>
        {/if}
        <span class="shrink-0 rounded bg-sunken px-1.5 text-[10.5px] leading-4 text-faint shadow-[inset_0_0_0_1px_var(--c-line)]">{sizeOf(value)}</span>
      </button>
    {:else}
      <span class="w-5 shrink-0"></span>
      <span class="shrink-0 font-medium text-ink">{label}<span class="text-faint">:</span></span>
      {#if typeof value === 'string' && value.length > 80}
        <button
          class="tok-str ml-1.5 min-w-0 flex-1 cursor-pointer text-left break-all whitespace-pre-wrap hover:underline"
          title={full[path] ? undefined : 'Show full value'}
          onclick={() => (full[path] = !full[path])}
        >{full[path] ? JSON.stringify(value) : JSON.stringify(value).slice(0, 81) + '…'}</button>
      {:else}
        <span class="ml-1.5 min-w-0 flex-1 break-all">{@render val(value, ejsonText(value))}</span>
      {/if}
    {/if}
    <button
      class="shrink-0 rounded p-0.5 text-faint opacity-0 group-hover/n:opacity-100 focus-visible:opacity-100 hover:bg-active hover:text-ink"
      title={doc ? 'Copy document' : 'Copy value'}
      aria-label={doc ? 'Copy document' : `Copy value of ${label}`}
      onclick={() => copyNode(value)}
    ><Copy size={12} /></button>
  </div>
  {#if isOpen(path) && isBranch(value)}
    <div class="ml-[12px] border-l border-line pl-1.5">
      {#each kidsOf(value, path) as k (k.path)}
        {@render node(k.path, k.label, k.value, depth + 1, '')}
      {/each}
    </div>
  {/if}
{/snippet}
