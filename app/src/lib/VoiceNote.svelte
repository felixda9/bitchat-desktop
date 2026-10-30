<script lang="ts" module>
  // One voice message plays at a time.
  let current: HTMLAudioElement | null = null;
  const waveforms = new Map<string, Promise<{ bins: number[]; duration: number }>>();

  /** As iOS `WaveformCache`: 120 bins of mean |sample|, scaled to the loudest. */
  function waveform(url: string) {
    let w = waveforms.get(url);
    if (!w) {
      w = fetch(url)
        .then((r) => r.arrayBuffer())
        .then(async (data) => {
          const ctx = new AudioContext();
          try {
            const audio = await ctx.decodeAudioData(data);
            const bins = 120;
            const length = audio.length;
            const perBin = Math.max(1, Math.floor(length / bins));
            const channels = [...Array(audio.numberOfChannels)].map((_, i) => audio.getChannelData(i));
            const out = new Array(bins).fill(0);
            for (let b = 0; b < bins; b++) {
              const start = b * perBin;
              const end = Math.min(length, start + perBin);
              if (start >= end) break;
              let sum = 0;
              for (let f = start; f < end; f++) {
                let v = 0;
                for (const c of channels) v += Math.abs(c[f]);
                sum += v / channels.length;
              }
              out[b] = sum / (end - start);
            }
            const max = Math.max(...out);
            return { bins: max > 0 ? out.map((m) => Math.min(m / max, 1)) : out, duration: audio.duration };
          } finally {
            ctx.close();
          }
        });
      waveforms.set(url, w);
    }
    return w;
  }
</script>

<script lang="ts">
  import { onDestroy } from "svelte";

  let { url }: { url: string } = $props();

  let audio: HTMLAudioElement | undefined = $state();
  let bins = $state<number[]>(new Array(64).fill(0.25));
  let duration = $state(NaN);
  let time = $state(0);
  let playing = $state(false);
  let frame = 0;

  $effect(() => {
    waveform(url)
      .then((w) => {
        bins = w.bins;
        if (!Number.isFinite(duration)) duration = w.duration;
      })
      .catch(() => {});
  });

  function follow() {
    if (audio) time = audio.currentTime;
    if (playing) frame = requestAnimationFrame(follow);
  }

  function toggle() {
    if (!audio) return;
    if (audio.paused) {
      if (current && current !== audio) current.pause();
      current = audio;
      audio.play();
    } else {
      audio.pause();
    }
  }

  function seek(e: MouseEvent) {
    if (!audio || !Number.isFinite(duration)) return;
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
    audio.currentTime = Math.max(0, Math.min(1, (e.clientX - box.left) / box.width)) * duration;
    time = audio.currentTime;
  }

  const progress = $derived(Number.isFinite(duration) && duration > 0 ? time / duration : 0);
  const label = $derived.by(() => {
    if (!Number.isFinite(duration)) return "--:--";
    const s = Math.round(playing ? Math.max(0, duration - time) : duration);
    return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
  });

  onDestroy(() => {
    cancelAnimationFrame(frame);
    audio?.pause();
  });
</script>

<span class="note">
  <button class="play" onclick={toggle} aria-label={playing ? "pause" : "play"}>
    {#if playing}
      <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="3.5" y="3" width="3" height="10" rx="0.8" /><rect x="9.5" y="3" width="3" height="10" rx="0.8" /></svg>
    {:else}
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3.2v9.6c0 .5.5.8.9.5l7.3-4.8c.4-.3.4-.8 0-1.1L5.9 2.7c-.4-.3-.9 0-.9.5z" /></svg>
    {/if}
  </button>
  <button class="wave" onclick={seek} aria-label="seek">
    <svg viewBox="0 0 {bins.length} 48" preserveAspectRatio="none" aria-hidden="true">
      {#each bins as b, i (i)}
        {@const h = Math.max(1, b * 48)}
        <rect x={i} y={(48 - h) / 2} width="0.7" height={h} class:played={i / bins.length <= progress && progress > 0} />
      {/each}
    </svg>
  </button>
  <span class="time">{label}</span>
  <audio
    bind:this={audio}
    src={url}
    preload="metadata"
    onloadedmetadata={() => audio && Number.isFinite(audio.duration) && (duration = audio.duration)}
    onplay={() => ((playing = true), follow())}
    onpause={() => (playing = false)}
    onended={() => ((playing = false), (time = 0))}
  ></audio>
</span>

<style>
  .note {
    display: inline-flex;
    align-items: center;
    gap: 12px;
    margin: 4px 0;
    padding: 12px;
    border-radius: 14px;
    background: color-mix(in srgb, var(--bg) 60%, var(--panel));
    border: 1px solid color-mix(in srgb, var(--accent) 30%, transparent);
    box-shadow: 0 2px 6px rgb(0 0 0 / 0.3);
  }
  .play {
    flex: none;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: none;
    background: var(--accent);
    display: grid;
    place-items: center;
    cursor: pointer;
    padding: 0;
  }
  .play svg {
    width: 16px;
    height: 16px;
    fill: #fff;
  }
  .wave {
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    width: 220px;
    height: 40px;
  }
  .wave svg {
    width: 100%;
    height: 100%;
    display: block;
  }
  rect {
    fill: color-mix(in srgb, var(--muted) 35%, transparent);
  }
  rect.played {
    fill: var(--accent);
  }
  .time {
    color: var(--muted);
    font-size: 13px;
    min-width: 5ch;
  }
  audio {
    display: none;
  }
</style>
