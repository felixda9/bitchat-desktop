<script lang="ts">
  import { api, cleanGeohash, levelForLength } from "./api";
  import { app } from "./store.svelte";

  let { onClose }: { onClose: () => void } = $props();
  let raw = $state("");
  let locating = $state(false);
  let locateError = $state("");
  const geohash = $derived(cleanGeohash(raw));

  function join() {
    if (!geohash) return;
    api.join(geohash, true);
    onClose();
  }

  function joinLocal(g: string) {
    api.join(g, false);
    onClose();
  }

  async function locate() {
    locating = true;
    locateError = "";
    try {
      await api.locate();
    } catch (e) {
      locateError = String(e);
    } finally {
      locating = false;
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onClose}>
  <div class="dialog" role="dialog" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === "Escape" && onClose()}>
    <h2>location channels</h2>
    {#if app.regional.length}
      <ul class="local">
        {#each app.regional as c (c.geohash)}
          <li>
            <button class="pick" onclick={() => joinLocal(c.geohash)}>
              #{c.geohash} <span class="muted">{c.level}</span>
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <button class="pick" onclick={locate} disabled={locating}>{locating ? "finding you…" : "use my location"}</button>
      {#if locateError}<p class="warn">{locateError}</p>{/if}
    {/if}
    <p class="muted">
      type a geohash to join that area's channel. joining an area you're not in marks you as teleported (↗) to
      people there.
    </p>
    <form onsubmit={(e) => (e.preventDefault(), join())}>
      <input bind:value={raw} placeholder="#u4pruy" spellcheck="false" autocomplete="off" />
      <button type="submit" disabled={!geohash}>join</button>
    </form>
    {#if raw && !geohash}
      <p class="warn">a geohash is 1–12 characters from 0-9 and b-z (no a, i, l, o)</p>
    {:else if geohash}
      <p class="muted">{levelForLength(geohash.length)} level</p>
    {/if}
    {#if app.channel}
      <button class="leave" onclick={() => (api.leave(), onClose())}>leave #{app.channel.geohash}</button>
    {/if}
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
    width: min(440px, calc(100vw - 32px));
  }
  h2 {
    font-size: 15px;
    margin: 0 0 8px;
  }
  form {
    display: flex;
    gap: 8px;
  }
  input {
    flex: 1;
    background: var(--bg);
    border: 1px solid var(--line);
    color: var(--fg);
    font: inherit;
    padding: 6px 8px;
  }
  button {
    background: var(--accent);
    color: var(--bg);
    border: none;
    font: inherit;
    padding: 6px 12px;
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .local {
    list-style: none;
    padding: 0;
    margin: 0 0 8px;
  }
  .pick {
    background: none;
    color: var(--accent);
    border: 1px solid var(--line);
    width: 100%;
    text-align: left;
    margin: 2px 0;
  }
  .leave {
    margin-top: 12px;
    background: none;
    color: var(--muted);
    border: 1px solid var(--line);
  }
  .muted {
    color: var(--muted);
  }
  .warn {
    color: var(--warn);
  }
</style>
