<script lang="ts">
  import { api } from "./api";
  import { app } from "./store.svelte";

  // Commands that work in location channels (iOS CommandInfo, geohash context).
  const COMMANDS = [
    { name: "/block", hint: "<nickname>", about: "block or list blocked people" },
    { name: "/unblock", hint: "<nickname>", about: "unblock someone" },
    { name: "/clear", hint: "", about: "clear this chat" },
    { name: "/drop", hint: "<note>", about: "pin a note here for 24h (needs location)" },
    { name: "/fav", hint: "<nickname>", about: "favorite a mesh peer (keep chatting online when apart)" },
    { name: "/help", hint: "", about: "list commands" },
    { name: "/hug", hint: "<nickname>", about: "send a hug" },
    { name: "/msg", hint: "<nickname> [message]", about: "start a private chat" },
    { name: "/slap", hint: "<nickname>", about: "slap with a large trout" },
    { name: "/unfav", hint: "<nickname>", about: "remove a favorite" },
    { name: "/who", hint: "", about: "list who's here" },
  ];

  let text = $state("");
  let input: HTMLInputElement;

  const suggestions = $derived(
    text.startsWith("/") && !text.includes(" ") ? COMMANDS.filter((c) => c.name.startsWith(text)) : [],
  );

  const bytes = $derived(new TextEncoder().encode(text).length);
  const tooLong = $derived(app.dm !== null && !text.startsWith("/") && bytes > 255);

  function send() {
    const t = text.trim();
    if (!t || tooLong) return;
    api.submit(t);
    text = "";
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      send();
    } else if (e.key === "Tab" && suggestions.length) {
      e.preventDefault();
      text = suggestions[0].name + " ";
    }
  }

  $effect(() => {
    app.dm?.peer;
    input?.focus();
  });
</script>

<div class="composer">
  {#if suggestions.length}
    <ul class="suggest">
      {#each suggestions as c (c.name)}
        <li>
          <button onclick={() => ((text = c.name + " "), input.focus())}>
            <b>{c.name}</b> <span class="muted">{c.hint} — {c.about}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
  <div class="row">
    <span class="prompt">{app.dm ? `@${app.dm.display}` : app.channel ? `#${app.channel.geohash}` : ""}&gt;</span>
    <input
      bind:this={input}
      bind:value={text}
      onkeydown={keydown}
      placeholder={app.channel ? "type a message or /help" : "pick a location channel first"}
      spellcheck="false"
      autocomplete="off"
    />
    {#if app.dm && !text.startsWith("/")}
      <span class="count" class:over={tooLong}>{bytes}/255</span>
    {/if}
  </div>
</div>

<style>
  .composer {
    border-top: 1px solid var(--line);
    position: relative;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
  }
  .prompt {
    color: var(--accent);
    white-space: nowrap;
  }
  input {
    flex: 1;
    background: transparent;
    border: none;
    outline: none;
    color: var(--fg);
    font: inherit;
  }
  .count {
    color: var(--muted);
    font-size: 12px;
  }
  .count.over {
    color: var(--warn);
  }
  .suggest {
    position: absolute;
    bottom: 100%;
    left: 0;
    right: 0;
    margin: 0;
    padding: 4px 0;
    list-style: none;
    background: var(--panel);
    border-top: 1px solid var(--line);
  }
  .suggest button {
    background: none;
    border: none;
    color: var(--fg);
    font: inherit;
    padding: 2px 12px;
    cursor: pointer;
    width: 100%;
    text-align: left;
  }
  .suggest button:hover {
    background: var(--line);
  }
  .muted {
    color: var(--muted);
  }
</style>
