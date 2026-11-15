<script lang="ts">
  import { request } from './lib/api';
  import { agentSeenReporter } from './lib/agentSeen';
  import type { TerminalInfo } from './lib/schema';
  let { terminals, visible, ready }: { terminals: TerminalInfo[]; visible: string[]; ready: boolean } = $props();
  const report = agentSeenReporter((terminal, turn) => request('/api/rpc', {
    method: 'terminal.label', params: { terminal, labels: { 'herdr.seen': String(turn) } },
  }));
  $effect(() => {
    const records = terminals, addresses = ready ? visible : [];
    const send = () => report(records, addresses, document.visibilityState === 'visible' && document.hasFocus());
    send();
    document.addEventListener('visibilitychange', send);
    window.addEventListener('focus', send); window.addEventListener('blur', send);
    return () => {
      document.removeEventListener('visibilitychange', send);
      window.removeEventListener('focus', send); window.removeEventListener('blur', send);
    };
  });
</script>
