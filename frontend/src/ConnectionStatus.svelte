<script lang="ts">
  let { name, status, error, retry, connections, forget }: { name: string; status: string; error?: string; retry: () => void; connections: () => void; forget?: () => void } = $props();
</script>

<!-- Connection availability is not navigation: never show onboarding on peer loss. -->
<div class="connection-workspace" data-testid="connection-workspace">
  <header><button class="quiet" aria-label="Connections" onclick={connections}>☰</button><span>{name}</span></header>
  <main aria-label="Terminal connection">
    <section aria-labelledby="connection-heading">
      <span class="prompt" aria-hidden="true">❯</span>
      <h1 id="connection-heading">{status === 'Your terminals, wherever you are.' ? 'Opening your terminals…' : status}</h1>
      <p role="status">{error || 'Your spaces and terminals will appear here. Input resumes when connected.'}</p>
      <div class="actions"><button onclick={retry}>Retry connection</button><button class="quiet" onclick={connections}>Connections</button></div>
      {#if forget}<details><summary>Connection help</summary><p>Keep the connector running on this computer. If access was revoked, scan its fresh QR code or open its new private link.</p><button class="quiet" onclick={forget}>Forget this computer</button></details>{/if}
    </section>
  </main>
</div>

<style>
  .connection-workspace { height: 100dvh; display: flex; flex-direction: column; background: var(--base); color: var(--ink); }
  header { min-height: 40px; display: flex; align-items: center; gap: 8px; padding: env(safe-area-inset-top, 0px) 8px 0; border-bottom: 1px solid var(--line); font-size: .923077rem; }
  header button { min-width: 44px; min-height: 44px; padding: 0; }
  header span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  main { flex: 1; min-height: 0; overflow: auto; display: grid; place-items: center; padding: 24px; }
  section { width: min(100%, 360px); }
  .prompt { font: 1.5rem ui-monospace, SFMono-Regular, monospace; color: var(--teal); }
  h1 { margin: 12px 0 8px; font-size: 1.230769rem; font-weight: 550; letter-spacing: -.02em; }
  p { color: var(--muted); font-size: .923077rem; line-height: 1.6; overflow-wrap: anywhere; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 20px; }
  button, summary { min-height: 44px; }
  details { margin-top: 20px; color: var(--muted); font-size: .846154rem; }
  summary { padding: 12px 0; cursor: pointer; }
</style>
