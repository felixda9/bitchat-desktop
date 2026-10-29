<script lang="ts">
  import { api, colorFor, time } from "./api";
  import { app } from "./store.svelte";
</script>

<aside>
  <h3>channels</h3>
  <ul>
    <li><span class="muted" title="the bluetooth mesh chat comes in a later version">#mesh · needs bluetooth</span></li>
    {#each app.regional as c (c.geohash)}
      <li>
        <button class:active={app.channel?.geohash === c.geohash} onclick={() => api.join(c.geohash, false)}>
          #{c.geohash} <span class="muted">{c.level}</span>
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
          {p.display}{#if p.me} (you){/if}{#if p.teleported} ↗{/if}
        </button>
      </li>
    {/each}
  </ul>

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
