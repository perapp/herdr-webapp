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
      if (connectionRoute().kind !== 'computer') { status = 'Open a paired computer to set up its notifications.'; busy = false; return; }
      const limitation = notificationUnavailable();
      if (limitation) { status = limitation; busy = false; return; }
      try {
        const config = await notificationConfiguration();
        if (disposed) return;
        supported = config.supported;
        enabled = notificationsEnabled();
        status = supported ? await notificationHealth() : 'Notifications need an up-to-date, running connector on a paired computer.';
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
<div class="notification-options">
  <div class="notification-content">
    <div class="notification-title"><h4>Agent activity</h4>{#if supported}<span class:enabled>{busy ? 'Updating…' : enabled ? 'On' : 'Off'}</span>{/if}</div>
    <p class="description">Know when an agent finishes, needs input or reports an error.</p>
    <p class="notification-status" role="status">{status}</p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if supported}
      <div class="notification-actions">
        {#if enabled}
          <button class="quiet" disabled={busy} onclick={() => change(true)}>Repair notifications</button>
          <button disabled={busy} onclick={() => change(false)}>Turn off</button>
        {:else}
          <button disabled={busy} onclick={() => change(true)}>Enable notifications</button>
        {/if}
      </div>
    {/if}
  </div>
  <details><summary>Privacy & delivery</summary>
    <div class="notification-details">
      <p>Opt in for each computer. Alerts include only structured agent status, never terminal contents or labels.</p>
      <p>Enabled computers can send alerts while the app is closed or another computer is selected, as long as their connector is running and authorization is valid.</p>
      <p>Alerts for this computer are silenced while its page is visible; use the ready-agent badge instead. Other panes are not marked seen. Forgetting a computer here does not revoke its connector’s authorization.</p>
      <p>{notificationPermissionHelp}</p>
    </div>
  </details>
</div>
<style>
  .notification-options { min-width: 0; border: 1px solid var(--line); border-radius: 12px; background: var(--panel); overflow: hidden; }
  .notification-content { padding: 16px; }
  .notification-title { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  h4 { margin: 0; color: var(--bright); font-size: 1rem; font-weight: 550; }
  .notification-title span { color: var(--muted); font: .846154rem ui-monospace, SFMono-Regular, monospace; }
  .notification-title span.enabled { color: var(--teal); }
  p { margin: 8px 0 0; color: var(--muted); font-size: .846154rem; line-height: 1.5; overflow-wrap: anywhere; }
  .description { margin-top: 5px; }
  .notification-status { margin-top: 12px; padding-left: 10px; border-left: 2px solid var(--line); }
  p.error { color: var(--error); }
  .notification-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; }
  button { min-height: 44px; border-radius: 8px; font-size: .923077rem; background: var(--base); }
  button.quiet { background: transparent; }
  details { border-top: 1px solid var(--line); }
  summary { min-height: 44px; padding: 13px 16px; cursor: pointer; font-size: .846154rem; color: var(--muted); }
  summary:hover { background: var(--raised); color: var(--bright); }
  .notification-details { padding: 0 16px 16px; }
  .notification-details p:first-child { margin-top: 0; }
</style>
