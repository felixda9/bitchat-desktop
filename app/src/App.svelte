<script lang="ts">
  import { onMount } from "svelte";
  import { api, onEvent } from "./lib/api";
  import { handle, app } from "./lib/store.svelte";
  import Header from "./lib/Header.svelte";
  import Timeline from "./lib/Timeline.svelte";
  import DmView from "./lib/DmView.svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import Composer from "./lib/Composer.svelte";
  import ChannelDialog from "./lib/ChannelDialog.svelte";
  import SettingsDialog from "./lib/SettingsDialog.svelte";
  import ConfirmWipe from "./lib/ConfirmWipe.svelte";

  let showChannel = $state(false);
  let showSettings = $state(false);
  let confirmWipe = $state(false);

  onMount(() => {
    // Ask for the full view once listening, so start-up events aren't missed.
    const unlisten = onEvent(handle).then((stop) => {
      api.refresh();
      // Like the phones: find the local channels straight away if allowed.
      api.locate().catch(() => {});
      return stop;
    });
    return () => {
      unlisten.then((f) => f());
    };
  });

  function wipe() {
    confirmWipe = false;
    showSettings = false;
    api.wipe();
  }
</script>

<div class="app">
  <Header onChannel={() => (showChannel = true)} onSettings={() => (showSettings = true)} onWipe={() => (confirmWipe = true)} />
  <div class="body">
    <main>
      {#if app.dm}
        <DmView />
      {:else}
        <Timeline />
      {/if}
      <Composer />
    </main>
    <Sidebar />
  </div>
</div>

{#if showChannel}
  <ChannelDialog onClose={() => (showChannel = false)} />
{/if}
{#if showSettings}
  <SettingsDialog onClose={() => (showSettings = false)} onWipe={() => (confirmWipe = true)} />
{/if}
{#if confirmWipe}
  <ConfirmWipe onConfirm={wipe} onCancel={() => (confirmWipe = false)} />
{/if}

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  main {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
</style>
