<script lang="ts">
  import { app } from "./store.svelte";

  let { onChannel, onSettings, onWipe }: { onChannel: () => void; onSettings: () => void; onWipe: () => void } =
    $props();

  // Triple-click the logo to wipe (after a confirmation), as on the phones.
  let clicks: number[] = [];
  function logoClick() {
    const now = Date.now();
    clicks = [...clicks.filter((t) => now - t < 700), now];
    if (clicks.length >= 3) {
      clicks = [];
      onWipe();
    }
  }

  const peopleLabel = $derived(app.people.unknown ? "? people" : `${app.people.count} ${app.people.count === 1 ? "person" : "people"}`);

  const torLabel = $derived.by(() => {
    switch (app.tor.state) {
      case "ready":
        return "tor";
      case "off":
        return "tor off";
      case "stalled":
        return "tor stalled";
      case "error":
        return "tor error";
      default:
        return `tor ${Math.round(app.tor.progress * 100)}%`;
    }
  });
</script>

<header>
  <button class="logo" onclick={logoClick} title="triple-click to wipe everything">bitchat/</button>
  <button class="channel" onclick={onChannel}>
    {#if app.channel}
      #{app.channel.geohash}
      {#if app.channel.teleported}<span class="tele" title="teleported">↗</span>{/if}
    {:else}
      pick a location channel
    {/if}
  </button>
  {#if app.channel}
    <span class="people">[{peopleLabel}]</span>
  {/if}
  <span class="spacer"></span>
  <span class="tor {app.tor.state}" title={app.tor.summary}>{torLabel}</span>
  <button class="gear" onclick={onSettings} title="settings">⚙</button>
</header>

<style>
  header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
    white-space: nowrap;
  }
  button {
    background: none;
    border: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
    padding: 2px 4px;
  }
  .logo {
    color: var(--accent);
    font-weight: 700;
    user-select: none;
  }
  .channel {
    color: var(--accent);
  }
  .channel:hover,
  .gear:hover {
    text-decoration: underline;
  }
  .tele {
    color: var(--muted);
  }
  .people {
    color: var(--muted);
  }
  .spacer {
    flex: 1;
  }
  .tor {
    color: var(--muted);
    font-size: 12px;
  }
  .tor.ready {
    color: var(--accent);
  }
  .tor.stalled,
  .tor.error {
    color: var(--warn);
  }
</style>
