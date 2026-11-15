<script lang="ts">
  import { onMount } from 'svelte';
  import { request } from './lib/api';
  import { captureTransportRequest } from './lib/transport';
  import { clearUpdateNotice, parseComponentUpdates, settleUpdateNotice, updateNotice, updateScope, watchUpdateNotices } from './lib/componentUpdates';
  let { connected = false, computerId = '' }: { connected?: boolean; computerId?: string } = $props();
  let revision = $state(0);
  const scope = $derived(updateScope(computerId));
  function observedNotice(key: string, version: number) { const value = updateNotice(key); return value ? { ...value, revision: version } : undefined; }
  const notice = $derived(observedNotice(scope, revision));
  onMount(() => watchUpdateNotices(() => revision++));
  $effect(() => {
    const target = scope;
    if (!connected || notice?.state !== 'pending') return;
    const send = captureTransportRequest();
    const abort = new AbortController();
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      try {
        const value = parseComponentUpdates(await request('/api/component-updates', undefined, abort.signal, send));
        if (!disposed) {
          const current = updateNotice(target);
          if (current) settleUpdateNotice(target, value[current.component]);
        }
      } catch { /* An update can replace this transport. Never replay the mutation. */ }
      if (!disposed) timer = setTimeout(poll, 2000);
    }
    void poll();
    return () => { disposed = true; abort.abort(); clearTimeout(timer); };
  });
</script>

{#if notice}
  <aside class="update-progress" role="status" aria-live="polite">
    <span>{notice.state === 'pending' && !connected ? 'Update requested. Reconnecting automatically; terminal input is not queued.' : notice.message}</span>
    <button aria-label="Dismiss update progress" onclick={() => clearUpdateNotice(scope)}>×</button>
  </aside>
{/if}

<style>
  .update-progress { position: fixed; left: 12px; right: 12px; bottom: max(12px, env(safe-area-inset-bottom)); z-index: 1100; display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 8px 12px; border: 1px solid var(--line); border-radius: 10px; background: var(--raised); box-shadow: 0 6px 24px #0002; font-size: .923077rem; }
  span { min-width: 0; overflow-wrap: anywhere; }
  button { flex-shrink: 0; min-width: 44px; min-height: 44px; padding: 0; background: transparent; border: 0; font-size: 1.5rem; }
</style>
