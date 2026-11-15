<script lang="ts">
  import { tick } from 'svelte';
  import { agentSortOptions, type AgentSortMode } from './lib/agentSort';
  let { mode, onchange }: { mode: AgentSortMode; onchange: (mode: AgentSortMode) => void } = $props();
  let open = $state(false);
  let trigger: HTMLButtonElement;
  let popup = $state<HTMLDivElement>();
  let container: HTMLDivElement;
  const selected = $derived(agentSortOptions.find(option => option.mode === mode) ?? agentSortOptions[0]);
  async function show() {
    open = true;
    await tick();
    popup?.querySelector<HTMLButtonElement>('[aria-checked="true"]')?.focus({ preventScroll: true });
  }
  function close(restore = false) {
    open = false;
    if (restore) trigger?.focus({ preventScroll: true });
  }
  function keydown(event: KeyboardEvent) {
    if (!open) return;
    // Header navigation must never leak to the terminal behind it.
    event.stopPropagation();
    if (event.key === 'Escape') { event.preventDefault(); close(true); }
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      const buttons = [...(popup?.querySelectorAll<HTMLButtonElement>('button') ?? [])];
      const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
      buttons[next]?.focus({ preventScroll: true });
    }
  }
</script>

<svelte:window onpointerdown={(event) => { if (open && !container.contains(event.target as Node)) close(); }} />
<div class="agent-sort" bind:this={container} onfocusout={(event) => { if (!container.contains(event.relatedTarget as Node)) close(); }}>
  <button class="sort-trigger" bind:this={trigger} type="button" aria-label={`Sort agents: ${selected.label}`} title={`Sort agents: ${selected.label}`} aria-haspopup="menu" aria-expanded={open}
    onkeydown={(event) => {
      event.stopPropagation();
      if (event.key === 'Escape' && open) { event.preventDefault(); close(true); }
      else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); void show(); }
    }}
    onclick={(event) => { event.stopPropagation(); if (open) close(); else void show(); }}>
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      {#if mode === 'grouped'}<path d="m12 3 10 5-10 5L2 8Z M2 12l10 5 10-5 M2 16l10 5 10-5" />
      {:else}<circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" />{/if}
    </svg>
  </button>
  {#if open}
    <div class="sort-popup" bind:this={popup} role="menu" aria-label="Sort agents" tabindex="-1" onkeydown={keydown}>
      <div class="sort-caption">Sort agents</div>
      {#each agentSortOptions as option (option.mode)}
        <button type="button" role="menuitemradio" aria-checked={mode === option.mode} onclick={(event) => { event.stopPropagation(); onchange(option.mode); close(true); }}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            {#if option.mode === 'grouped'}<path d="m12 3 10 5-10 5L2 8Z M2 12l10 5 10-5 M2 16l10 5 10-5" />
            {:else}<circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" />{/if}
          </svg>
          <span>{option.label}</span>
          {#if mode === option.mode}<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="m5 12 4 4L19 6" /></svg>{/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .agent-sort{position:relative;flex:none}
  .sort-trigger{display:flex;align-items:center;justify-content:center;width:44px;height:44px;min-height:44px;padding:0;border:0;background:transparent;color:var(--muted);border-radius:7px}
  .sort-popup{position:absolute;top:100%;right:0;z-index:30;width:220px;max-width:calc(100vw - 24px);padding:5px;border:1px solid var(--line);border-radius:10px;background:var(--panel);color:var(--ink);box-shadow:0 8px 24px #0003}
  .sort-caption{padding:4px 8px 6px;font-size:calc(11rem / 13);color:var(--muted)}
  .sort-popup button{display:flex;align-items:center;gap:9px;width:100%;min-height:44px;padding:6px 8px;border:0;border-radius:7px;background:transparent;text-align:left;font-size:calc(12.8rem / 13)}
  .sort-popup button:hover,.sort-popup button:focus-visible{background:var(--raised)}
  .sort-popup button span{flex:1}
  .sort-popup svg{flex:none}
  .sort-popup button>svg:first-child{color:var(--muted)}
</style>
