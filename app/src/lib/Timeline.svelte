<script lang="ts">
  import { api, colorFor, time } from "./api";
  import { app } from "./store.svelte";

  let box: HTMLDivElement;

  $effect(() => {
    app.timeline.length;
    queueMicrotask(() => box && (box.scrollTop = box.scrollHeight));
  });

  function openDm(pubkey: string, own: boolean) {
    if (!own) api.openDm(pubkey);
  }
</script>

<div class="lines" bind:this={box}>
  {#if !app.channel}
    <p class="hint">pick a location channel (click the channel name above) to start chatting.</p>
  {/if}
  {#each app.timeline as line, i (i)}
    {#if line.kind === "system"}
      <p class="system">{line.text}</p>
    {:else if line.message.emote}
      <p class="system">{time(line.message.timestamp)} {line.message.content}</p>
    {:else}
      <p class:own={line.message.own}>
        <span class="time">[{time(line.message.timestamp)}]</span>
        <button class="nick" style:color={colorFor(line.message.pubkey)} onclick={() => openDm(line.message.pubkey, line.message.own)}>
          &lt;{line.message.display}{#if line.message.teleported}↗{/if}&gt;
        </button>
        <span class="text">{line.message.content}</span>
      </p>
    {/if}
  {/each}
</div>

<style>
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
  .time {
    color: var(--muted);
  }
  .nick {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    cursor: pointer;
  }
  .own .text {
    color: var(--fg-strong);
  }
  .system,
  .hint {
    color: var(--muted);
    font-style: italic;
  }
</style>
