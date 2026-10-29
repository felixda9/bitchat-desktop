<script lang="ts">
  let status = "not connected";
</script>

<header>
  <span class="logo">bitchat/</span>
</header>
<main>
  <p class="muted">{status}</p>
</main>

<style>
  header {
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
  }
  .logo {
    font-weight: 700;
    color: var(--accent);
  }
  main {
    padding: 14px;
  }
  .muted {
    color: var(--muted);
  }
</style>
