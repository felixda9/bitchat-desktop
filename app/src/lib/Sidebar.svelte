<script lang="ts">
  import { api, colorFor, time } from "./api";
  import { app } from "./store.svelte";

  /** iOS rule: nobody seen in a small, precise channel shows as "?" (people there
   *  don't announce themselves, for privacy). */
  function people(geohash: string): string {
    const n = app.counts[geohash];
    if (n === undefined) return "";
    if (n === 0 && geohash.length >= 6) return "[? people]";
    return `[${n} ${n === 1 ? "person" : "people"}]`;
  }

  const meshLabel = $derived.by(() => {
    switch (app.mesh.state) {
      case "on":
        return app.mesh.links ? `bluetooth · ${app.mesh.links} nearby` : "bluetooth · nobody nearby";
      case "off":
        return "bluetooth is off";
      case "unavailable":
        return "no bluetooth adapter";
      default:
        return "bluetooth";
    }
  });
</script>

<aside>
  <h3>channels</h3>
  <ul>
    <li>
      <button class:active={app.channel?.geohash === "mesh"} onclick={() => api.join("mesh", false)}>
        #mesh <span class="muted">{meshLabel}</span>
      </button>
    </li>
    {#each app.regional as c (c.geohash)}
      <li>
        <button class:active={app.channel?.geohash === c.geohash} onclick={() => api.join(c.geohash, false)}>
          #{c.geohash} <span class="muted">{c.level} {people(c.geohash)}</span>
        </button>
      </li>
    {/each}
  </ul>
  {#if !app.regional.length}
    <p class="muted">turn on location for desktop apps in Windows settings to see channels near you, or pick one from the channel menu.</p>
  {/if}

  <h3>people</h3>
  {#if !app.channel}
    <p class="muted">not in a channel</p>
  {:else if app.people.list.filter((p) => !p.me).length === 0}
    <p class="muted">{app.people.unknown ? "nobody seen yet (people here may just be lurking)" : "nobody else here"}</p>
  {/if}
  <ul>
    {#each app.people.list as p (p.pubkey)}
      <li>
        <button disabled={p.me} style:color={colorFor(p.pubkey)} onclick={() => api.openDm(p.pubkey)}>
          {#if p.favorite}★ {/if}{p.display}{#if p.me} (you){/if}{#if p.teleported} ↗{/if}
        </button>
      </li>
    {/each}
  </ul>

  {#if app.favorites.length}
    <h3>favorites</h3>
    <ul>
      {#each app.favorites as f (f.peer)}
        <li>
          <button
            class:offline={!f.online}
            title={f.online ? "nearby on bluetooth" : f.reachable ? "away — messages go over the internet" : "away — they need to favorite you before you can reach them online"}
            onclick={() => api.openDm(f.peer)}
          >
            {f.ours ? "★" : "☆"} <span style:color={colorFor(f.peer)}>{f.display}</span>
            {#if !f.online}<span class="muted">{f.reachable ? "· online" : "· away"}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if app.conversations.length}
    <h3>private chats</h3>
    <ul>
      {#each app.conversations as c (c.peer)}
        <li>
          <button class:active={app.dm?.peer === c.peer} onclick={() => api.openDm(c.peer)}>
            <span style:color={colorFor(c.peer)}>{c.display}</span>
            {#if c.unread}<span class="badge">{c.unread}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if app.settings.has_location && app.notes.length}
    <h3>notes here</h3>
    <ul class="notes">
      {#each app.notes as n (n.id)}
        <li><span class="muted">[{time(n.timestamp)}] {n.display}:</span> {n.content}</li>
      {/each}
    </ul>
  {/if}
</aside>

<style>
  aside {
    width: 240px;
    border-left: 1px solid var(--line);
    padding: 8px 12px;
    overflow-y: auto;
  }
  h3 {
    font-size: 12px;
    font-weight: 400;
    color: var(--muted);
    text-transform: lowercase;
    margin: 12px 0 4px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  button {
    background: none;
    border: none;
    padding: 2px 0;
    font: inherit;
    color: inherit;
    cursor: pointer;
    text-align: left;
    width: 100%;
  }
  button:disabled {
    cursor: default;
  }
  button.active {
    text-decoration: underline;
  }
  button.offline {
    opacity: 0.75;
  }
  .badge {
    background: var(--accent);
    color: var(--bg);
    border-radius: 8px;
    padding: 0 6px;
    margin-left: 6px;
    font-size: 11px;
  }
  .muted {
    color: var(--muted);
  }
  .notes li {
    margin: 4px 0;
    overflow-wrap: anywhere;
  }
  @media (max-width: 700px) {
    aside {
      display: none;
    }
  }
</style>
