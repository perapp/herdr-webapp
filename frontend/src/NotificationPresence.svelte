<script lang="ts">
  import { notificationPresenceReporter } from './lib/notifications';
  let { terminal = '', ready = false }: { terminal?: string; ready?: boolean } = $props();
  const report = notificationPresenceReporter();
  $effect(() => {
    const address = ready ? terminal : '';
    // The ready-agent badge replaces OS alerts anywhere in this visible computer.
    // Visibility, not keyboard focus, also covers open drawers and phone keyboards.
    const send = () => {
      const visible = ready && document.visibilityState === 'visible';
      void report(visible ? address : '', visible).catch(() => {});
    };
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
