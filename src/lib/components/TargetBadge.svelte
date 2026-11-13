<script lang="ts">
  import type { Snippet } from 'svelte';
  import { engines, envTag, type Conn } from '../connections.svelte';
  import { displayName, type TableRef } from '../tableActions';

  /** The identity of a target: engine, connection name, environment, then database and table or collection. */
  let {
    conn,
    name,
    target = null,
    bare = false,
    children,
  }: {
    conn: Pick<Conn, 'engine' | 'name' | 'env'>;
    /** Shown instead of `conn.name` when the caller has a better label. */
    name?: string;
    target?: TableRef | null;
    /** Inline row, without the sunken box. */
    bare?: boolean;
    /** Extra badges or text after the target. */
    children?: Snippet;
  } = $props();
</script>

<p class={bare ? 'flex min-w-0 flex-wrap items-center gap-2 text-[12px]' : 'flex flex-wrap items-center gap-2 rounded-lg bg-sunken px-3 py-2'}>
  <span class="font-mono text-[10.5px] font-semibold {engines[conn.engine].badge}">{engines[conn.engine].short}</span>
  <b class="min-w-0 truncate font-medium text-ink">{name ?? conn.name}</b>
  <span class="tag {envTag[conn.env]}">{conn.env}</span>
  {#if target}
    <span class="min-w-0 truncate font-mono text-[12px] text-muted">{conn.engine === 'sqlite' ? '' : target.database + ' · '}{displayName(target)}</span>
  {/if}
  {@render children?.()}
</p>
