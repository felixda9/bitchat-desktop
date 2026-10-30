<script lang="ts">
  import { api, colorFor, time, type DmStatus } from "./api";
  import { app } from "./store.svelte";
  import Media from "./Media.svelte";

  let box: HTMLDivElement | undefined = $state();

  $effect(() => {
    app.dm?.lines.length;
    queueMicrotask(() => box && (box.scrollTop = box.scrollHeight));
  });

  function tick(status: DmStatus | null): string {
    switch (status) {
      case "sent":
        return "✓";
      case "delivered":
        return "✓✓";
      case "read":
        return "✓✓";
      case "failed":
        return "!";
      default:
        return "";
    }
  }
</script>

{#if app.dm}
  <div class="bar">
    <span style:color={colorFor(app.dm.peer)}>#{app.conversations.find((c) => c.peer === app.dm?.peer)?.geohash || app.channel?.geohash || ""}/@{app.dm.display}</span>
    <span class="spacer"></span>
    {#if app.dm.peer.startsWith("mesh:")}
      {@const fav = app.favorites.find((f) => f.peer === app.dm?.peer)?.ours ?? false}
      <button title={fav ? "remove from favorites" : "add to favorites"} onclick={() => app.dm && api.favorite(app.dm.peer, !fav)}>{fav ? "★" : "☆"}</button>
    {/if}
    <button onclick={() => api.openDm(null)}>close</button>
  </div>
  <div class="lines" bind:this={box}>
    {#each app.dm.lines as line, i (i)}
      {#if line.kind === "system"}
        <p class="system">{line.text}</p>
      {:else}
        <p class:mine={line.message.from_me}>
          <span class="time">[{time(line.message.timestamp)}]</span>
          <span class="who">{line.message.from_me ? `<${app.nickname}>` : `<${app.dm.display}>`}</span>
          {#if !line.message.media}<span>{line.message.content}</span>{/if}
          {#if line.message.from_me}
            <span class="status {line.message.status}" title={line.message.status ?? ""}>{tick(line.message.status)}</span>
          {/if}
          {#if line.message.media}
            <span class="media"><Media media={line.message.media} /></span>
          {/if}
        </p>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .bar {
    display: flex;
    padding: 6px 12px;
    border-bottom: 1px solid var(--line);
  }
  .spacer {
    flex: 1;
  }
  button {
    background: none;
    border: 1px solid var(--line);
    color: var(--muted);
    font: inherit;
    cursor: pointer;
  }
  .media {
    display: block;
    padding-left: 2ch;
  }
  .lines {
    flex: 1;
    overflow-y: auto;
    padding: 8px 12px;
  }
  p {
    margin: 2px 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .time,
  .who {
    color: var(--muted);
  }
  .mine .who {
    color: var(--accent);
  }
  .status {
    color: var(--muted);
    margin-left: 6px;
  }
  .status.read {
    color: var(--accent);
  }
  .system {
    color: var(--muted);
    font-style: italic;
  }
</style>
