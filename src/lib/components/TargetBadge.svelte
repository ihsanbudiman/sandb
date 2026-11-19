<script lang="ts">
  import EnvironmentBadge from './EnvironmentBadge.svelte';
  import type { Snippet } from 'svelte';
  import type { Conn } from '../connections.svelte';
  import { displayName, type TableRef } from '../tableActions';
  import EngineBadge from './ui/EngineBadge.svelte';

  /** The identity of a target: engine, connection name, environment, then database and table or collection. */
  let {
    conn,
    name,
    target = null,
    bare = false,
    children,
  }: {
    conn: Pick<Conn, 'engine' | 'name' | 'env' | 'envLabel'>;
    /** Shown instead of `conn.name` when the caller has a better label. */
    name?: string;
    target?: TableRef | null;
    /** Inline row, without the sunken box. */
    bare?: boolean;
    /** Extra badges or text after the target. */
    children?: Snippet;
  } = $props();
</script>

<p class={bare ? 'flex min-w-0 flex-wrap items-center gap-2 text-sm' : 'flex flex-wrap items-center gap-2 rounded-lg bg-sunken px-3 py-2'}>
  <EngineBadge engine={conn.engine} />
  <b class="min-w-0 truncate font-medium text-ink">{name ?? conn.name}</b>
  <EnvironmentBadge conn={conn} />
  {#if target}
    <span class="min-w-0 truncate font-mono text-sm text-muted">{conn.engine === 'sqlite' ? '' : target.database + ' · '}{displayName(target)}</span>
  {/if}
  {@render children?.()}
</p>
