<script lang="ts">
  let { onConfirm, onCancel }: { onConfirm: () => void; onCancel: () => void } = $props();
</script>

<div class="backdrop" role="presentation" onclick={onCancel}>
  <div class="dialog" role="alertdialog" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === "Escape" && onCancel()}>
    <h2>wipe all data?</h2>
    <p>messages, keys, blocks and settings are deleted. you come back as a new person.</p>
    <div class="row">
      <button class="danger" onclick={onConfirm}>wipe everything</button>
      <button onclick={onCancel}>cancel</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.6);
    display: grid;
    place-items: center;
  }
  .dialog {
    background: var(--panel);
    border: 1px solid var(--warn);
    padding: 16px 20px;
    width: min(400px, calc(100vw - 32px));
  }
  h2 {
    font-size: 15px;
    margin: 0 0 8px;
    color: var(--warn);
  }
  .row {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  button {
    background: none;
    border: 1px solid var(--line);
    color: var(--fg);
    font: inherit;
    padding: 6px 12px;
    cursor: pointer;
  }
  .danger {
    background: var(--warn);
    border-color: var(--warn);
    color: var(--bg);
  }
</style>
