<script lang="ts">
  // Phase 0 placeholder: proves the UI talks to the Rust core.
  // The real UI is built in Phase 4.
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  type CoreInfo = {
    version: string;
    extension: string;
    format_version: number;
    presets: string[];
  };

  let info = $state<CoreInfo | null>(null);
  let error = $state("");

  onMount(async () => {
    try {
      info = await invoke<CoreInfo>("core_info");
    } catch (e) {
      error = String(e);
    }
  });
</script>

<main>
  <div class="card">
    <div class="logo">N</div>
    <h1>Nanarchive</h1>
    <p class="tag">Compress smaller, beautifully.</p>

    {#if info}
      <div class="status ok">Core connected · v{info.version} · .{info.extension} format v{info.format_version}</div>
      <div class="chips">
        {#each info.presets as p}
          <span class="chip">{p}</span>
        {/each}
      </div>
    {:else if error}
      <div class="status err">{error}</div>
    {:else}
      <div class="status">Connecting…</div>
    {/if}
  </div>
</main>

<style>
  :global(:root) {
    --bg: #f4f1fb;
    --card: #ffffff;
    --text: #2a2540;
    --muted: #7a7496;
    --accent: #8b6cf6;
    --accent-soft: #ece6ff;
    --ok: #2f9e6e;
    --err: #d9485f;
    --shadow: 0 10px 40px rgba(80, 60, 160, 0.12);
    font-family: "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
    color: var(--text);
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #17151f;
      --card: #221f2e;
      --text: #ece9f7;
      --muted: #9a94b5;
      --accent-soft: #2f2946;
      --shadow: 0 10px 40px rgba(0, 0, 0, 0.4);
    }
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
  }
  main {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 16px;
    box-sizing: border-box;
  }
  .card {
    background: var(--card);
    border-radius: 24px;
    box-shadow: var(--shadow);
    padding: 40px 48px;
    text-align: center;
    max-width: 440px;
    width: 100%;
  }
  .logo {
    width: 64px;
    height: 64px;
    margin: 0 auto 16px;
    border-radius: 20px;
    background: linear-gradient(135deg, #a58bff, #7c5cf0);
    color: white;
    font-size: 32px;
    font-weight: 700;
    display: grid;
    place-items: center;
  }
  h1 {
    margin: 0;
    font-size: 28px;
  }
  .tag {
    margin: 6px 0 24px;
    color: var(--muted);
  }
  .status {
    font-size: 14px;
    padding: 10px 14px;
    border-radius: 12px;
    background: var(--accent-soft);
    color: var(--muted);
  }
  .status.ok {
    color: var(--ok);
  }
  .status.err {
    color: var(--err);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: center;
    margin-top: 16px;
  }
  .chip {
    padding: 6px 14px;
    border-radius: 999px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: 13px;
    font-weight: 600;
    text-transform: capitalize;
  }
</style>
