<script lang="ts">
  import { LoaderCircle, Square } from '@lucide/svelte';
  import { clock, type Running } from '../run.svelte';

  /**
   * The results pane while a query runs. A re-run of the shown result (load more, sort) gets the top bar
   * only, so its rows stay usable. Another run veils the shown result (`over`), or, with nothing shown,
   * puts a skeleton grid in place of the rows.
   */
  let { run, over = false, oncancel }: { run: Running; over?: boolean; oncancel?: () => void } = $props();

  // Cancel can take a moment on the server; the button says so until the run ends.
  let stopping = $state<string | null>(null);
  const isStopping = $derived(stopping === run.id);
  const ms = $derived(Math.max(0, clock.now - run.started));
  const elapsed = $derived(ms < 60_000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.floor(ms / 60_000)}m ${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}s`);
  const script = $derived(run.script);
  const pct = $derived(script?.total ? Math.min(100, (script.done / script.total) * 100) : null);

  const COLS = [18, 28, 22, 34, 16, 26];
  const ROWS = 14;

  function stop() {
    stopping = run.id;
    oncancel?.();
  }
</script>

<!-- Top bar: determinate for a script once its total is known, indeterminate otherwise. -->
<div class="pointer-events-none absolute inset-x-0 top-0 z-[21] h-0.5 overflow-hidden bg-accent/15" aria-hidden="true">
  {#if pct !== null}
    <div class="h-full bg-accent transition-[width] duration-300" style:width="{pct}%"></div>
  {:else}
    <div class="h-full w-full origin-left animate-indeterminate bg-accent motion-reduce:animate-none motion-reduce:opacity-60"></div>
  {/if}
</div>

{#if !run.again}
  <div class="absolute inset-0 z-20 overflow-hidden {over ? 'bg-surface/70 backdrop-blur-[1.5px]' : ''}">
    {#if !over}
      <!-- Skeleton grid: the shape of the rows to come, fading out downward. -->
      <div class="absolute inset-0 [mask-image:linear-gradient(to_bottom,black_30%,transparent)]" aria-hidden="true">
        <div class="flex h-8 items-center border-b border-line bg-surface">
          <div class="w-9 shrink-0"></div>
          {#each COLS as w, ci (ci)}
            <div class="flex h-full flex-1 items-center border-l border-line px-2.5">
              <div class="h-2.5 animate-shimmer rounded-full bg-hover motion-reduce:animate-none" style:width="{w + 22}%"></div>
            </div>
          {/each}
        </div>
        {#each { length: ROWS } as _, ri (ri)}
          <div class="flex h-[26px] items-center border-b border-line/60">
            <div class="flex w-9 shrink-0 justify-end px-2"><div class="h-2 w-3 rounded-full bg-hover/70"></div></div>
            {#each COLS as w, ci (ci)}
              <div class="flex h-full flex-1 items-center border-l border-line/60 px-2.5">
                <div
                  class="h-2 animate-shimmer rounded-full bg-hover motion-reduce:animate-none"
                  style:width="{w + ((ri * 37 + ci * 23) % 45)}%"
                  style:animation-delay="{(ri * 60 + ci * 25) % 900}ms"
                ></div>
              </div>
            {/each}
          </div>
        {/each}
      </div>
    {/if}

    <div class="absolute inset-0 grid place-items-center p-4">
      <div class="popover flex w-80 max-w-full flex-col gap-3 px-4 py-3.5" role="status" aria-live="polite">
        <div class="flex items-center gap-3">
          <span class="relative grid size-8 shrink-0 place-items-center rounded-full bg-accent/12 text-accent">
            <LoaderCircle size={16} class="animate-spin" />
          </span>
          <div class="min-w-0 flex-1">
            <div class="truncate text-[13px] font-semibold text-ink" title={script?.name}>{script ? script.name : isStopping ? 'Cancelling query' : 'Running query'}</div>
            <div class="text-[11.5px] leading-snug text-muted">
              {#if isStopping}Waiting for the database to stop
              {:else if script}{script.total ? `Statement ${Math.min(script.done + 1, script.total)} of ${script.total}` : 'Reading the script'}
              {:else if over}The last result stays until this one is ready
              {:else}Waiting for the database{/if}
            </div>
          </div>
          <span class="shrink-0 font-mono text-[12px] text-ink tabular-nums" aria-label="Elapsed time">{elapsed}</span>
        </div>
        {#if script?.total}
          <div class="h-1 overflow-hidden rounded-full bg-hover" role="progressbar" aria-label="Script progress" aria-valuenow={script.done} aria-valuemin={0} aria-valuemax={script.total}>
            <div class="h-full rounded-full bg-accent transition-[width] duration-300" style:width="{pct}%"></div>
          </div>
        {/if}
        {#if oncancel}
          <button class="btn btn-secondary w-full" onclick={stop} disabled={isStopping}>
            {#if isStopping}<LoaderCircle size={12} class="animate-spin" /> Cancelling…{:else}<Square size={10} class="fill-current" /> Cancel query{/if}
          </button>
        {/if}
      </div>
    </div>
  </div>
{/if}
