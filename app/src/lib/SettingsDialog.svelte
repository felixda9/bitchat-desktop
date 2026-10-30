<script lang="ts">
  import { api } from "./api";
  import { app } from "./store.svelte";

  let { onClose, onWipe }: { onClose: () => void; onWipe: () => void } = $props();
  let nickname = $state(app.nickname);
  let relay = $state("");

  function saveNick() {
    if (nickname.trim() && nickname.trim() !== app.nickname) api.setNickname(nickname);
  }

  function setBridge(on: boolean) {
    api.setBridge(on);
    // Turning it on is the moment to look for a location fix (as iOS).
    if (on && !app.settings.has_location) api.locate().catch(() => {});
  }

  function addRelay() {
    if (relay.trim()) api.addRelay(relay.trim());
    relay = "";
  }
</script>

<div class="backdrop" role="presentation" onclick={onClose}>
  <div class="dialog" role="dialog" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === "Escape" && onClose()}>
    <h2>settings</h2>

    <label for="nick">nickname</label>
    <form onsubmit={(e) => (e.preventDefault(), saveNick())}>
      <input id="nick" bind:value={nickname} onblur={saveNick} spellcheck="false" />
    </form>

    <label for="npub">your nostr key</label>
    <input id="npub" readonly value={app.npub} />

    <h3>network</h3>
    <label class="check">
      <input type="checkbox" checked={app.settings.tor_enabled} onchange={(e) => api.setTor(e.currentTarget.checked)} />
      route through tor
    </label>
    <p class="muted">{app.tor.summary}</p>
    {#if !app.settings.tor_enabled}
      <p class="warn">tor is off: every relay, including the ones carrying your private messages, sees your IP address.</p>
    {/if}

    <label class="check">
      <input type="checkbox" checked={app.settings.bridge_enabled} onchange={(e) => setBridge(e.currentTarget.checked)} />
      mesh bridge
    </label>
    <p class="muted">
      joins nearby mesh islands over the internet: what you say in #mesh also reaches people in your area beyond
      bluetooth range, and their messages show up marked with 🌐. while you have internet, this computer also carries
      bridge and location-channel traffic for phones around you that have none.
    </p>
    {#if app.settings.bridge_enabled}
      <p class="muted">
        {app.settings.bridge_cell
          ? `rendezvous cell: ${app.settings.bridge_cell}`
          : "no rendezvous cell yet — needs location access or a nearby bridge peer"}
      </p>
    {/if}

    <label for="relay">extra relays for private messages</label>
    <ul>
      {#each app.settings.custom_relays as r (r)}
        <li>{r} <button class="link" onclick={() => api.removeRelay(r)}>remove</button></li>
      {/each}
    </ul>
    <form onsubmit={(e) => (e.preventDefault(), addRelay())}>
      <input id="relay" bind:value={relay} placeholder="wss://relay.example.com" spellcheck="false" />
      <button type="submit">add</button>
    </form>

    {#if !app.settings.key_in_keychain}
      <p class="warn">no system keychain found: your keys are protected by file permissions only.</p>
    {/if}

    <h3>danger</h3>
    <button class="danger" onclick={onWipe}>wipe everything</button>
    <p class="muted">erases all messages, keys and identity. triple-clicking the bitchat/ logo does the same.</p>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.5);
    display: grid;
    place-items: center;
  }
  .dialog {
    background: var(--panel);
    border: 1px solid var(--line);
    padding: 16px 20px;
    width: min(480px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow-y: auto;
  }
  h2 {
    font-size: 15px;
    margin: 0 0 12px;
  }
  h3 {
    font-size: 13px;
    color: var(--muted);
    font-weight: 400;
    margin: 18px 0 6px;
  }
  label {
    display: block;
    color: var(--muted);
    margin: 10px 0 4px;
  }
  .check {
    color: var(--fg);
  }
  form {
    display: flex;
    gap: 8px;
  }
  input:not([type="checkbox"]) {
    flex: 1;
    width: 100%;
    box-sizing: border-box;
    background: var(--bg);
    border: 1px solid var(--line);
    color: var(--fg);
    font: inherit;
    padding: 6px 8px;
  }
  ul {
    margin: 0 0 6px;
    padding-left: 16px;
  }
  button {
    background: var(--accent);
    color: var(--bg);
    border: none;
    font: inherit;
    padding: 6px 12px;
    cursor: pointer;
  }
  .link {
    background: none;
    color: var(--muted);
    padding: 0 4px;
    text-decoration: underline;
  }
  .danger {
    background: var(--warn);
  }
  .muted {
    color: var(--muted);
    font-size: 12px;
  }
  .warn {
    color: var(--warn);
    font-size: 12px;
  }
</style>
