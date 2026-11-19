<script lang="ts">
  import type { Asked } from '../ask.svelte';
  import type { Conn } from '../connections.svelte';
  import { quoteRedisArg } from '../redis/text';
  import Modal from './Modal.svelte';
  import TargetBadge from './TargetBadge.svelte';
  import Select from './ui/Select.svelte';

  type Target = { mode: 'new'; database: string } | { mode: 'rename' | 'ttl'; database: string; key: string };
  let { conn, target, answer }: { conn: Conn; target: Target } & Asked<string | null> = $props();

  const kinds = ['string', 'hash', 'list', 'set', 'zset', 'stream'] as const;
  const kindOptions = kinds.map((value) => ({ value, label: value }));
  let kind = $state<(typeof kinds)[number]>('string');
  let name = $state('');
  let field = $state('');
  let value = $state('');
  let score = $state('0');
  let milliseconds = $state('');
  let persist = $state(false);
  const id = $props.id();

  const title = $derived(target.mode === 'new' ? 'New Redis key' : target.mode === 'rename' ? 'Rename Redis key' : 'Set Redis key TTL');
  const ms = $derived(milliseconds.trim().replace(/^0+(?=\d)/, ''));
  const ttlValid = $derived(/^[1-9]\d{0,18}$/.test(ms) && BigInt(ms) <= 9_223_372_036_854_775_807n);
  const scoreText = $derived(score.trim());
  const scoreValid = $derived(/^[+-]?(?:inf|(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?)$/i.test(scoreText));

  const command = $derived.by(() => {
    if (target.mode === 'rename') return name ? `RENAME ${target.key} ${quoteRedisArg(name)}` : '';
    if (target.mode === 'ttl') return persist ? `PERSIST ${target.key}` : ttlValid ? `PEXPIRE ${target.key} ${ms}` : '';
    if (!name) return '';
    const key = quoteRedisArg(name);
    const arg = quoteRedisArg(value);
    switch (kind) {
      case 'string': return `SET ${key} ${arg}`;
      case 'hash': return `HSET ${key} ${quoteRedisArg(field)} ${arg}`;
      case 'list': return `RPUSH ${key} ${arg}`;
      case 'set': return `SADD ${key} ${arg}`;
      case 'zset': return scoreValid ? `ZADD ${key} ${scoreText} ${arg}` : '';
      case 'stream': return `XADD ${key} * ${quoteRedisArg(field)} ${arg}`;
    }
  });
</script>

<Modal {title} onclose={() => answer(null)} onsubmit={(e) => { e.preventDefault(); if (command) answer(command); }}>
  <TargetBadge {conn}>
    <span class="min-w-0 truncate font-mono text-sm text-muted">db{target.database}{target.mode === 'new' ? '' : ` · ${target.key}`}</span>
  </TargetBadge>

  <div class="mt-3 space-y-3">
    {#if target.mode === 'new' || target.mode === 'rename'}
      <label class="block space-y-1.5">
        <span class="label">{target.mode === 'new' ? 'Key name' : 'New key name'}</span>
        <input class="input input-sm font-mono" data-autofocus bind:value={name} autocapitalize="off" spellcheck="false" autocomplete="off" />
      </label>
    {/if}
    {#if target.mode === 'new'}
      <div class="space-y-1.5">
        <span class="label" id="{id}-type">Key type</span>
        <Select aria-labelledby="{id}-type" size="sm" bind:value={kind} options={kindOptions} />
      </div>
      {#if kind === 'hash' || kind === 'stream'}
        <label class="block space-y-1.5"><span class="label">Field</span><input class="input input-sm font-mono" bind:value={field} autocapitalize="off" spellcheck="false" autocomplete="off" /></label>
      {/if}
      {#if kind === 'zset'}
        <label class="block space-y-1.5"><span class="label">Score</span><input class="input input-sm font-mono" bind:value={score} spellcheck="false" autocomplete="off" /></label>
      {/if}
      <label class="block space-y-1.5">
        <span class="label">{kind === 'set' || kind === 'zset' ? 'Member' : 'Value'}</span>
        <textarea class="input h-auto py-2 font-mono text-sm" rows="2" bind:value autocapitalize="off" spellcheck="false"></textarea>
      </label>
    {:else if target.mode === 'ttl'}
      <label class="block space-y-1.5">
        <span class="label">TTL (milliseconds)</span>
        <input class="input input-sm font-mono" data-autofocus bind:value={milliseconds} disabled={persist} inputmode="numeric" placeholder="1000" autocomplete="off" />
      </label>
      <label class="flex items-center gap-2 text-sm text-muted"><input type="checkbox" bind:checked={persist} /> Remove expiry</label>
    {/if}
    {#if command}<pre class="whitespace-pre-wrap break-words rounded-md bg-sunken p-2 font-mono text-sm text-ink"><code>{command}</code></pre>{/if}
  </div>

  {#snippet footer()}
    <button type="button" class="btn btn-sm btn-secondary" onclick={() => answer(null)}>Cancel</button>
    <button type="submit" class="btn btn-sm btn-primary" disabled={!command}>Run</button>
  {/snippet}
</Modal>
