<script lang="ts">
  import { notificationPresenceReporter } from './lib/notifications';
  let { terminal = '', ready = false }: { terminal?: string; ready?: boolean } = $props();
  $effect(() => {
    const address = ready ? terminal : '';
    const report = notificationPresenceReporter();
    // Capture address and authenticated computer; never mark unrelated panes as seen.
    const send = () => { void report(document.visibilityState === 'visible' && document.hasFocus() ? address : '').catch(() => {}); };
    send();
    const timer = window.setInterval(send, 10000);
    document.addEventListener('visibilitychange', send);
    window.addEventListener('focus', send); window.addEventListener('blur', send);
    return () => {
      clearInterval(timer); document.removeEventListener('visibilitychange', send);
      window.removeEventListener('focus', send); window.removeEventListener('blur', send);
      void report('').catch(() => {}); // Bound to the old computer; lease also expires on disconnect.
    };
  });
</script>
