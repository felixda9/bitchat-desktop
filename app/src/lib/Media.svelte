<script lang="ts">
  import type { MediaRef } from "./api";
  import { mediaUrl } from "./media";
  import VoiceNote from "./VoiceNote.svelte";

  let { media }: { media: MediaRef } = $props();
  let url = $state<string | null>(null);
  let failed = $state(false);
  let big = $state(false);

  $effect(() => {
    mediaUrl(media)
      .then((u) => (url = u))
      .catch(() => (failed = true));
  });
</script>

{#if failed}
  <span class="muted">[{media.kind === "audio" ? "voice message" : media.kind} no longer available]</span>
{:else if !url}
  <span class="muted">…</span>
{:else if media.kind === "image"}
  <button class="image" class:big onclick={() => (big = !big)} title={big ? "click to shrink" : "click to enlarge"}>
    <img src={url} alt="" />
  </button>
{:else if media.kind === "audio"}
  <VoiceNote {url} />
{:else}
  <a href={url} download={media.name ?? "file"}>📎 {media.name ?? "file"}</a>
{/if}

<style>
  .image {
    display: block;
    background: none;
    border: none;
    padding: 0;
    margin: 4px 0;
    cursor: zoom-in;
  }
  img {
    display: block;
    max-width: min(320px, 100%);
    max-height: 320px;
    border-radius: 4px;
  }
  .big {
    cursor: zoom-out;
  }
  .big img {
    max-width: 100%;
    max-height: 70vh;
  }
  .muted {
    color: var(--muted);
  }
  a {
    color: var(--accent);
  }
</style>
