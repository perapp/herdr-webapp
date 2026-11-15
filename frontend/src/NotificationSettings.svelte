<script lang="ts">
  import { onMount } from 'svelte';
  import { disableNotifications, enableNotifications, notificationConfiguration, notificationHealth, notificationsEnabled, notificationUnavailable } from './lib/notifications';
  import { notificationPermissionHelp } from './lib/notificationSupport';
  import { errorMessage } from './lib/api';
  import { connectionRoute } from './lib/connectionRoute';
  let supported = $state(false);
  let busy = $state(true);
  let enabled = $state(false);
  let status = $state('Checking notification support…');
  let error = $state('');
  let disposed = false;
  onMount(() => {
    void (async () => {
      if (connectionRoute().kind !== 'computer') { status = 'Open a paired computer to configure its background notifications.'; busy = false; return; }
      const limitation = notificationUnavailable();
      if (limitation) { status = limitation; busy = false; return; }
      try {
        const config = await notificationConfiguration();
        if (disposed) return;
        supported = config.supported;
        enabled = notificationsEnabled();
        status = supported ? await notificationHealth() : 'Background notifications are unavailable on this backend. Open a paired computer with an up-to-date, running connector.';
      } catch (e) { if (!disposed) error = errorMessage(e); }
      finally { if (!disposed) busy = false; }
    })();
    return () => { disposed = true; };
  });
  async function change(enable: boolean) {
    busy = true; error = '';
    try {
      // Called before awaiting anything: browser permission remains a user gesture.
      await (enable ? enableNotifications() : disableNotifications());
      if (!disposed) { enabled = notificationsEnabled(); status = await notificationHealth(); }
    } catch (e) { if (!disposed) error = errorMessage(e); }
    finally { if (!disposed) busy = false; }
  }
</script>
<fieldset class="notification-options">
  <legend>Agent notifications</legend>
  <p>Opt in separately for each computer. Get private “finished”, “needs input” and “reported an error” alerts from agents that report structured status. Terminal contents and labels are never shown.</p>
  <p role="status">{status}</p>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if supported}
    <button disabled={busy} onclick={() => change(true)}>{enabled ? 'Repair / enable notifications' : 'Enable notifications'}</button>
    <button disabled={busy} onclick={() => change(false)}>Disable and unsubscribe</button>
  {/if}
  <details><summary>Delivery, coverage and permissions</summary>
    <p>Enabled remembered computers keep sending even when another computer is selected or the app is closed, while their connector is running and authorization remains valid. Alerts for the terminal actively viewed in this window are suppressed; other panes are not marked seen. Forgetting locally is not connector-side revocation.</p>
    <p>{notificationPermissionHelp}</p>
  </details>
</fieldset>
<style>
  p { color: var(--muted); font-size: .85rem; line-height: 1.45; }
  button { min-height: 44px; margin: 4px 4px 4px 0; }
  summary { min-height: 44px; padding-top: 10px; cursor: pointer; font-size: .85rem; }
</style>
