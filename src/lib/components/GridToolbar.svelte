<script lang="ts">
  import { Download, FileCode, Eye, LoaderCircle, Minus, Pencil, Plus, Undo2 } from '@lucide/svelte';
  import type { QueryResult } from '../api';
  import { pendingCount, type Edits } from '../editModel';
  import type { ExportMessage } from '../gridExport';
  import { moreRows } from '../results';

  let {
    result,
    edits,
    mongo,
    blocker,
    preparing,
    loading,
    view,
    exportMsg,
    canExportSql,
    fileExport,
    selectedCount,
    onedit,
    onnew,
    ondelete,
    ondiscard,
    onpreview,
    onsave,
    onview,
    onexport,
    onexportsql,
  }: {
    result: QueryResult;
    edits: Edits;
    mongo: boolean;
    /** Why the result cannot be edited; null when it can. */
    blocker: string | null;
    preparing: boolean;
    loading: boolean;
    view: 'columns' | 'json';
    exportMsg: ExportMessage | null;
    canExportSql: boolean;
    fileExport: boolean;
    selectedCount: number;
    onedit: () => void;
    onnew: () => void;
    ondelete: () => void;
    ondiscard: () => void;
    onpreview: () => void;
    onsave: () => void;
    onview: (v: 'columns' | 'json') => void;
    onexport: () => void;
    onexportsql: () => void;
  } = $props();

  const count = $derived(pendingCount(edits));
</script>

<footer class="@container flex h-9 shrink-0 items-center gap-1 border-t border-line px-2 text-[12px] text-muted">
  <span title={blocker ?? (edits.on ? 'Leave edit mode' : mongo ? 'Edit documents in this result' : 'Edit rows in this result')}>
    <button
      class="btn h-7 px-2 text-[12px] {edits.on ? 'bg-accent/6 text-accent' : 'btn-ghost'}"
      disabled={!!blocker || preparing}
      title={blocker ?? undefined}
      onclick={onedit}
    ><Pencil size={13} /> Edit</button>
  </span>
  {#if edits.on}
    <span class="px-1 text-[12px] font-medium text-accent">Editing</span>
    <button class="btn btn-ghost h-7 px-2 text-[12px]" title={mongo ? 'Add document' : 'Add row'} onclick={onnew}><Plus size={13} /><span class="hidden @[36rem]:inline">{mongo ? 'Document' : 'Row'}</span></button>
    <button class="btn btn-ghost h-7 px-2 text-[12px]" title={mongo ? 'Delete selected documents' : 'Delete selected rows'} disabled={!selectedCount} onclick={ondelete}><Minus size={13} /><span class="hidden @[36rem]:inline">Delete</span></button>
    <span class="mx-1 h-4 w-px bg-line"></span>
  {/if}
  {#if count}
    <span class="px-1 text-[12px] font-medium text-ink">{count} change{count === 1 ? '' : 's'}</span>
    <button class="btn btn-ghost h-7 px-2 text-[12px]" title="Discard changes" onclick={ondiscard}><Undo2 size={13} /><span class="hidden @[36rem]:inline">Discard</span></button>
    <button class="btn btn-ghost h-7 px-2 text-[12px]" title={mongo ? 'Preview commands' : 'Preview SQL'} onclick={onpreview}><Eye size={13} /><span class="hidden @[36rem]:inline">{mongo ? 'Preview commands' : 'Preview SQL'}</span></button>
    <button class="btn btn-primary h-7 px-3 text-[12px]" onclick={onsave}>Save</button>
  {:else}
    <span class="flex items-center gap-2.5 px-1.5 text-[12px] text-faint tabular-nums">
      <span title={result.truncated ? `First ${result.rows.length} rows; ${result.isRead ? (moreRows(result) ? 'scroll down to load more' : 'the page reached its size limit, so loading more brings no new rows') : 'more exist. Only a read-only statement loads more, because loading runs the SQL again'}` : undefined}>{result.rows.length}{result.truncated ? '+' : ''} rows</span>
      <span class="size-0.5 rounded-full bg-faint"></span>
      <span>{result.ms} ms</span>
    </span>
    {#if result.truncated && loading}
      <span class="flex items-center gap-1.5 px-1 text-[12px] text-faint"><LoaderCircle size={13} class="animate-spin" /> Loading more…</span>
    {/if}
  {/if}
  {#if Array.isArray(result.documents)}
    <div class="seg ml-2 shrink-0" role="tablist" aria-label="Result view">
      <button class="seg-item {view === 'columns' ? 'seg-on' : ''}" role="tab" aria-selected={view === 'columns'} onclick={() => onview('columns')}>Columns</button>
      <button class="seg-item {view === 'json' ? 'seg-on' : ''}" role="tab" aria-selected={view === 'json'} onclick={() => onview('json')}>JSON</button>
    </div>
  {/if}
  {#if exportMsg}
    <span class="min-w-0 truncate text-[12px] {exportMsg.ok ? 'text-emerald-700 dark:text-emerald-400' : 'text-red-600 dark:text-red-400'}" title={exportMsg.text}>{exportMsg.text}</span>
  {/if}
  <!-- A MongoDB result has no INSERT statements; the document formats come from the export dialog. -->
  {#if !mongo && canExportSql}
    <button class="btn btn-ghost ml-auto h-7 px-2 text-[12px]" title="Export as SQL INSERT statements" onclick={onexportsql}><FileCode size={13} /><span class="hidden @[36rem]:inline">Export SQL</span></button>
  {/if}
  <button class="btn btn-ghost h-7 px-2 text-[12px] {mongo ? 'ml-auto' : ''}" title={fileExport ? 'Export the documents as CSV, JSON or NDJSON' : 'Export CSV'} onclick={onexport}><Download size={13} /><span class="hidden @[36rem]:inline">{fileExport ? 'Export...' : 'Export CSV'}</span></button>
</footer>
