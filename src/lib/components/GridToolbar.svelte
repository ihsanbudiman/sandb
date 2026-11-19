<script lang="ts">
  import { Download, FileCode, Eye, LoaderCircle, Minus, Pencil, Plus, Sparkles, Undo2 } from '@lucide/svelte';
  import type { Snippet } from 'svelte';
  import type { QueryResult } from '../api';
  import { pendingCount, type Edits } from '../editModel';
  import type { ExportMessage } from '../gridExport';
  import { moreRows } from '../results';

  type GridActions = {
    result: QueryResult;
    edits: Edits;
    mongo: boolean;
    redis: boolean;
    canEditRows: boolean;
    deleteBlocker: string | null;
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
    /** Set when the result is a query plan: sends it to the assistant for a performance review. */
    onreview?: () => void;
  };

  let props: { windowControls?: Snippet; summary?: QueryResult | null } & (GridActions | { result?: never }) = $props();
</script>

<footer class="@container flex h-sub-toolbar shrink-0 items-center gap-1 border-t border-line px-2 text-sm text-muted">
  {@render props.windowControls?.()}
  {#if props.result}
    {@const { result, edits, mongo, redis, canEditRows, deleteBlocker, blocker, preparing, loading, view, exportMsg, canExportSql, fileExport, selectedCount, onedit, onnew, ondelete, ondiscard, onpreview, onsave, onview, onexport, onexportsql, onreview } = props}
    {@const count = pendingCount(edits)}
    {#if props.windowControls}<span class="mx-0.5 h-4 w-px shrink-0 bg-line"></span>{/if}
    <span class="shrink-0" title={blocker ?? (edits.on ? 'Leave edit mode' : mongo ? 'Edit documents in this result' : 'Edit rows in this result')}>
      <button class="btn btn-sm btn-ghost btn-on" aria-pressed={edits.on} disabled={!!blocker || preparing} title={blocker ?? undefined} onclick={onedit}><Pencil size={13} /> Edit</button>
    </span>
    {#if edits.on}
      <span class="hidden px-1 text-xs font-medium text-accent @[36rem]:inline">Editing</span>
      {#if canEditRows}
        <button class="btn btn-sm btn-ghost" title={mongo ? 'Add document' : 'Add row'} onclick={onnew}><Plus size={13} /><span class="hidden @[44rem]:inline">{mongo ? 'Document' : 'Row'}</span></button>
        <button class="btn btn-sm btn-ghost" title={deleteBlocker ?? (mongo ? 'Delete selected documents' : 'Delete selected rows')} disabled={!selectedCount || !!deleteBlocker} onclick={ondelete}><Minus size={13} /><span class="hidden @[44rem]:inline">Delete</span></button>
      {/if}
      <span class="mx-0.5 h-4 w-px shrink-0 bg-line"></span>
    {/if}
    {#if count}
      <span class="shrink-0 px-1 text-xs font-medium text-ink">{count} change{count === 1 ? '' : 's'}</span>
      <button class="btn btn-sm btn-ghost" title="Discard changes" onclick={ondiscard}><Undo2 size={13} /><span class="hidden @[44rem]:inline">Discard</span></button>
      <button class="btn btn-sm btn-ghost" title={mongo || redis ? 'Preview commands' : 'Preview SQL'} onclick={onpreview}><Eye size={13} /><span class="hidden @[44rem]:inline">{mongo || redis ? 'Preview commands' : 'Preview SQL'}</span></button>
      <button class="btn btn-sm btn-primary" onclick={onsave}>Save</button>
    {:else}
      <span class="flex min-w-0 items-center gap-1.5 truncate px-1 text-xs text-faint tabular-nums">
        <span class="shrink-0" title={result.truncated ? `First ${result.rows.length} rows; ${result.isRead ? (moreRows(result) ? 'scroll down to load more' : 'the page reached its size limit, so loading more brings no new rows') : `more exist. Only a read-only statement loads more, because loading runs the ${redis ? 'command' : 'SQL'} again`}` : undefined}>{result.rows.length}{result.truncated ? '+' : ''} rows</span>
        <span class="size-0.5 shrink-0 rounded-full bg-faint"></span>
        <span class="shrink-0">{result.ms} ms</span>
      </span>
      {#if result.truncated && loading}
        <span class="flex items-center gap-1.5 px-1 text-xs text-faint" role="status"><LoaderCircle size={13} class="motion-safe:animate-spin" /> <span class="sr-only @[36rem]:not-sr-only">Loading more…</span></span>
      {/if}
    {/if}
    {#if Array.isArray(result.documents)}
      <div class="seg ml-1 shrink-0" role="tablist" aria-label="Result view">
        <button class="seg-item {view === 'columns' ? 'seg-on' : ''}" role="tab" aria-selected={view === 'columns'} onclick={() => onview('columns')}>Columns</button>
        <button class="seg-item {view === 'json' ? 'seg-on' : ''}" role="tab" aria-selected={view === 'json'} onclick={() => onview('json')}>JSON</button>
      </div>
    {/if}
    {#if exportMsg}
      <span class="min-w-0 truncate text-xs {exportMsg.ok ? 'text-ok' : 'text-danger'}" title={exportMsg.text}>{exportMsg.text}</span>
    {/if}
    <div class="ml-auto flex shrink-0 items-center gap-1">
      {#if onreview}
        <button class="btn btn-sm btn-ghost text-accent" title="Send this plan to the assistant for a performance review" onclick={onreview}><Sparkles size={13} /><span class="hidden @[44rem]:inline">Review with AI</span></button>
      {/if}
      {#if canExportSql}
        <button class="btn btn-sm btn-ghost" title="Export as SQL INSERT statements" onclick={onexportsql}><FileCode size={13} /><span class="hidden @[44rem]:inline">Export SQL</span></button>
      {/if}
      <button class="btn btn-sm btn-ghost" title={fileExport ? 'Export the documents as CSV, JSON or NDJSON' : 'Export CSV'} onclick={onexport}><Download size={13} /><span class="hidden @[44rem]:inline">{fileExport ? 'Export...' : 'Export CSV'}</span></button>
    </div>
  {:else if props.summary}
    {@const result = props.summary}
    <span class="px-1 text-xs text-faint tabular-nums">{result.rows.length}{result.truncated ? '+' : ''} rows · {result.ms} ms</span>
  {/if}
</footer>
