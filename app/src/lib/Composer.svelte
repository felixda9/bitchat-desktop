<script lang="ts">
  import { api } from "./api";
  import { app } from "./store.svelte";
  import { prepareImage } from "./media";

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
  let picker: HTMLInputElement | undefined = $state();

  // Images go over Bluetooth only: #mesh, or a chat with a mesh peer.
  const canSendImage = $derived(
    app.dm ? app.dm.peer.startsWith("mesh:") : app.channel?.geohash === "mesh",
  );

  async function sendImage(file: Blob) {
    try {
      await api.sendImage(await prepareImage(file));
    } catch (e) {
      console.warn("image not sent", e);
    }
  }

  function picked() {
    const file = picker?.files?.[0];
    if (file) sendImage(file);
    if (picker) picker.value = "";
  }

  function paste(e: ClipboardEvent) {
    const item = [...(e.clipboardData?.items ?? [])].find((i) => i.type.startsWith("image/"));
    const file = item?.getAsFile();
    if (file && canSendImage) {
      e.preventDefault();
      sendImage(file);
    }
  }

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
      onpaste={paste}
      placeholder={app.channel ? "type a message or /help" : "pick a location channel first"}
      spellcheck="false"
      autocomplete="off"
    />
    {#if canSendImage}
      <button class="attach" title="send an image" onclick={() => picker?.click()}>📷</button>
      <input bind:this={picker} type="file" accept="image/*" hidden onchange={picked} />
    {/if}
    {#if app.dm && !text.startsWith("/")}
      <span class="count" class:over={tooLong}>{bytes}/255</span>
    {:else if !app.dm && app.channel?.geohash === "mesh" && app.settings.bridge_enabled}
      <button
        class="scope"
        class:nearby={app.settings.nearby_only}
        onclick={() => api.setNearbyOnly(!app.settings.nearby_only)}
        title={app.settings.nearby_only
          ? "nearby only — this message won't cross the bridge"
          : "bridged — reaches people beyond bluetooth range in this area"}
      >
        {app.settings.nearby_only ? "📡 nearby only" : "🌐 bridged"}
      </button>
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
  .attach {
    background: none;
    border: none;
    font: inherit;
    cursor: pointer;
    padding: 0 2px;
    opacity: 0.8;
  }
  .attach:hover {
    opacity: 1;
  }
  .scope {
    background: none;
    border: 1px solid var(--line);
    color: var(--accent);
    font: inherit;
    font-size: 12px;
    padding: 2px 8px;
    cursor: pointer;
    white-space: nowrap;
  }
  .scope.nearby {
    color: var(--muted);
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
