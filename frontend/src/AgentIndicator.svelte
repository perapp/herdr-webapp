<script lang="ts">
  let { status, showIdle = true }: { status: string; showIdle?: boolean } = $props();
  const label = $derived(status === 'blocked' ? 'Needs you' : status === 'done' ? 'Finished' : status === 'working' ? 'Working' : status === 'idle' ? 'Idle' : 'No reported status');
</script>

{#if status !== 'unreported' && (status !== 'idle' || showIdle)}
  <span class="agent-indicator" data-status={status} role="status" aria-label={label} title={label}>
    {#if status === 'working'}
      <!-- Native defaults/mods/workspace-chrome/spinner.svg; 900ms rotation. -->
      <svg class="spinner" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="9.4" stroke-opacity="0.24"/><path d="M12 2.6a9.4 9.4 0 0 1 9.4 9.4"/></svg>
    {:else if status === 'blocked'}<span class="needs-you">Needs you</span>
    {:else}<span class="status-dot" aria-hidden="true"></span>{/if}
  </span>
{/if}

<style>
  .agent-indicator{display:inline-flex;align-items:center;justify-content:center;flex:none;color:var(--agent-idle)}
  [data-status=working]{color:var(--agent-working)}
  [data-status=blocked]{color:var(--agent-blocked)}
  [data-status=done]{color:var(--agent-done)}
  .spinner{animation:agent-spin 900ms linear infinite}
  .needs-you{padding:1px 6px;border-radius:5px;background:color-mix(in srgb,var(--agent-blocked) 15%,transparent);font-size:calc(10.5rem / 13);font-weight:600;white-space:nowrap}
  .status-dot{width:7px;height:7px;border-radius:50%;background:currentColor}
  [data-status=idle] .status-dot{background:transparent;border:1.5px solid currentColor}
  @keyframes agent-spin{to{transform:rotate(360deg)}}
  @media(prefers-reduced-motion:reduce){.spinner{animation:none}}
</style>
